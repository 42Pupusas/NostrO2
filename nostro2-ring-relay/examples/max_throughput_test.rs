use nostro2::NostrRelayEvent;
use nostro2_ring_relay::{PoolMessage, create_pool};
use quetzalcoatl::capacity::Capacity;
use std::time::Instant;

fn generate_event(id: usize) -> NostrRelayEvent {
    let note = nostro2::NostrNote {
        id: Some(format!("{:064x}", id)),
        pubkey: "test".to_string(),
        created_at: 1234567890,
        kind: 1,
        tags: vec![].into(),
        content: format!("Event {}", id),
        sig: Some("sig".to_string()),
    };
    NostrRelayEvent::NewNote(nostro2::RelayEventTag::Event, "sub".to_string(), note)
}

fn make_msg(thread_id: usize, i: usize, events_per_producer: usize) -> PoolMessage {
    PoolMessage::RelayEvent {
        relay_url: format!("test_{}", thread_id).into(),
        event: generate_event(thread_id * events_per_producer + i),
    }
}

fn test_ring_relay(producers: usize, total_events: usize) -> f64 {
    let (mut consumer, producer) = create_pool(16384, 10_000);
    let events_per_producer = total_events / producers;

    let handles: Vec<_> = (0..producers)
        .map(|thread_id| {
            let prod = producer.clone();
            std::thread::spawn(move || {
                for i in 0..events_per_producer {
                    let msg = make_msg(thread_id, i, events_per_producer);
                    while prod.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            })
        })
        .collect();

    let start = Instant::now();
    let mut received = 0;
    while received < total_events {
        if let Some(PoolMessage::RelayEvent { .. }) = consumer.try_recv() {
            received += 1;
        }
    }
    let elapsed = start.elapsed();

    for handle in handles {
        handle.join().unwrap();
    }

    total_events as f64 / elapsed.as_secs_f64()
}

fn test_spsc_ring_relay(total_events: usize) -> f64 {
    let (producer, mut consumer) =
        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384)).split();
    let dedup = nostro2_cache::Cache::new(10_000);

    std::thread::spawn(move || {
        for i in 0..total_events {
            let msg = make_msg(0, i, total_events);
            while producer.push(msg.clone()).is_err() {
                std::hint::spin_loop();
            }
        }
    });

    let start = Instant::now();
    let mut received = 0;
    while received < total_events {
        if let Some(msg) = consumer.pop() {
            if let PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            } = msg
            {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
    }
    let elapsed = start.elapsed();

    total_events as f64 / elapsed.as_secs_f64()
}

fn test_spsc_ring_relay_zero_copy(total_events: usize) -> f64 {
    let (producer, mut consumer) =
        quetzalcoatl::spsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384)).split();
    let dedup = nostro2_cache::Cache::new(10_000);

    std::thread::spawn(move || {
        for i in 0..total_events {
            let msg = make_msg(0, i, total_events);
            while producer.push(msg.clone()).is_err() {
                std::hint::spin_loop();
            }
        }
    });

    let start = Instant::now();
    let mut received = 0;
    while received < total_events {
        if let Some(reader) = consumer.pop_ref() {
            if let PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            } = *reader
            {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
    }
    let elapsed = start.elapsed();

    total_events as f64 / elapsed.as_secs_f64()
}

