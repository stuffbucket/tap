//! Measures checkpoints and seek on a long synthetic recording.
//!
//! ```sh
//! mise x rust@1.99.0 zig@0.17.1 -- cargo run --release -p tap-index --example checkpoint_bench
//! mise x rust@1.99.0 zig@0.17.1 -- cargo run --release -p tap-index --example checkpoint_bench -- --minutes 30 --mb 10
//! ```
//!
//! It builds the session described in `tests/support/long_session.rs`
//! (60 minutes and 20 MB of output unless told otherwise), indexes it and
//! seeks to random times, and prints the build time, the number and size of
//! the checkpoints, the peak resident set size and the seek latency.
//!
//! Peak RSS comes from `/proc/self/status` on Linux. Elsewhere the benchmark
//! runs itself again under `/usr/bin/time -l` and reads it from there.

use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use tap_index::SessionIndex;

#[path = "../tests/support/long_session.rs"]
#[allow(dead_code)]
mod long_session;

const CHILD: &str = "TAP_CHECKPOINT_BENCH_CHILD";

struct Args {
    minutes: u64,
    mb: usize,
    seed: u64,
    seeks: usize,
}

fn args() -> Args {
    let mut args = Args {
        minutes: 60,
        mb: 20,
        seed: 0x7a9_2026,
        seeks: 400,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_default();
        match flag.as_str() {
            "--minutes" => args.minutes = value.parse().expect("--minutes N"),
            "--mb" => args.mb = value.parse().expect("--mb N"),
            "--seed" => args.seed = value.parse().expect("--seed N"),
            "--seeks" => args.seeks = value.parse().expect("--seeks N"),
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1e3)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn peak_rss_linux() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Runs this benchmark again under `/usr/bin/time -l` and reports the peak
/// RSS it prints (in bytes on macOS).
fn rerun_under_time() -> Option<ExitCode> {
    if std::env::var_os(CHILD).is_some() || !std::path::Path::new("/usr/bin/time").exists() {
        return None;
    }
    let me = std::env::current_exe().ok()?;
    let output = Command::new("/usr/bin/time")
        .arg("-l")
        .arg(me)
        .args(std::env::args().skip(1))
        .env(CHILD, "1")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?
        .wait_with_output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let peak = stderr
        .lines()
        .find(|l| l.contains("maximum resident set size"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok());
    match peak {
        Some(bytes) => println!("peak RSS:            {}", mib(bytes)),
        None => eprint!("{stderr}"),
    }
    Some(if output.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    if !cfg!(target_os = "linux")
        && let Some(code) = rerun_under_time()
    {
        return code;
    }
    let args = args();

    let started = Instant::now();
    let session = long_session::long_session(args.minutes, args.mb * 1_000_000, args.seed);
    let generated = started.elapsed();
    let pane = session.manifest.panes[0].id.clone();
    let events = session.pane_events(&pane).len();

    let started = Instant::now();
    let index = SessionIndex::build(&session).expect("index");
    let build = started.elapsed();
    let stream_len = index.panes[0].stream_len;

    let checkpoints: Vec<_> = index.checkpoints().pane(&pane).collect();
    let logical: u64 = checkpoints.iter().map(|c| c.snapshot.len() as u64).sum();
    let largest = checkpoints
        .iter()
        .map(|c| c.snapshot.len() as u64)
        .max()
        .unwrap_or(0);
    let widest_gap = checkpoints
        .windows(2)
        .map(|w| w[1].key.byte - w[0].key.byte)
        .chain(checkpoints.last().map(|c| stream_len - c.key.byte))
        .max()
        .unwrap_or(stream_len);

    let mut rng = long_session::Rng(args.seed ^ 0x5eec);
    let duration = session.manifest.duration_us;
    let mut latencies: Vec<Duration> = (0..args.seeks)
        .map(|_| {
            let at = rng.below(duration + 1);
            let started = Instant::now();
            let term = index.seek(&pane, at).expect("seek");
            let took = started.elapsed();
            drop(term);
            took
        })
        .collect();
    latencies.sort_unstable();
    let pct = |p: f64| latencies[((latencies.len() - 1) as f64 * p).round() as usize];

    println!(
        "session:             {:.1} min, {} of output, {events} events (generated in {})",
        duration as f64 / 60e6,
        mib(stream_len),
        ms(generated)
    );
    println!("index build:         {}", ms(build));
    println!("checkpoints:         {}", checkpoints.len());
    println!(
        "checkpoint bytes:    {} (largest {})",
        mib(logical),
        mib(largest)
    );
    println!("widest gap:          {} of output", mib(widest_gap));
    println!(
        "seek ({} random):   p50 {}, p99 {}, max {}",
        latencies.len(),
        ms(pct(0.5)),
        ms(pct(0.99)),
        ms(pct(1.0))
    );
    if cfg!(target_os = "linux") {
        match peak_rss_linux() {
            Some(bytes) => println!("peak RSS:            {}", mib(bytes)),
            None => println!("peak RSS:            unavailable"),
        }
    }
    ExitCode::SUCCESS
}
