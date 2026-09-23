//! A process' CPU time and resident memory, read straight from the operating
//! system, and a sampler that turns them into the per-interval CPU percent and
//! RSS series github.com/lesismal/perf's PSCounter keeps. The client samples a
//! server on its own machine with it, and the quiche server answers /ps from
//! it the way the Go servers answer from perf.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The pids whose argv[0] has this base name.
pub fn processes_named(name: &str) -> Result<Vec<i32>, String> {
    if std::path::Path::new("/proc/self").exists() {
        let entries = std::fs::read_dir("/proc").map_err(|e| e.to_string())?;
        let mut pids = Vec::new();
        for entry in entries.flatten() {
            if let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() {
                if process_name(pid).as_deref() == Some(name) {
                    pids.push(pid);
                }
            }
        }
        return Ok(pids);
    }
    // No /proc: macOS, where ps knows every process' executable path.
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,comm="])
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // An executable path may hold spaces, so everything after the pid.
        let Some((pid, comm)) = line.trim_start().split_once(char::is_whitespace) else { continue };
        if basename(comm.trim()) == name {
            if let Ok(pid) = pid.parse() {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The base name of a pid's argv[0].
pub fn process_name(pid: i32) -> Option<String> {
    if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let argv0 = cmdline.split(|&b| b == 0).next()?;
        if argv0.is_empty() {
            return None;
        }
        return Some(basename(&String::from_utf8_lossy(argv0)).to_string());
    }
    let out = std::process::Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().ok()?;
    let comm = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if comm.is_empty() {
        None
    } else {
        Some(basename(&comm).to_string())
    }
}

/// A process' CPU time so far, user and system, and its resident memory.
#[cfg(target_os = "linux")]
pub fn cpu_and_rss(pid: i32) -> Result<(Duration, u64), String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    // The command name is in parentheses and may hold spaces; the fields
    // that follow the last ')' start with the state, field 3.
    let rest = &stat[stat.rfind(')').ok_or("malformed stat")? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| -> Result<u64, String> {
        fields.get(i).and_then(|v| v.parse().ok()).ok_or_else(|| "malformed stat".to_string())
    };
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let cpu_ticks = ticks(11)? + ticks(12)?;
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).map_err(|e| e.to_string())?;
    let pages: u64 = statm.split_whitespace().nth(1).and_then(|v| v.parse().ok()).ok_or("malformed statm")?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    Ok((Duration::from_nanos(cpu_ticks * 1_000_000_000 / hz), pages * page))
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn cpu_and_rss(pid: i32) -> Result<(Duration, u64), String> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut libc::c_void, size)
    };
    if n != size {
        return Err(format!("proc_pidinfo({pid}): {}", std::io::Error::last_os_error()));
    }
    // The task's times are in Mach absolute time units, which are
    // nanoseconds on Intel and not on Apple silicon.
    let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
    unsafe {
        libc::mach_timebase_info(&mut tb);
    }
    let units = (info.pti_total_user + info.pti_total_system) as u128;
    let ns = units * tb.numer.max(1) as u128 / tb.denom.max(1) as u128;
    Ok((Duration::from_nanos(ns as u64), info.pti_resident_size))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cpu_and_rss(_pid: i32) -> Result<(Duration, u64), String> {
    Err("sampling a process is only supported on Linux and macOS".into())
}

#[derive(Default)]
struct Samples {
    cpu: Vec<f64>,
    rss: Vec<u64>,
}

/// Samples a process every interval, in a thread of its own, until dropped:
/// the CPU it used over the interval, as a percent of one core, and its
/// resident memory at the end of it.
pub struct Sampler {
    pid: i32,
    samples: Arc<Mutex<Samples>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Starts sampling, after one read that proves the process can be
    /// sampled at all.
    pub fn start(pid: i32, interval: Duration) -> Result<Self, String> {
        let interval = if interval.is_zero() { Duration::from_secs(1) } else { interval };
        let (mut last_cpu, _) = cpu_and_rss(pid).map_err(|e| format!("pid {pid}: {e}"))?;
        let samples = Arc::new(Mutex::new(Samples::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (samples.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("ps-sampler".into())
            .spawn(move || {
                let mut last = Instant::now();
                let mut failures = 0;
                loop {
                    let next = last + interval;
                    while Instant::now() < next {
                        if st.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep((next - Instant::now()).min(Duration::from_millis(50)));
                    }
                    let now = Instant::now();
                    match cpu_and_rss(pid) {
                        Ok((cpu, rss)) => {
                            failures = 0;
                            let wall = now.duration_since(last).as_secs_f64();
                            let percent = cpu.saturating_sub(last_cpu).as_secs_f64() / wall * 100.0;
                            let mut set = s.lock().unwrap();
                            set.cpu.push(percent);
                            set.rss.push(rss);
                            last_cpu = cpu;
                        }
                        Err(e) => {
                            // Almost always a process that has exited;
                            // what was sampled before that is kept.
                            failures += 1;
                            if failures >= 5 {
                                crate::logf!("sampling pid {pid} stopped after {failures} failures: {e}");
                                return;
                            }
                        }
                    }
                    last = now;
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { pid, samples, stop, thread: Some(thread) })
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// How many CPU and memory samples there are so far.
    pub fn len(&self) -> (usize, usize) {
        let s = self.samples.lock().unwrap();
        (s.cpu.len(), s.rss.len())
    }

    /// The samples after the first cpu and mem of each series.
    pub fn since(&self, cpu: usize, mem: usize) -> (Vec<f64>, Vec<u64>) {
        let s = self.samples.lock().unwrap();
        (s.cpu[cpu.min(s.cpu.len())..].to_vec(), s.rss[mem.min(s.rss.len())..].to_vec())
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_self() {
        let me = std::process::id() as i32;
        let (_, rss) = cpu_and_rss(me).unwrap();
        assert!(rss > 0);
        let sampler = Sampler::start(me, Duration::from_millis(50)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // Burn a little CPU so there is something to measure.
        let mut x = 0u64;
        while sampler.len().0 < 3 && Instant::now() < deadline {
            x = x.wrapping_mul(31).wrapping_add(7);
        }
        assert!(x != 1);
        let (cpu, rss) = sampler.since(0, 0);
        assert!(cpu.len() >= 3 && cpu.iter().any(|&c| c > 0.0), "{cpu:?}");
        assert!(rss.iter().all(|&r| r > 0));
        assert!(processes_named("no-such-process-name").unwrap().is_empty());
        let name = process_name(me).unwrap();
        assert!(processes_named(&name).unwrap().contains(&me));
    }
}
