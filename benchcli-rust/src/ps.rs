//! Where a report's CPU and MEM columns - and so CPU EER and MEM EER - come from.
//!
//! The server is sampled from this machine when it runs here: the process is
//! found by the name script/build.sh built it under, <framework>.server, and
//! its CPU time and resident memory are read straight from the operating
//! system every -pi milliseconds. Nothing is asked of the server, so nothing
//! fails because it is busy. A server on another machine samples itself: /init
//! starts that on its control port and answers with its pid, and /ps answers
//! with the samples. This is the Go benchmarks' config.SetupPS, and the
//! numbers are worked out the way github.com/lesismal/perf's PSCounter works
//! them out.
//!
//! Unlike the Go benchmarks, each benchmark reads only the samples taken
//! while it ran, so BenchMultiplex's CPU is not averaged with BenchEcho's.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use benchkit::procstat::{process_name, processes_named, Sampler};
use serde::Serialize;

use crate::config;

pub const MODE_AUTO: &str = "auto";
pub const MODE_LOCAL: &str = "local";
pub const MODE_REMOTE: &str = "remote";

pub fn validate_mode(mode: &str) -> Result<(), String> {
    match mode {
        MODE_AUTO | MODE_LOCAL | MODE_REMOTE => Ok(()),
        _ => Err(format!(
            "unsupported -ps value {mode:?} (want {MODE_AUTO}, {MODE_LOCAL} or {MODE_REMOTE})"
        )),
    }
}

#[derive(Serialize, Default, Clone, Debug)]
pub struct PsStats {
    #[serde(rename = "CPUMin")]
    pub cpu_min: f64,
    #[serde(rename = "CPUAvg")]
    pub cpu_avg: f64,
    #[serde(rename = "CPUMax")]
    pub cpu_max: f64,
    #[serde(rename = "MEMMin")]
    pub mem_min: u64,
    #[serde(rename = "MEMAvg")]
    pub mem_avg: u64,
    #[serde(rename = "MEMMax")]
    pub mem_max: u64,
}

