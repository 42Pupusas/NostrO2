use divan::black_box;
use nostro2_cache::Cache;

fn main() {
    divan::main();
}

fn generate_event_id(n: usize) -> String {
    format!("{n:064x}")
}

#[divan::bench]
fn single_thread_insert(bencher: divan::Bencher) {
    let cache = Cache::new(10_000);
    let counter = std::sync::atomic::AtomicUsize::new(0);
    bencher.bench(|| {
        for _ in 0..1000 {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let id = generate_event_id(n);
            black_box(cache.insert(id));
        }
    });
}

const THREAD_COUNTS: &[usize] = &[2, 4, 8, 10, 20];

#[divan::bench(args = THREAD_COUNTS)]
fn multi_thread_insert(bencher: divan::Bencher, threads: usize) {
    bencher.bench(|| {
        let cache = Cache::new(10_000);
        let handles: Vec<_> = (0..threads)
            .map(|thread_id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for i in 0..1000 {
                        let id = generate_event_id(thread_id * 1000 + i);
                        black_box(cache.insert(id));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
}

#[divan::bench]
fn realistic_relay_pattern(bencher: divan::Bencher) {
    let num_threads = 10_usize;
    let events_per_thread = 1000_usize;
    bencher.bench(|| {
        let cache = Cache::new(10_000);
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for i in 0..events_per_thread {
                        let id_num = if i % 5 == 0 && i > 0 {
                            thread_id * events_per_thread + i - 1
                        } else {
                            thread_id * events_per_thread + i
                        };
                        let id = generate_event_id(id_num);
                        black_box(cache.insert(id));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
}
