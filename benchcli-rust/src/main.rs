//! benchcli-rust: the HTTP/3 benchmark client, in Rust on cloudflare/quiche.
//!
//! It runs three benchmarks one after another on the same QUIC connections,
//! as the Go benchmarks' client does on its TCP ones, and writes one JSON
//! report per benchmark to output/report, which benchreport turns into the
//! Markdown tables:
//!
//!   Connections     dials -c connections, -dc at a time, each a QUIC
//!                   handshake and one GET /echo answered on it
//!   BenchEcho       -en round trips, a POST /echo of -b random bytes read
//!                   back, one request in flight per connection and -ec
//!                   connections busy at once
//!   BenchMultiplex  for -rd seconds, -rr requests a second on every
//!                   connection, opened -rb streams at a time without waiting
//!                   for the ones before: HTTP/3's answer to pipelining

#[macro_use]
extern crate benchkit;

mod config;
mod engine;
mod ps;
mod report;
mod stats;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use engine::{Command, EchoJob, Engine, EngineConfig, Limiter, Outcome, RateJob};
use benchkit::flags;
use benchkit::logging::{self, LONG_LINE, SHORT_LINE};
use report::{BenchEchoReport, BenchRateReport, ConnectionsReport, BENCH_MULTIPLEX, CLIENT_NAME};
use stats::{summarize, Latencies};

