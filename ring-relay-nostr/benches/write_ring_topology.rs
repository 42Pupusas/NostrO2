//! Microbench: producer-side cost of the storage write-ring topology.
//!
//! Compares two layouts for N publisher shards feeding one storage thread:
//!
//! 1. **N × SPSC, round-robin drained.** Each producer owns its own ring;
//!    the consumer iterates rings popping items. Producer-side: no atomics
//!    on the hot path. Consumer-side: pays a stride per ring per drain
//!    iteration plus cache traffic from N independent head/tail pairs.
//!
//! 2. **One shared MPSC.** All producers push into one ring via
//!    fetch-and-add on the tail; consumer drains a single head. Producer
//!    side pays FAA. Consumer side: one cache line.
//!
//! No parsing, no verification, no disk — just the ring. This lets us
//! measure the contention/topology cost independent of downstream
//! bottlenecks. Throughput is reported in items/s aggregated across all
//! producers.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer as MpscRing;
use quetzalcoatl::spsc::RingBuffer as SpscRing;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Shape-matched to `storage::handle::WriteReq` so allocation profile and
/// per-item size mirror the real load. We don't import the real type to
/// keep this bench self-contained and runnable against any topology.
///
/// Fields look unread because nothing reads them — the bench only cares
/// about the per-item size, the `Arc::clone` cost on push, and the cache
/// footprint on pop. `black_box(item)` keeps the optimizer honest.
#[allow(dead_code)]
#[derive(Clone)]
struct Item {
    raw_json: Arc<[u8]>,
    event_id: [u8; 32],
    pubkey: [u8; 32],
    kind: u32,
}

impl Item {
    fn new() -> Self {
        // ~256 byte payload — typical kind-1 event. The Arc::clone on push
        // is the same cost the real path pays.
        Self {
            raw_json: Arc::from(vec![0u8; 256].into_boxed_slice()),
            event_id: [0xab; 32],
            pubkey: [0xcd; 32],
            kind: 1,
        }
    }
}

/// Per-shard ring capacity — kept consistent with `StorageConfig::default`.
const PER_SHARD_CAP: usize = 4096;

/// Items each producer pushes per iteration. Tuned so the bench finishes in
/// reasonable time per shard count while still amortizing thread spawn cost.
const ITEMS_PER_PRODUCER: usize = 50_000;

/// Drive N SPSC rings round-robin from one consumer thread; N producers each
/// push `ITEMS_PER_PRODUCER` items. Returns the wall-clock duration.
fn run_spsc(num_producers: usize) -> Duration {
    let popped = Arc::new(AtomicUsize::new(0));

    let mut producers = Vec::with_capacity(num_producers);
    let mut consumers = Vec::with_capacity(num_producers);
    for _ in 0..num_producers {
        let (tx, rx) = SpscRing::<Item>::new(Capacity::at_least(PER_SHARD_CAP)).split();
        producers.push(tx);
        consumers.push(rx);
    }

    let target = num_producers * ITEMS_PER_PRODUCER;
    let popped_consumer = Arc::clone(&popped);
    let consumer_thread = std::thread::spawn(move || {
        let mut local: usize = 0;
        let mut rxs = consumers;
        while local < target {
            let mut drained_any = false;
            for rx in rxs.iter_mut() {
                while let Some(item) = rx.pop() {
                    drained_any = true;
                    std::hint::black_box(item);
                    local += 1;
                }
            }
            if !drained_any {
                std::hint::spin_loop();
            }
        }
        popped_consumer.store(local, Ordering::Release);
    });

    let start = Instant::now();
    let mut handles = Vec::with_capacity(num_producers);
    for tx in producers {
        let h = std::thread::spawn(move || {
            let item = Item::new();
            for _ in 0..ITEMS_PER_PRODUCER {
                let mut to_push = item.clone();
                loop {
                    match tx.push(to_push) {
                        Ok(()) => break,
                        Err(returned) => {
                            to_push = returned;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });
        handles.push(h);
    }
    for h in handles {
        h.join().unwrap();
    }
    consumer_thread.join().unwrap();
    let elapsed = start.elapsed();
    assert_eq!(popped.load(Ordering::Acquire), target);
    elapsed
}

/// Same as `run_spsc` but with a single shared MPSC ring sized to
/// `PER_SHARD_CAP * num_producers` so the absolute buffering matches.
fn run_mpsc(num_producers: usize) -> Duration {
    let total_cap = PER_SHARD_CAP.saturating_mul(num_producers);
    let (tx_seed, mut rx) = MpscRing::<Item>::new(Capacity::at_least(total_cap)).split();

    let popped = Arc::new(AtomicUsize::new(0));
    let target = num_producers * ITEMS_PER_PRODUCER;

    let popped_consumer = Arc::clone(&popped);
    let consumer_thread = std::thread::spawn(move || {
        let mut local: usize = 0;
        while local < target {
            // `drain_up_to` amortizes the consumer's head-pointer update
            // across the batch — one Release store per drain call instead
            // of one per item. This is exactly what the storage thread
            // does in production after the MPSC swap.
            let drained = rx.drain_up_to(1024, |item| {
                std::hint::black_box(item);
                local += 1;
            });
            if drained == 0 {
                std::hint::spin_loop();
            }
        }
        popped_consumer.store(local, Ordering::Release);
    });

    let start = Instant::now();
    let mut handles = Vec::with_capacity(num_producers);
    for _ in 0..num_producers {
        let tx = tx_seed.clone();
        let h = std::thread::spawn(move || {
            let item = Item::new();
            for _ in 0..ITEMS_PER_PRODUCER {
                let mut to_push = item.clone();
                loop {
                    match tx.push(to_push) {
                        Ok(()) => break,
                        Err(returned) => {
                            to_push = returned;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });
        handles.push(h);
    }
    drop(tx_seed);
    for h in handles {
        h.join().unwrap();
    }
    consumer_thread.join().unwrap();
    let elapsed = start.elapsed();
    assert_eq!(popped.load(Ordering::Acquire), target);
    elapsed
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_ring_topology");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for &num_producers in &[1usize, 2, 4, 8, 16] {
        let total_items = (num_producers * ITEMS_PER_PRODUCER) as u64;
        group.throughput(Throughput::Elements(total_items));

        group.bench_with_input(
            BenchmarkId::new("spsc_round_robin", num_producers),
            &num_producers,
            |b, &n| b.iter_custom(|iters| (0..iters).map(|_| run_spsc(n)).sum()),
        );

        group.bench_with_input(
            BenchmarkId::new("shared_mpsc", num_producers),
            &num_producers,
            |b, &n| b.iter_custom(|iters| (0..iters).map(|_| run_mpsc(n)).sum()),
        );
    }

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