/// perf.PSCounter's statistics. The first sample is left out of the minimum
/// and the average, as perf leaves it out: it covers the interval in which
/// sampling started rather than one the benchmark filled. The memory minimum
/// is the second smallest sample, as perf has it.
pub fn stats(cpu: &[f64], rss: &[u64]) -> PsStats {
    let mut s = PsStats::default();
    match cpu.len() {
        0 => {}
        1 => {
            s.cpu_min = cpu[0];
            s.cpu_avg = cpu[0];
            s.cpu_max = cpu[0];
        }
        n => {
            s.cpu_min = cpu[1..].iter().copied().fold(f64::MAX, f64::min);
            s.cpu_avg = cpu[1..].iter().sum::<f64>() / (n - 1) as f64;
            s.cpu_max = cpu.iter().copied().fold(0.0, f64::max);
        }
    }
    match rss.len() {
        0 => {}
        1 => {
            s.mem_min = rss[0];
            s.mem_avg = rss[0];
            s.mem_max = rss[0];
        }
        n => {
            let mut sorted = rss.to_vec();
            sorted.sort_unstable();
            s.mem_min = sorted[1];
            s.mem_avg = rss[1..].iter().sum::<u64>() / (n - 1) as u64;
            s.mem_max = sorted[n - 1];
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Control requests.

/// One HTTP/1.0 request to a server's control port, retried the way the Go
/// client retries it: the process may be slow to answer while it works
/// through the backlog of a benchmark that has just finished. A reply the
/// server actually produced is returned as it is, 404 included.
pub fn control_request(
    addr: SocketAddr, method: &str, path: &str, body: &[u8], timeout: Duration,
) -> Result<Vec<u8>, String> {
    const ATTEMPTS: u32 = 4;
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            std::thread::sleep(Duration::from_secs(2 * (attempt - 1) as u64));
        }
        match control_once(addr, method, path, body, timeout) {
            Ok((200, body)) => return Ok(body),
            Ok((status, body)) => {
                return Err(format!(
                    "http://{addr}{path}: status {status}: {}",
                    String::from_utf8_lossy(&body).trim()
                ))
            }
            Err(e) => {
                last = format!("http://{addr}{path}: {e}");
                if attempt < ATTEMPTS {
                    logf!("control request failed, retrying ({attempt}/{ATTEMPTS}): {last}");
                }
            }
        }
    }
    Err(last)
}

fn control_once(
    addr: SocketAddr, method: &str, path: &str, body: &[u8], timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {addr}\r\n");
    if method == "POST" {
        req += &format!("Content-Length: {}\r\n", body.len());
    }
    req += "\r\n";
    let mut data = req.into_bytes();
    data.extend_from_slice(body);
    stream.write_all(&data).map_err(|e| e.to_string())?;
    // HTTP/1.0 without keep-alive: the server closes the connection after the
    // response, so its end is the end of the body.
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "malformed response".to_string())?;
    let head = String::from_utf8_lossy(&resp[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {:?}", head.lines().next()))?;
    Ok((status, resp[split + 4..].to_vec()))
}

// ---------------------------------------------------------------------------
// The server's own sampling, read over /ps.

pub struct Remote {
    addr: SocketAddr,
}

#[derive(serde::Deserialize, Default)]
struct PsBody {
    #[serde(default)]
    cpu: Option<Vec<f64>>,
    #[serde(default)]
    mem: Option<Vec<MemBody>>,
}

#[derive(serde::Deserialize)]
struct MemBody {
    #[serde(default)]
    rss: u64,
}

impl Remote {
    fn init(&self, interval: Duration) -> Result<i32, String> {
        let body = format!("{{\"PsInterval\":{}}}", interval.as_nanos());
        let resp = control_request(self.addr, "POST", "/init", body.as_bytes(), Duration::from_secs(30))?;
        String::from_utf8_lossy(&resp)
            .trim()
            .parse()
            .map_err(|e| format!("http://{}/init answered {:?}: {e}", self.addr, String::from_utf8_lossy(&resp)))
    }

    fn samples(&self) -> Result<(Vec<f64>, Vec<u64>), String> {
        let resp = control_request(self.addr, "GET", "/ps", b"", Duration::from_secs(30))?;
        let body: PsBody =
            serde_json::from_slice(&resp).map_err(|e| format!("http://{}/ps: {e}", self.addr))?;
        Ok((
            body.cpu.unwrap_or_default(),
            body.mem.unwrap_or_default().into_iter().map(|m| m.rss).collect(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Sampling from this machine.

/// The pid of the framework's server on this machine, found by the name it
/// was built under. Two of them is an error rather than a guess - a leftover
/// from an earlier run - and so is none, which is what a server on another
/// machine or in another container looks like from here.
pub fn find_server_process(framework: &str) -> Result<i32, String> {
    let name = format!("{framework}.server");
    let pids = processes_named(&name)?;
    match pids.len() {
        0 => Err(format!("no {name} process on this machine")),
        1 => Ok(pids[0]),
        n => Err(format!(
            "{n} {name} processes on this machine ({pids:?}): stop the leftovers of earlier runs, e.g. with script/killall.sh"
        )),
    }
}

// ---------------------------------------------------------------------------

/// Where a run reads the server's samples from, set up once after the
/// connections are up and shared by the benchmarks that follow.
pub enum PsSource {
    Local { sampler: Sampler, fallback: Option<Remote> },
    Remote(Remote),
}

/// How many samples there were when a benchmark started, so that its report
/// reads only the ones taken while it ran.
#[derive(Default, Clone, Copy)]
pub struct Mark {
    cpu: usize,
    mem: usize,
}

impl PsSource {
    pub fn describe(&self) -> String {
        match self {
            PsSource::Local { sampler, fallback: None } => format!("sampled here, from pid {}", sampler.pid()),
            PsSource::Local { sampler, fallback: Some(_) } => {
                format!("sampled here, from pid {}, with the server's own /ps as the fallback", sampler.pid())
            }
            PsSource::Remote(r) => format!("sampled by the server, read from {} over /ps", r.addr),
        }
    }

    pub fn mark(&self) -> Mark {
        match self {
            PsSource::Local { sampler, .. } => {
                let (cpu, mem) = sampler.len();
                Mark { cpu, mem }
            }
            PsSource::Remote(r) => match r.samples() {
                Ok((cpu, rss)) => Mark { cpu: cpu.len(), mem: rss.len() },
                Err(_) => Mark::default(),
            },
        }
    }

    /// The statistics of the samples taken since mark. It returns what it has
    /// along with an error, so that the columns are incomplete rather than
    /// missing when something went wrong.
    pub fn stats_since(&self, mark: Mark) -> (PsStats, Option<String>) {
        let (cpu, rss) = match self {
            PsSource::Local { sampler, fallback } => {
                let (cpu, rss) = sampler.since(mark.cpu, mark.mem);
                if cpu.is_empty() {
                    if let Some(remote) = fallback {
                        return remote_stats(remote, Mark::default());
                    }
                    return (
                        stats(&cpu, &rss),
                        Some(format!(
                            "pid {}: no CPU samples in this benchmark, so it was shorter than the -pi sampling interval",
                            sampler.pid()
                        )),
                    );
                }
                (cpu, rss)
            }
            PsSource::Remote(r) => return remote_stats(r, mark),
        };
        (stats(&cpu, &rss), None)
    }
}

fn remote_stats(r: &Remote, mark: Mark) -> (PsStats, Option<String>) {
    match r.samples() {
        Ok((cpu, rss)) => {
            let cpu = &cpu[mark.cpu.min(cpu.len())..];
            let rss = &rss[mark.mem.min(rss.len())..];
            let s = stats(cpu, rss);
            if s.cpu_avg <= 0.0 {
                return (
                    s,
                    Some(format!(
                        "http://{}/ps answered with no CPU samples, so either /init did not reach it or the benchmark was shorter than the -pi sampling interval",
                        r.addr
                    )),
                );
            }
            (s, None)
        }
        Err(e) => (PsStats::default(), Some(e)),
    }
}

/// Decides how this run reads the server's CPU and memory, and starts doing
/// it; see the module comment. The source it returns is usable whatever
/// happened, and the error says what went wrong on the way.
pub fn setup(framework: &str, ip: &str, mode: &str, interval: Duration) -> (PsSource, Option<String>) {
    let addr = match config::control_addr(framework, ip) {
        Ok(a) => a,
        Err(e) => {
            return (PsSource::Remote(Remote { addr: SocketAddr::from(([127, 0, 0, 1], 0)) }), Some(e));
        }
    };
    let remote = Remote { addr };
    let want_local = mode == MODE_LOCAL || (mode == MODE_AUTO && config::is_local_host(ip));
    if want_local {
        let started = find_server_process(framework).and_then(|pid| Sampler::start(pid, interval));
        match started {
            Ok(sampler) => {
                let source = PsSource::Local { sampler, fallback: None };
                logf!("{framework}: {}, so it is not asked to sample itself", source.describe());
                return (source, None);
            }
            Err(e) => logf!("{framework}: cannot sample the server from this machine, asking it over HTTP instead: {e}"),
        }
    }

    let pid = match remote.init(interval) {
        Ok(pid) => pid,
        Err(e) => return (PsSource::Remote(remote), Some(e)),
    };
    // The pid is from the server's own namespace, so it names the server
    // here only when the two share one. Where they do, sample it from here
    // as well, and keep /ps for when this has nothing.
    if want_local && process_name(pid).as_deref() == Some(format!("{framework}.server").as_str()) {
        if let Ok(sampler) = Sampler::start(pid, interval) {
            let source = PsSource::Local { sampler, fallback: Some(remote) };
            logf!("{framework}: {}", source.describe());
            return (source, None);
        }
    }
    let source = PsSource::Remote(remote);
    logf!("{framework}: {}", source.describe());
    (source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_statistics() {
        let s = stats(&[500.0, 100.0, 300.0], &[10, 30, 20]);
        assert_eq!((s.cpu_min, s.cpu_avg, s.cpu_max), (100.0, 200.0, 500.0));
        assert_eq!((s.mem_min, s.mem_avg, s.mem_max), (20, 25, 30));
        let one = stats(&[7.0], &[9]);
        assert_eq!((one.cpu_min, one.cpu_avg, one.mem_min), (7.0, 7.0, 9));
        assert_eq!(stats(&[], &[]).cpu_avg, 0.0);
    }

    #[test]
    fn missing_server() {
        assert!(find_server_process("no-such-framework").is_err());
    }
}
