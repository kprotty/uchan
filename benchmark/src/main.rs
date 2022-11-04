use std::{
    fmt,
    thread,
    time::{Instant, Duration},
    sync::{Arc, Barrier},
    sync::atomic::{AtomicBool, Ordering},
};

mod queues;
use queues::{Channel, ChannelSender, ChannelReceiver, apply_to_all};

pub fn main() {
    let num_cpus = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    bench_concurrent("spsc", 2);

    if num_cpus > 3 {
        bench_concurrent("micro_contention", 3);
    }

    if num_cpus > 5 {
        bench_concurrent("traditional contention", 5);
    }

    bench_concurrent("high_contention", num_cpus);

    bench_concurrent("over_subscribed", num_cpus * 2);

    {
        let core_ids = core_affinity::get_core_ids().unwrap();
        let context  = Arc::new((core_ids, AtomicBool::new(false)));

        let busy_threads = (0..num_cpus)
            .map(|i| {
                let context = context.clone();
                thread::spawn(move || {
                    let core_id = context.0[i % context.0.len()];
                    core_affinity::set_for_current(core_id);
                    
                    while !context.1.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect::<Vec<_>>();

        bench_concurrent("busy_system", num_cpus);

        context.1.store(true, Ordering::Relaxed);
        for thread in busy_threads.into_iter() {
            thread.join().unwrap();
        }
    }
}

fn bench_concurrent(bench_name: &str, num_cpus: usize) {
    println!("{:?} (producers:{}, consumers:1) \n{}\n{:?}",
        bench_name,
        num_cpus - 1,
        "-".repeat(82),
        ConcurrentResult::default(),
    );

    apply_to_all!(bench_concurrent_queue, num_cpus);
    println!();
}

fn bench_concurrent_queue<C: Channel>(num_cpus: usize) {
    let context = Arc::new((Barrier::new(num_cpus), AtomicBool::new(false)));
    let (tx, rx) = C::create();

    let recv_thread = thread::spawn({
        let context = context.clone();
        move || {
            let mut received = 0u64;
            context.0.wait();

            while !context.1.load(Ordering::Relaxed) {
                let _ = rx.recv();
                received += 1;
            }

            received
        }
    });

    let send_threads: Vec<_> = (0..(num_cpus - 1))
        .map(|mut i| {
            let tx = tx.clone();
            let context = context.clone();

            thread::spawn(move || {
                let mut send_times = Vec::with_capacity(1024);
                context.0.wait();

                loop {
                    let started = Instant::now();
                    if !tx.send(i) {
                        return send_times;
                    }

                    let elapsed: u64 = started.elapsed().as_nanos().try_into().unwrap();
                    send_times.push(elapsed);
                    i += 1;
                }
            })
        })
        .collect();

    thread::sleep(Duration::from_secs(1));
    context.1.store(true, Ordering::Relaxed);

    let received = recv_thread.join().unwrap() as f64;
    let results: Vec<_> = send_threads.into_iter().map(|t| t.join().unwrap()).collect();

    let sent = results.iter().fold(0f64, |sum, times| sum + (times.len() as f64));

    let mean = sent / (results.len() as f64);
    let min = results.iter().fold(f64::MAX, |min, times| min.min(times.len() as f64));
    let max = results.iter().fold(0f64, |max, times| max.max(times.len() as f64));

    let mut stdev = results.iter().fold(0f64, |stdev, times| {
        let r = (times.len() as f64) - mean;
        stdev + (r * r)
    });

    if results.len() > 1 {
        stdev /= (results.len() - 1) as f64;
        stdev = stdev.sqrt();
    }

    let mut send_times: Vec<u64> = results.into_iter().flatten().collect();
    send_times.sort();

    let latencies: Vec<_> = [50f64, 99f64]
        .into_iter()
        .map(|percentile| {
            let p = percentile / 100f64;
            let i = (p * (send_times.len() as f64)).round();
            let v = send_times.len().min(i as usize);
            Duration::from_nanos(send_times[v - 1])
        })
        .collect();

    println!("{:?}", ConcurrentResult {
        name: Some(C::name()),
        send_total: Some(sent),
        recv_total: Some(received),
        send_min: Some(min),
        send_max: Some(max),
        send_stdev: Some(stdev),
        send_p50: Some(latencies[0]),
        send_p99: Some(latencies[1]),
    });
}

#[derive(Default)]
struct ConcurrentResult {
    name: Option<&'static str>,
    send_total: Option<f64>,
    recv_total: Option<f64>,
    send_min: Option<f64>,
    send_max: Option<f64>,
    send_stdev: Option<f64>,
    send_p50: Option<Duration>,
    send_p99: Option<Duration>,
}

impl fmt::Debug for ConcurrentResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let humanize_lat = |elapsed: Duration| format!("{:?}", elapsed);
        let humanize = |value: f64| if value < 1_000f64 {
            format!("{}", value.round())
        } else if value < 1_000_000f64 {
            format!("{}k", (value / 1_000f64).round())
        } else if value < 1_000_000_000f64 {
            format!("{:.2}m", value / 1_000_000f64)
        } else {
            format!("{:.2}b", value / 1_000_000_000f64)
        };

        write!(f, "{:<10} |", self.name.unwrap_or("name"))?;
        write!(f, " {:>7} |", self.recv_total.map(humanize).unwrap_or("recv".to_string()))?;
        write!(f, " {:>7} |", self.send_total.map(humanize).unwrap_or("sent".to_string()))?;
        write!(f, " {:>7} |", self.send_stdev.map(humanize).unwrap_or("stdev".to_string()))?;
        write!(f, " {:>7} |", self.send_min.map(humanize).unwrap_or("min".to_string()))?;
        write!(f, " {:>7} |", self.send_max.map(humanize).unwrap_or("max".to_string()))?;
        write!(f, " {:>7} |", self.send_p50.map(humanize_lat).unwrap_or("<50%".to_string()))?;
        write!(f, " {:>7} |", self.send_p99.map(humanize_lat).unwrap_or("<99%".to_string()))?;

        Ok(())
    }
}