fn define_flags() -> flags::FlagSet {
    let mut f = flags::FlagSet::new();
    // The servers' flags, which the drivers hand the client as well.
    f.int("m", 4 << 30, "accepted for the servers' sake: their memory limit");
    f.int("streams", 100, "accepted for the servers' sake: their max concurrent request streams per connection");
    f.duration("idle", "120s", "QUIC max idle timeout; a connection quiet for a quarter of it is sent a PING");

    // Server side
    f.string("f", "quicgo", r#"framework, e.g. "quicgo""#);
    f.string("ip", "127.0.0.1", r#"ip, e.g. "127.0.0.1""#);
    f.int("t", 0, "client: event-loop threads, 0 for one per CPU this process may run on");

    // Connections
    f.int("c", 10000, "client: num of connections");
    f.int("dc", 2000, "client: dial concurrency: how many handshakes are in progress at once");
    f.duration("dt", "5s", "client: dial timeout, which also bounds the first request on a connection");
    f.int("dr", 5, "client: dial retry times");
    f.duration("dri", "100ms", "client: dial retry interval");

    // BenchEcho && BenchMultiplex
    f.int("b", 1024, "benchmark: request body size of benchecho and benchrate, which the server echoes back");
    f.bool("check", false, "benchmark: whether to check the validity of the response data");
    f.int("pi", 1000, "benchmark: ps interval of benchecho and benchrate, 1000 ms by default");
    f.string(
        "ps",
        ps::MODE_AUTO,
        r#"benchmark: where the server's CPU and MEM samples come from: "auto" samples the server here when it runs on this machine and asks it over HTTP when it does not, "local" always samples here, "remote" always asks"#,
    );
    f.bool("tpn", true, "benchmark: whether enable TPN caculation");

    // BenchEcho
    f.int("ec", 10000, "benchecho: concurrency: how many connections have a request in flight at once");
    f.int("en", 2000000, "benchecho: benchmark times");
    f.int("el", 0, "benchecho: TPS limitation per second");
    f.bool("ep", false, "benchecho: generate pprof report");
    f.int("epd", 5, "benchecho: pprof duration");

    // BenchMultiplex
    f.bool("rate", false, "benchrate: whether run BenchMultiplex");
    f.int("rd", 10, "benchrate: how long to spend to do the test, in seconds");
    f.int("rr", 200, "benchrate: how many requests are sent on 1 conn every second");
    f.int("rbs", 16 * 1024, "benchrate: how many bytes of request bodies are sent on 1 conn at a time, when -rb is 0");
    f.int("rb", 0, "benchrate: batch: how many request streams are opened together on 1 conn, which must divide -rr; 0 takes as many as fit in -rbs bytes");
    f.int("rl", 0, "benchrate: request sending limitation per second");
    f.bool("rp", false, "benchrate: generate pprof report");
    f.int("rpd", 5, "benchrate: pprof duration");

    // Report files
    f.bool("r", false, "make report: done by benchreport (output/bin/bench.report), which script/report.sh runs");
    f.string("preffix", "", r#"report file preffix, e.g. "1m_connections_""#);
    f.string("suffix", "", r#"report file suffix, e.g. "_20060102150405""#);
    f.string("sort", "result", r#"accepted for benchreport's sake: report row order, "result" or "framework""#);
    f
}

/// The batch -rb asks for, or with -rb=0 the most request bodies that fit in
/// -rbs bytes and divide -rr, with how many times a second it is sent.
fn rate_batch(rb: usize, rate: usize, rbs: usize, payload: usize) -> Result<(usize, usize), String> {
    if rb > 0 {
        if rate % rb != 0 {
            return Err(format!(
                "batch {rb} does not divide the send rate {rate}, so no whole number of bursts a second sends it; pick a divisor of {rate}"
            ));
        }
        return Ok((rb, rate / rb));
    }
    let mut batch = (rbs / payload.max(1)).max(1);
    while batch > 1 && rate % batch != 0 {
        batch -= 1;
    }
    Ok((batch, rate / batch))
}

/// Splits total into n parts that differ by at most one.
fn split(total: usize, n: usize) -> Vec<usize> {
    (0..n).map(|i| total * (i + 1) / n - total * i / n).collect()
}

/// Splits total over the workers in proportion to what each holds, and no
/// more than it holds.
fn split_by(total: usize, holds: &[usize]) -> Vec<usize> {
    let sum: usize = holds.iter().sum();
    if sum == 0 {
        return vec![0; holds.len()];
    }
    let total = total.min(sum);
    let mut parts: Vec<usize> = holds.iter().map(|&h| total * h / sum).collect();
    let mut left = total - parts.iter().sum::<usize>();
    for (i, p) in parts.iter_mut().enumerate() {
        if left == 0 {
            break;
        }
        if *p < holds[i] {
            *p += 1;
            left -= 1;
        }
    }
    parts
}

fn http_get(addr: std::net::SocketAddr, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    ps::control_request(addr, "GET", path, b"", timeout)
}

/// Fetches a CPU and a heap profile from the server's pprof routes, two
/// seconds from now, as the Go client does once a benchmark is under way.
fn pprof_later(name: &'static str, addr: std::net::SocketAddr, seconds: i64) -> JoinHandle<Option<(Vec<u8>, Vec<u8>)>> {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let timeout = Duration::from_secs(seconds as u64 + 30);
        let cpu = match http_get(addr, &format!("/debug/pprof/profile?seconds={seconds}"), timeout) {
            Ok(v) => v,
            Err(e) => {
                println!("{name}: [pprof cpu] httpGet failed: {e}");
                return None;
            }
        };
        let mem = match http_get(addr, "/debug/pprof/heap", timeout) {
            Ok(v) => v,
            Err(e) => {
                println!("{name}: [pprof mem] httpGet failed: {e}");
                return None;
            }
        };
        Some((cpu, mem))
    })
}

/// The profiles, if they have arrived by the time the benchmark is over.
fn pprof_result(handle: Option<JoinHandle<Option<(Vec<u8>, Vec<u8>)>>>) -> Option<(Vec<u8>, Vec<u8>)> {
    let handle = handle?;
    if handle.is_finished() {
        handle.join().ok().flatten()
    } else {
        None
    }
}

fn save<T: serde::Serialize>(r: &T, name: &str, kind: &str, pprof: Option<&(Vec<u8>, Vec<u8>)>, preffix: &str, suffix: &str) {
    if let Err(e) = report::to_file(r, name, pprof, preffix, suffix) {
        logf!("{name}: writing the {kind} report failed: {e}");
    }
}

fn print_section(s: &str) {
    logging::print(SHORT_LINE);
    logging::print(s);
    logging::print("\n\n");
    logging::print(SHORT_LINE);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Ok(wd) = std::env::current_dir() {
        logf!("pwd: {}", wd.display());
    }
    let mut f = define_flags();
    if let Err(e) = f.parse(&args[1..]) {
        if !e.is_empty() {
            eprintln!("{e}");
        }
        eprint!("{}", f.usage(&args[0]));
        std::process::exit(2);
    }

    let sort = f.get_str("sort");
    if sort != "result" && sort != "framework" {
        fatalf!("report: unknown sort order {sort:?}, want one of [result framework]");
    }
    let ps_mode = f.get_str("ps");
    if let Err(e) = ps::validate_mode(&ps_mode) {
        fatalf!("{e}");
    }
    if f.get_bool("r") {
        fatalf!("-r: the report tables are written by benchreport; run script/report.sh");
    }

    let framework = f.get_str("f");
    let ip = f.get_str("ip");
    let payload = f.get_int("b").max(1) as usize;
    let rate_enabled = f.get_bool("rate");
    let rate_send = f.get_int("rr").max(1) as usize;
    let rate_plan = if rate_enabled {
        match rate_batch(f.get_int("rb").max(-1) as usize, rate_send, f.get_int("rbs").max(1) as usize, payload) {
            Ok(v) => Some(v),
            Err(e) => fatalf!("-rb={}: {e}", f.get_int("rb")),
        }
    } else {
        None
    };
    if f.get_int("rb") < 0 {
        fatalf!("-rb={}: want 0, which fits as many requests as -rbs holds, or more", f.get_int("rb"));
    }
    if !config::is_framework(&framework) {
        fatalf!("-f={framework}: unknown framework {framework:?}, want one of {:?}", config::framework_names());
    }
    let addrs = match config::benchmark_addrs(&framework, &ip) {
        Ok(a) => a,
        Err(e) => fatalf!("{e}"),
    };
    let control = config::control_addr(&framework, &ip).ok();
    let preffix = f.get_str("preffix");
    let suffix = f.get_str("suffix");
    let tpn = f.get_bool("tpn");
    let threads = match f.get_int("t") {
        n if n > 0 => n as usize,
        _ => std::thread::available_parallelism().map_or(1, |n| n.get()),
    };
    let num_conns = f.get_int("c").max(1) as usize;
    let echo_times = f.get_int("en").max(0) as u64;
    let ps_interval = Duration::from_millis(f.get_int("pi").max(1) as u64);

    logging::print(LONG_LINE);
    logf!("Benchmark [{framework}]: {num_conns} connections, {payload} payload, {echo_times} times, {threads} threads");
    logging::print(SHORT_LINE);

    let engine = match Engine::new(
        threads,
        EngineConfig {
            addrs,
            authority: config::authority(&ip),
            dial_timeout: f.get_duration("dt"),
            dial_retries: f.get_int("dr").max(1) as usize,
            dial_retry_interval: f.get_duration("dri"),
            idle_timeout: f.get_duration("idle"),
            check: f.get_bool("check"),
        },
    ) {
        Ok(e) => e,
        Err(e) => fatalf!("starting the client: {e}"),
    };

    // -- Connections ---------------------------------------------------------
    let dial_conc = (f.get_int("dc").max(1) as usize).min(num_conns);
    logf!("Dial Connections: [{num_conns}]");
    logf!("Dial Concurrency: [{dial_conc}]");
    logf!("Connections start ...");
    let counts = split(num_conns, threads);
    let concs = split(dial_conc, threads);
    let commands = counts
        .iter()
        .zip(&concs)
        .map(|(&count, &conc)| Command::Dial { count, concurrency: if count > 0 { conc.max(1) } else { 0 } })
        .collect();
    let begin = Instant::now();
    let connected = engine.connected.clone();
    let outcomes = engine.run(commands, |s| {
        logf!("{s:03} seconds passed, {} Connected ...", connected.load(Ordering::Relaxed));
    });
    let used = begin.elapsed();
    let mut lat = Latencies::default();
    for o in outcomes {
        if let Outcome::Dial(l) = o {
            lat.merge(l);
        }
    }
    logf!("Connections done: {} Success, {} Failed", lat.success, lat.failed);
    if !lat.errors.is_empty() {
        logf!("Connections errors: {:?}", lat.errors);
    }
    let s = summarize(&mut lat, used);
    let cr = ConnectionsReport {
        framework: framework.clone(),
        bench_client: CLIENT_NAME.into(),
        threads,
        tps: s.tps,
        min: s.min,
        avg: s.avg,
        max: s.max,
        tp50: if tpn { s.tp50 } else { 0 },
        tp75: if tpn { s.tp75 } else { 0 },
        tp90: if tpn { s.tp90 } else { 0 },
        tp95: if tpn { s.tp95 } else { 0 },
        tp99: if tpn { s.tp99 } else { 0 },
        used: used.as_nanos() as i64,
        total: num_conns,
        success: lat.success,
        failed: lat.failed,
        concurrency: dial_conc,
    };
    save(&cr, &format!("{framework}-Connections"), "Connections", None, &preffix, &suffix);
    print_section(&cr.console(&s, tpn));

    // -- resource sampling ---------------------------------------------------
    let (source, err) = ps::setup(&framework, &ip, &ps_mode, ps_interval);
    if let Some(e) = err {
        logf!("SetupPS({framework}) failed: {e}");
    }
    if let Some(addr) = control.filter(|_| config::serves_pprof(&framework)) {
        println!("pprof cpu :\n  curl --output ./cpu_profile http://{addr}/debug/pprof/profile");
        println!("  go tool pprof -http=:6060 ./cpu_profile");
        println!("pprof heap:\n  curl --output ./mem_profile http://{addr}/debug/pprof/heap");
        println!("  go tool pprof -http=:6061 ./mem_profile");
        logging::print(SHORT_LINE);
    }

    // -- BenchEcho -----------------------------------------------------------
    let ready = engine.ready_counts();
    let total_ready: usize = ready.iter().sum();
    if total_ready == 0 {
        engine.stop();
        fatalf!("BenchEcho: no connections to run on");
    }
    let echo_conc = (f.get_int("ec").max(1) as usize).min(total_ready);
    let echo_concs = split_by(echo_conc, &ready);
    let payloads: Vec<Arc<Vec<u8>>> = (0..1024)
        .map(|_| {
            let mut b = vec![0u8; payload];
            rand::fill(&mut b[..]);
            Arc::new(b)
        })
        .collect();
    let echo_limit = f.get_int("el").max(0) as usize;
    let echo_job = |times: u64, measure: bool| {
        Arc::new(EchoJob {
            times,
            issued: AtomicU64::new(0),
            payloads: payloads.clone(),
            measure,
            limiter: (echo_limit > 0).then(|| Limiter::new(echo_limit)),
        })
    };
    let echo_commands = |job: &Arc<EchoJob>| -> Vec<Command> {
        echo_concs.iter().map(|&c| Command::Echo { job: job.clone(), concurrency: c }).collect()
    };

    let warmup = (total_ready as u64 * 5).min(2_000_000);
    logf!("BenchEcho Warmup for {warmup} times ...");
    let pprof = control.filter(|_| config::serves_pprof(&framework));
    let echo_pprof = (f.get_bool("ep") && pprof.is_some())
        .then(|| pprof_later("BenchEcho", pprof.unwrap(), f.get_int("epd")));
    engine.run(echo_commands(&echo_job(warmup, false)), |_| {});
    logf!("BenchEcho Warmup for {warmup} times done");

    logf!("BenchEcho for {echo_times} times ...");
    let mark = source.mark();
    let begin = Instant::now();
    let outcomes = engine.run(echo_commands(&echo_job(echo_times, true)), |_| {});
    let used = begin.elapsed();
    logf!("BenchEcho for {echo_times} times done");
    let mut lat = Latencies::default();
    for o in outcomes {
        if let Outcome::Echo(l) = o {
            lat.merge(l);
        }
    }
    if !lat.errors.is_empty() {
        logf!("BenchEcho errors: {:?}", lat.errors);
    }
    let s = summarize(&mut lat, used);
    let (ps_stats, err) = source.stats_since(mark);
    if let Some(e) = err {
        logf!("BenchEcho: resource statistics for {framework} incomplete, CPU EER and MEM EER will read 0: {e}");
    }
    let er = BenchEchoReport {
        framework: framework.clone(),
        bench_client: CLIENT_NAME.into(),
        threads,
        tps: s.tps,
        cpu_eer: report::cpu_eer(s.tps as f64, ps_stats.cpu_avg),
        mem_eer: report::mem_eer(s.tps as f64, ps_stats.mem_avg),
        min: s.min,
        avg: s.avg,
        max: s.max,
        tp50: if tpn { s.tp50 } else { 0 },
        tp75: if tpn { s.tp75 } else { 0 },
        tp90: if tpn { s.tp90 } else { 0 },
        tp95: if tpn { s.tp95 } else { 0 },
        tp99: if tpn { s.tp99 } else { 0 },
        used: used.as_nanos() as i64,
        total: echo_times,
        success: lat.success,
        failed: lat.failed,
        connections: total_ready,
        concurrency: echo_conc,
        payload,
        ps: ps_stats,
        pprof_on: f.get_bool("ep"),
        pprof: pprof_result(echo_pprof),
    };
    save(&er, &format!("{framework}-BenchEcho"), "BenchEcho", er.pprof.as_ref(), &preffix, &suffix);
    print_section(&er.console(&s, tpn));

    // -- BenchMultiplex ------------------------------------------------------
    if let Some((batch, tick_rate)) = rate_plan {
        let duration = Duration::from_secs(f.get_int("rd").max(1) as u64);
        let mut body = vec![0u8; payload];
        rand::fill(&mut body[..]);
        let rate_limit = f.get_int("rl").max(0) as usize;
        let job = Arc::new(RateJob {
            duration,
            batch,
            tick: Duration::from_secs(1) / tick_rate as u32,
            payload: Arc::new(body),
            limiter: (rate_limit > 0).then(|| Limiter::new(rate_limit)),
        });
        let conns: usize = engine.ready_counts().iter().sum();
        logf!("{BENCH_MULTIPLEX} for {:.2} seconds, {batch} request streams opened together ...", duration.as_secs_f64());
        let rate_pprof = (f.get_bool("rp") && pprof.is_some())
            .then(|| pprof_later(BENCH_MULTIPLEX, pprof.unwrap(), f.get_int("rpd")));
        let mark = source.mark();
        let outcomes = engine.run((0..engine.threads()).map(|_| Command::Rate(job.clone())).collect(), |_| {});
        logf!("{BENCH_MULTIPLEX} for {:.2} seconds done", duration.as_secs_f64());
        let mut counts = engine::RateCounts::default();
        for o in outcomes {
            if let Outcome::Rate(c) = o {
                counts.send_times += c.send_times;
                counts.send_bytes += c.send_bytes;
                counts.recv_times += c.recv_times;
                counts.recv_bytes += c.recv_bytes;
            }
        }
        let (ps_stats, err) = source.stats_since(mark);
        if let Some(e) = err {
            logf!("{BENCH_MULTIPLEX}: resource statistics for {framework} incomplete, CPU EER and MEM EER will read 0: {e}");
        }
        let tps = counts.recv_times as f64 / duration.as_secs_f64();
        let rr = BenchRateReport {
            framework: framework.clone(),
            bench_client: CLIENT_NAME.into(),
            threads,
            duration: duration.as_nanos() as i64,
            tps: tps.floor() as i64,
            cpu_eer: report::cpu_eer(tps, ps_stats.cpu_avg),
            mem_eer: report::mem_eer(tps, ps_stats.mem_avg),
            send_times: counts.send_times,
            send_bytes: counts.send_bytes,
            recv_times: counts.recv_times,
            recv_bytes: counts.recv_bytes,
            connections: conns,
            send_rate: rate_send,
            batch,
            payload,
            ps: ps_stats,
            pprof_on: f.get_bool("rp"),
            pprof: pprof_result(rate_pprof),
        };
        save(&rr, &format!("{framework}-{BENCH_MULTIPLEX}"), BENCH_MULTIPLEX, rr.pprof.as_ref(), &preffix, &suffix);
        print_section(&rr.console());
    }

    drop(source);
    engine.stop();
    logging::print(LONG_LINE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches() {
        assert_eq!(rate_batch(0, 200, 16384, 1024), Ok((10, 20)));
        assert_eq!(rate_batch(50, 200, 16384, 1024), Ok((50, 4)));
        assert!(rate_batch(30, 200, 16384, 1024).is_err());
        assert_eq!(rate_batch(0, 7, 16384, 100_000), Ok((1, 7)));
    }

    #[test]
    fn splits() {
        assert_eq!(split(10, 3), vec![3, 3, 4]);
        assert_eq!(split(2, 4), vec![0, 1, 0, 1]);
        assert_eq!(split_by(10, &[5, 5]), vec![5, 5]);
        assert_eq!(split_by(3, &[1, 10]), vec![1, 2]);
        assert_eq!(split_by(100, &[3, 4]), vec![3, 4]);
    }
}
