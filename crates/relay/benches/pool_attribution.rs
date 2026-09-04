//! What attributing a pooled message costs.
//!
//! [`PoolEvent::Message`] carries the relay that served it, so the
//! forwarder pairs every message with its address. A [`RelayUrl`] owns
//! three `String`s, so carrying it by value would clone three heap
//! allocations per message on the pool's hot path; the pool therefore
//! shares one `Arc` per relay instead.
//!
//! These arms measure both, and measure them against the work the same
//! message already costs elsewhere, so the number is read in proportion
//! rather than on its own. The `clone RelayUrl` arm is the rejected
//! alternative, kept so the choice stays justified rather than asserted.
//!
//! [`PoolEvent::Message`]: nostro2_relay::PoolEvent::Message
//! [`RelayUrl`]: nostro2_relay::RelayUrl

fn main() {
    divan::main();
}

/// How many messages one iteration carries.
const COUNT: usize = 500;

/// The inputs every arm shares.
struct Fixture;

impl Fixture {
    fn url() -> nostro2_relay::RelayUrl {
        nostro2_relay::RelayUrl::parse("wss://relay.damus.io/v1?compat=1").unwrap()
    }

    fn shared_url() -> std::sync::Arc<nostro2_relay::RelayUrl> {
        std::sync::Arc::new(Self::url())
    }

    fn event() -> nostro2::NostrRelayEvent {
        nostro2::NostrRelayEvent::NewNote(
            nostro2::RelayEventTag::Event,
            "subscription".to_string(),
            nostro2::NostrNote {
                kind: 1,
                content: "a benchmark note of an ordinary size".to_string(),
                id: Some("a".repeat(64)),
                ..Default::default()
            },
        )
    }
}

/// Cloning the URL per message: the rejected alternative.
#[divan::bench(name = "attribution: clone RelayUrl (3 Strings)", sample_count = 50)]
fn clone_url(bencher: divan::Bencher) {
    let url = Fixture::url();
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(url.clone());
        }
    });
}

/// Sharing the URL per message: what the forwarder does, one atomic bump
/// and no allocation.
#[divan::bench(name = "attribution: clone Arc<RelayUrl>", sample_count = 50)]
fn clone_arc(bencher: divan::Bencher) {
    let url = Fixture::shared_url();
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(std::sync::Arc::clone(&url));
        }
    });
}

/// Building the whole event the forwarder pushes, attribution included.
#[divan::bench(name = "attribution: build PoolEvent::Message", sample_count = 50)]
fn build_event(bencher: divan::Bencher) {
    let url = Fixture::shared_url();
    let event = Fixture::event();
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(nostro2_relay::PoolEvent::Message(
                std::sync::Arc::clone(&url),
                Box::new(event.clone()),
            ));
        }
    });
}

/// The same event without the attribution, for the difference.
#[divan::bench(name = "attribution: build the message alone", sample_count = 50)]
fn build_message_only(bencher: divan::Bencher) {
    let event = Fixture::event();
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(Box::new(event.clone()));
        }
    });
}

/// The box the driver already allocates for every message, unchanged by
/// attribution. This is the allocation that was there in 0.7.0 too.
#[divan::bench(name = "baseline: Box::new(event) as the driver does", sample_count = 50)]
fn box_event(bencher: divan::Bencher) {
    let event = Fixture::event();
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(Box::new(event.clone()));
        }
    });
}

/// Parsing one relay frame: the work every message already costs on the
/// driver thread, before it ever reaches the pool.
#[divan::bench(name = "baseline: parse one relay frame", sample_count = 50)]
fn parse_frame(bencher: divan::Bencher) {
    let frame = format!(
        "[\"EVENT\",\"subscription\",{{\"id\":\"{}\",\"pubkey\":\"{}\",\"created_at\":1700000000,\"kind\":1,\"tags\":[],\"content\":\"a benchmark note of an ordinary size\",\"sig\":\"{}\"}}]",
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(128)
    );
    bencher.bench_local(|| {
        for _ in 0..COUNT {
            divan::black_box(
                divan::black_box(&frame).parse::<nostro2::NostrRelayEvent>().ok(),
            );
        }
    });
}
