//! Where the driver's transport time goes.
//!
//! The `steady state (no verify)` arm of `pipeline` costs ~7.9us per
//! message with the signature check off. That number is a sum, and a sum
//! is not actionable: it could be the socket read, the frame decode, the
//! JSON parse, or the ring handoff.
//!
//! These arms cut it apart, each measuring one stage on the same 500
//! notes, so the next optimization is aimed at evidence rather than at a
//! guess.

fn main() {
    divan::main();
}

/// How many messages one iteration carries.
const COUNT: usize = 500;

/// The pre-signed frames every arm consumes.
struct Frames;

impl Frames {
    fn build() -> Vec<String> {
        use nostro2::{NostrKeypair as _, NostrSigner as _};
        let keypair = nostro2_signer::NostrKeypair::generate();
        (0..COUNT)
            .map(|i| {
                let mut note = nostro2::NostrNote {
                    kind: 1,
                    content: format!("benchmark note {i}"),
                    pubkey: keypair.public_key(),
                    created_at: 1_700_000_000 + i64::try_from(i).unwrap(),
                    ..Default::default()
                };
                note.sign_with(&keypair).unwrap();
                let body = Self::encode(&note);
                format!("[\"EVENT\",\"bench\",{body}]")
            })
            .collect()
    }

    #[cfg(feature = "bourne")]
    fn encode(note: &nostro2::NostrNote) -> String {
        json_bourne::to_string(note).unwrap()
    }

    #[cfg(feature = "serde")]
    fn encode(note: &nostro2::NostrNote) -> String {
        serde_json::to_string(note).unwrap()
    }
}

/// Parsing the frame into a relay event: the JSON stage.
#[divan::bench(name = "stage: parse frame -> NostrRelayEvent", sample_count = 30)]
fn parse(bencher: divan::Bencher) {
    let frames = Frames::build();
    bencher.bench_local(|| {
        for frame in &frames {
            divan::black_box(frame.parse::<nostro2::NostrRelayEvent>().ok());
        }
    });
}

/// Copying the frame out of the decode buffer, as `take_decoded` does
/// with `text.to_owned()` before the parse ever sees it.
#[divan::bench(name = "stage: copy frame out of decode buffer", sample_count = 30)]
fn copy_out(bencher: divan::Bencher) {
    let frames = Frames::build();
    bencher.bench_local(|| {
        for frame in &frames {
            divan::black_box(frame.as_str().to_owned());
        }
    });
}

/// Decoding a burst of coalesced frames out of one buffer.
///
/// A relay answering a REQ writes every note at once, so a single read
/// hands the decoder many whole frames. `FrameDecoder::drain_consumed`
/// fast-paths `clear()` only when the buffer was fully consumed; with
/// several frames still buffered it takes `Vec::drain(..consumed)`
/// instead, which memmoves the remainder down once per frame.
///
/// The `one frame per read` arm is the same bytes delivered so that the
/// fast path always applies. The difference between the two arms is the
/// memmove.
struct Burst;

impl Burst {
    fn frames() -> Vec<Vec<u8>> {
        Frames::build()
            .iter()
            .map(|frame| {
                let mut encoded = Vec::new();
                coyoquil::Frame::Text(frame)
                    .encode_masked(coyoquil::MaskKey::new(), &mut encoded);
                encoded
            })
            .collect()
    }

    fn coalesced() -> Vec<u8> {
        Self::frames().concat()
    }
}

/// Every frame pushed as one block, then decoded: the real burst.
#[divan::bench(name = "decode: 500 frames coalesced in one buffer", sample_count = 30)]
fn decode_coalesced(bencher: divan::Bencher) {
    let bytes = Burst::coalesced();
    bencher.bench_local(|| {
        let mut decoder: coyoquil::FrameDecoder =
            coyoquil::FrameDecoder::new(coyoquil::Role::Client);
        decoder.push(&bytes).unwrap();
        let mut seen = 0_usize;
        while let Ok(Some(frame)) = decoder.next_frame() {
            divan::black_box(&frame);
            seen += 1;
        }
        divan::black_box(seen)
    });
}

/// The same frames, each pushed and decoded alone.
#[divan::bench(name = "decode: 500 frames one at a time", sample_count = 30)]
fn decode_one_at_a_time(bencher: divan::Bencher) {
    let frames = Burst::frames();
    bencher.bench_local(|| {
        let mut decoder: coyoquil::FrameDecoder =
            coyoquil::FrameDecoder::new(coyoquil::Role::Client);
        let mut seen = 0_usize;
        for frame in &frames {
            decoder.push(frame).unwrap();
            while let Ok(Some(decoded)) = decoder.next_frame() {
                divan::black_box(&decoded);
                seen += 1;
            }
        }
        divan::black_box(seen)
    });
}

/// Drives one producer thread and drains it with `strategy`.
///
/// The payload is a `Box<NostrRelayEvent>`-sized value so the handoff
/// moves what the driver actually moves.
struct Handoff;

impl Handoff {
    fn run(strategy: impl Fn(&mut quetzalcoatl::spmc::Consumer<Box<u128>>) -> usize) -> usize {
        let (producer, mut consumer) = quetzalcoatl::spmc::RingBuffer::<Box<u128>>::new(
            quetzalcoatl::capacity::Capacity::at_least(1024),
        )
        .split();
        let writer = std::thread::spawn(move || {
            for i in 0..COUNT as u128 {
                let mut item = Box::new(i);
                while let Err(returned) = producer.push(item) {
                    item = returned;
                    std::hint::spin_loop();
                }
            }
        });
        let seen = strategy(&mut consumer);
        writer.join().unwrap();
        seen
    }
}

/// One `pop_block` per message: what the pool's forwarder does today.
///
/// Every `pop` calls `wake_producer`, which is a `SeqCst` load per
/// message even when nobody is parked.
#[divan::bench(name = "ring: pop_block per message", sample_count = 30)]
fn ring_pop_block(bencher: divan::Bencher) {
    bencher.bench_local(|| {
        divan::black_box(Handoff::run(|consumer| {
            let mut seen = 0_usize;
            while seen < COUNT {
                if consumer.pop_block().is_some() {
                    seen += 1;
                } else {
                    break;
                }
            }
            seen
        }))
    });
}

/// Batched draining: one producer wake per batch, not per message.
#[divan::bench(name = "ring: drain batches", sample_count = 30)]
fn ring_drain(bencher: divan::Bencher) {
    bencher.bench_local(|| {
        divan::black_box(Handoff::run(|consumer| {
            let mut seen = 0_usize;
            while seen < COUNT {
                seen += consumer.drain(|item| {
                    divan::black_box(item);
                });
                if seen < COUNT {
                    match consumer.pop_block() {
                        Some(item) => {
                            divan::black_box(item);
                            seen += 1;
                        }
                        None => break,
                    }
                }
            }
            seen
        }))
    });
}

/// Zero-copy: read the slot in place, never moving the value out.
#[divan::bench(name = "ring: pop_ref_block (zero copy)", sample_count = 30)]
fn ring_pop_ref(bencher: divan::Bencher) {
    bencher.bench_local(|| {
        divan::black_box(Handoff::run(|consumer| {
            let mut seen = 0_usize;
            while seen < COUNT {
                match consumer.pop_ref_block() {
                    Some(reader) => {
                        divan::black_box(**reader);
                        seen += 1;
                    }
                    None => break,
                }
            }
            seen
        }))
    });
}
