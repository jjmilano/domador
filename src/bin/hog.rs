use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: f64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(20.0);
    let threads: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(2);
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut workers = Vec::new();
    for _ in 0..threads {
        workers.push(std::thread::spawn(move || {
            let mut x: u64 = 0;
            while Instant::now() < deadline {
                for i in 0..1_000_000u64 {
                    x = x.wrapping_add(i);
                }
            }
            std::hint::black_box(x);
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
}