fn test_ring_relay_zero_copy(producers: usize, total_events: usize) -> f64 {
    let (producer, mut consumer) =
        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384)).split();
    let dedup = nostro2_cache::Cache::new(10_000);
    let events_per_producer = total_events / producers;

    let handles: Vec<_> = (0..producers)
        .map(|thread_id| {
            let prod = producer.clone();
            std::thread::spawn(move || {
                for i in 0..events_per_producer {
                    let msg = make_msg(thread_id, i, events_per_producer);
                    while prod.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            })
        })
        .collect();

    let start = Instant::now();
    let mut received = 0;
    while received < total_events {
        if let Some(reader) = consumer.pop_ref() {
            if let PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            } = *reader
            {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
    }
    let elapsed = start.elapsed();

    for handle in handles {
        handle.join().unwrap();
    }

    total_events as f64 / elapsed.as_secs_f64()
}

/// Busy-wait to simulate inter-message work (e.g. network read, parsing).
/// ~1-2ns per iteration on modern CPUs.
#[inline(never)]
fn simulate_work(iterations: u32) {
    for i in 0..iterations {
        std::hint::black_box(i);
    }
}

fn test_ring_relay_paced(producers: usize, total_events: usize, work: u32) -> f64 {
    let (producer, mut consumer) =
        quetzalcoatl::mpsc::RingBuffer::<PoolMessage>::new(Capacity::at_least(16384)).split();
    let dedup = nostro2_cache::Cache::new(10_000);
    let events_per_producer = total_events / producers;

    let handles: Vec<_> = (0..producers)
        .map(|thread_id| {
            let prod = producer.clone();
            std::thread::spawn(move || {
                for i in 0..events_per_producer {
                    simulate_work(work);
                    let msg = make_msg(thread_id, i, events_per_producer);
                    while prod.push(msg.clone()).is_err() {
                        std::hint::spin_loop();
                    }
                }
            })
        })
        .collect();

    let start = Instant::now();
    let mut received = 0;
    while received < total_events {
        if let Some(reader) = consumer.pop_ref() {
            if let PoolMessage::RelayEvent {
                event: NostrRelayEvent::NewNote(_, _, ref note),
                ..
            } = *reader
            {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
    }
    let elapsed = start.elapsed();

    for handle in handles {
        handle.join().unwrap();
    }

    total_events as f64 / elapsed.as_secs_f64()
}

fn test_async_relay_paced(senders: usize, total_events: usize, work: u32) -> f64 {
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<NostrRelayEvent>();
        let events_per_sender = total_events / senders;

        for thread_id in 0..senders {
            let tx = tx.clone();
            tokio::spawn(async move {
                for i in 0..events_per_sender {
                    simulate_work(work);
                    let event = generate_event(thread_id * events_per_sender + i);
                    tx.send(event).unwrap();
                }
            });
        }
        drop(tx);

        let dedup = nostro2_cache::Cache::new(10_000);
        let start = Instant::now();
        let mut received = 0;

        while received < total_events {
            if let Some(NostrRelayEvent::NewNote(_, _, note)) = rx.recv().await {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
        let elapsed = start.elapsed();
        total_events as f64 / elapsed.as_secs_f64()
    })
}

fn test_async_relay(senders: usize, total_events: usize) -> f64 {
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<NostrRelayEvent>();
        let events_per_sender = total_events / senders;

        for thread_id in 0..senders {
            let tx = tx.clone();
            tokio::spawn(async move {
                for i in 0..events_per_sender {
                    let event = generate_event(thread_id * events_per_sender + i);
                    tx.send(event).unwrap();
                }
            });
        }
        drop(tx);

        let dedup = nostro2_cache::Cache::new(10_000);
        let start = Instant::now();
        let mut received = 0;

        while received < total_events {
            if let Some(NostrRelayEvent::NewNote(_, _, note)) = rx.recv().await {
                if let Some(ref id) = note.id {
                    if dedup.insert(id.clone()) {
                        received += 1;
                    }
                } else {
                    received += 1;
                }
            }
        }
        let elapsed = start.elapsed();
        total_events as f64 / elapsed.as_secs_f64()
    })
}

fn main() {
    println!("=== Maximum Throughput Comparison ===\n");
    println!("Testing 100K events with varying concurrency...\n");

    // SPSC vs MPSC(1) comparison
    println!("--- Single-Producer Ring Buffer Comparison ---");
    println!("Strategy         | Throughput (ev/s)");
    println!("-----------------|------------------");

    let mpsc_1 = test_ring_relay(1, 100_000);
    println!("MPSC(1) pop      | {:>16.0}", mpsc_1);

    let spsc_pop = test_spsc_ring_relay(100_000);
    println!("SPSC pop         | {:>16.0}", spsc_pop);

    let spsc_pop_ref = test_spsc_ring_relay_zero_copy(100_000);
    println!("SPSC pop_ref     | {:>16.0}", spsc_pop_ref);

    println!("\nSPSC vs MPSC(1): {:.1}x", spsc_pop / mpsc_1);
    println!("SPSC pop_ref vs pop: {:.1}x\n", spsc_pop_ref / spsc_pop);

    // Full comparison table
    println!("--- Multi-Producer Comparison ---");
    println!(
        "Concurrency | MPSC pop (ev/s) | MPSC pop_ref (ev/s) | Async Relay (ev/s) | pop_ref gain"
    );
    println!(
        "------------|-----------------|---------------------|--------------------|--------------"
    );

    for concurrency in [1, 5, 10, 20] {
        let ring_rate = test_ring_relay(concurrency, 100_000);
        let ring_zc_rate = test_ring_relay_zero_copy(concurrency, 100_000);
        let async_rate = test_async_relay(concurrency, 100_000);

        println!(
            "{:>11} | {:>15.0} | {:>19.0} | {:>18.0} | {:>11.1}x",
            concurrency,
            ring_rate,
            ring_zc_rate,
            async_rate,
            ring_zc_rate / ring_rate
        );
    }

    println!("\nAbove tests pure synchronization overhead without network delays");

    // Paced producer comparison - simulating real-world inter-message delays
    // ~1-2ns per work iteration, so:
    //   100  iters ≈ 100-200ns  (extremely fast local relay)
    //   1000 iters ≈ 1-2us      (fast relay, just parsing overhead)
    //   5000 iters ≈ 5-10us     (typical network jitter between messages)
    //  10000 iters ≈ 10-20us    (realistic relay with network latency)
    println!("\n--- Paced Producer Comparison (10 producers, 100K events) ---");
    println!("Work/msg | Ring MPSC (ev/s) | Async Relay (ev/s) | Ring vs Async");
    println!("---------|------------------|--------------------|--------------");

    for work in [0, 100, 1000, 5000, 10000] {
        let ring_rate = test_ring_relay_paced(10, 100_000, work);
        let async_rate = test_async_relay_paced(10, 100_000, work);

        let comparison = if ring_rate > async_rate {
            format!("Ring {:.1}x", ring_rate / async_rate)
        } else {
            format!("Async {:.1}x", async_rate / ring_rate)
        };

        println!(
            "{:>8} | {:>16.0} | {:>18.0} | {}",
            work, ring_rate, async_rate, comparison
        );
    }

    println!("\nAs producer work increases (simulating network delay),");
    println!("the ring buffer contention drops and throughput characteristics change.");
}
