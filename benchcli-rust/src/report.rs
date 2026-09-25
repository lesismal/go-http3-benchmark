//! The JSON report files the client writes, one per framework and benchmark,
//! which benchreport turns into the Markdown tables. The field names are the
//! JSON names of the structs in benchreport/report, and have to stay so.

use serde::Serialize;

use crate::ps::PsStats;
use crate::stats::{mem_string, time_string, Summary};

pub const CLIENT_NAME: &str = "benchcli-rust";
pub const BENCH_MULTIPLEX: &str = "BenchMultiplex";

#[derive(Serialize)]
pub struct ConnectionsReport {
    #[serde(rename = "Framework")]
    pub framework: String,
    #[serde(rename = "BenchClient")]
    pub bench_client: String,
    #[serde(rename = "Threads")]
    pub threads: usize,
    #[serde(rename = "TPS")]
    pub tps: i64,
    #[serde(rename = "Min")]
    pub min: i64,
    #[serde(rename = "Avg")]
    pub avg: i64,
    #[serde(rename = "Max")]
    pub max: i64,
    #[serde(rename = "TP50")]
    pub tp50: i64,
    #[serde(rename = "TP75")]
    pub tp75: i64,
    #[serde(rename = "TP90")]
    pub tp90: i64,
    #[serde(rename = "TP95")]
    pub tp95: i64,
    #[serde(rename = "TP99")]
    pub tp99: i64,
    #[serde(rename = "Used")]
    pub used: i64,
    #[serde(rename = "Total")]
    pub total: usize,
    #[serde(rename = "Success")]
    pub success: i64,
    #[serde(rename = "Failed")]
    pub failed: i64,
    #[serde(rename = "Concurrency")]
    pub concurrency: usize,
}

#[derive(Serialize)]
pub struct BenchEchoReport {
    #[serde(rename = "Framework")]
    pub framework: String,
    #[serde(rename = "BenchClient")]
    pub bench_client: String,
    #[serde(rename = "Threads")]
    pub threads: usize,
    #[serde(rename = "TPS")]
    pub tps: i64,
    #[serde(rename = "CPUEER")]
    pub cpu_eer: f64,
    #[serde(rename = "MEMEER")]
    pub mem_eer: f64,
    #[serde(rename = "Min")]
    pub min: i64,
    #[serde(rename = "Avg")]
    pub avg: i64,
    #[serde(rename = "Max")]
    pub max: i64,
    #[serde(rename = "TP50")]
    pub tp50: i64,
    #[serde(rename = "TP75")]
    pub tp75: i64,
    #[serde(rename = "TP90")]
    pub tp90: i64,
    #[serde(rename = "TP95")]
    pub tp95: i64,
    #[serde(rename = "TP99")]
    pub tp99: i64,
    #[serde(rename = "Used")]
    pub used: i64,
    #[serde(rename = "Total")]
    pub total: u64,
    #[serde(rename = "Success")]
    pub success: i64,
    #[serde(rename = "Failed")]
    pub failed: i64,
    #[serde(rename = "Conns")]
    pub connections: usize,
    #[serde(rename = "Concurrency")]
    pub concurrency: usize,
    #[serde(rename = "Payload")]
    pub payload: usize,
    #[serde(flatten)]
    pub ps: PsStats,
    /// Whether the client was set to profile Go servers through pprof while
    /// this benchmark ran (-ep, -rp), which costs them throughput.
    #[serde(rename = "Pprof")]
    pub pprof_on: bool,
    #[serde(skip)]
    pub pprof: Option<(Vec<u8>, Vec<u8>)>,
}

#[derive(Serialize)]
pub struct BenchRateReport {
    #[serde(rename = "Framework")]
    pub framework: String,
    #[serde(rename = "BenchClient")]
    pub bench_client: String,
    #[serde(rename = "Threads")]
    pub threads: usize,
    #[serde(rename = "Duration")]
    pub duration: i64,
    #[serde(rename = "TPS")]
    pub tps: i64,
    #[serde(rename = "CPUEER")]
    pub cpu_eer: f64,
    #[serde(rename = "MEMEER")]
    pub mem_eer: f64,
    #[serde(rename = "SendTimes")]
    pub send_times: u64,
    #[serde(rename = "SendBytes")]
    pub send_bytes: u64,
    #[serde(rename = "RecvTimes")]
    pub recv_times: u64,
    #[serde(rename = "RecvBytes")]
    pub recv_bytes: u64,
    #[serde(rename = "Conns")]
    pub connections: usize,
    #[serde(rename = "SendRate")]
    pub send_rate: usize,
    #[serde(rename = "Batch")]
    pub batch: usize,
    #[serde(rename = "Payload")]
    pub payload: usize,
    #[serde(flatten)]
    pub ps: PsStats,
    /// Whether the client was set to profile Go servers through pprof while
    /// this benchmark ran (-ep, -rp), which costs them throughput.
    #[serde(rename = "Pprof")]
    pub pprof_on: bool,
    #[serde(skip)]
    pub pprof: Option<(Vec<u8>, Vec<u8>)>,
}

/// CPU EER: the throughput a server got for each percent of a CPU core it
/// spent, or 0 when there is nothing to divide by.
pub fn cpu_eer(throughput: f64, cpu_avg: f64) -> f64 {
    eer(throughput, cpu_avg)
}

/// MEM EER: the throughput a server got for each MB (1024*1024 bytes) of
/// resident memory it held, or 0 when there is nothing to divide by.
pub fn mem_eer(throughput: f64, mem_avg: u64) -> f64 {
    eer(throughput, mem_avg as f64 / (1024.0 * 1024.0))
}

/// throughput / cost, or 0 when there is nothing to divide by, which JSON
/// could not carry otherwise.
fn eer(throughput: f64, cost: f64) -> f64 {
    if cost <= 0.0 || !cost.is_finite() || !throughput.is_finite() {
        return 0.0;
    }
    let v = throughput / cost;
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

pub fn filename(base: &str, preffix: &str, suffix: &str) -> String {
    format!("./output/report/{preffix}{base}{suffix}")
}

/// Writes one report's JSON, and the pprof profiles taken during it if there
/// are any, next to the others benchreport reads.
pub fn to_file<T: Serialize>(
    report: &T, name: &str, pprof: Option<&(Vec<u8>, Vec<u8>)>, preffix: &str, suffix: &str,
) -> Result<(), String> {
    std::fs::create_dir_all("./output/report").map_err(|e| e.to_string())?;
    if let Some((cpu, mem)) = pprof {
        if !cpu.is_empty() {
            std::fs::write(filename(name, preffix, &format!("{suffix}.pprof.cpu")), cpu)
                .map_err(|e| e.to_string())?;
        }
        if !mem.is_empty() {
            std::fs::write(filename(name, preffix, &format!("{suffix}.pprof.mem")), mem)
                .map_err(|e| e.to_string())?;
        }
    }
    let json = serde_json::to_vec(report).map_err(|e| e.to_string())?;
    std::fs::write(filename(name, preffix, &format!("{suffix}.json")), json).map_err(|e| e.to_string())
}

fn client_name() -> String {
    CLIENT_NAME.trim_start_matches("benchcli-").to_string()
}

/// The block a benchmark prints to the console when it finishes: one
/// "name: value" line per column, the names padded to one width, with the
/// benchmark's type in front of the framework the way the Go client prints it.
fn console(bench_type: &str, rows: Vec<(&str, String)>) -> String {
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max("BenchType".len());
    let mut out = String::new();
    for (i, (name, value)) in rows.iter().enumerate() {
        if *name == "Framework" {
            out += &format!("{:<width$}: {bench_type}\n", "BenchType");
        }
        out += &format!("{name:<width$}: {value}");
        if i + 1 != rows.len() {
            out.push('\n');
        }
    }
    out
}

fn latency_rows(s: &Summary, tpn: bool) -> Vec<(&'static str, String)> {
    if !tpn {
        return Vec::new();
    }
    vec![
        ("Min", time_string(s.min)),
        ("Avg", time_string(s.avg)),
        ("Max", time_string(s.max)),
        ("TP95", time_string(s.tp95)),
        ("TP99", time_string(s.tp99)),
    ]
}

fn ps_rows(ps: &PsStats) -> Vec<(&'static str, String)> {
    vec![
        ("CPU Avg", format!("{:.2}%", ps.cpu_avg)),
        ("CPU Max", format!("{:.2}%", ps.cpu_max)),
        ("MEM Avg", mem_string(ps.mem_avg)),
        ("MEM Max", mem_string(ps.mem_max)),
    ]
}

impl ConnectionsReport {
    pub fn console(&self, s: &Summary, tpn: bool) -> String {
        let mut rows = vec![
            ("Framework", self.framework.clone()),
            ("Client", client_name()),
            ("Threads", self.threads.to_string()),
            ("TPS", self.tps.to_string()),
        ];
        rows.extend(latency_rows(s, tpn));
        rows.extend([
            ("Used", time_string(self.used)),
            ("Total", self.total.to_string()),
            ("Success", self.success.to_string()),
            ("Failed", self.failed.to_string()),
            ("Concurrency", self.concurrency.to_string()),
        ]);
        console("Connections", rows)
    }
}

impl BenchEchoReport {
    pub fn console(&self, s: &Summary, tpn: bool) -> String {
        let mut rows = vec![
            ("Framework", self.framework.clone()),
            ("Client", client_name()),
            ("Threads", self.threads.to_string()),
            ("TPS", self.tps.to_string()),
            ("CPU EER", format!("{:.2}", self.cpu_eer)),
            ("MEM EER", format!("{:.2}", self.mem_eer)),
        ];
        rows.extend(latency_rows(s, tpn));
        rows.extend([
            ("Used", time_string(self.used)),
            ("Total", self.total.to_string()),
            ("Success", self.success.to_string()),
            ("Failed", self.failed.to_string()),
            ("Conns", self.connections.to_string()),
            ("Concurrency", self.concurrency.to_string()),
            ("Payload", self.payload.to_string()),
        ]);
        rows.extend(ps_rows(&self.ps));
        console("BenchEcho", rows)
    }
}

impl BenchRateReport {
    pub fn console(&self) -> String {
        let mut rows = vec![
            ("Framework", self.framework.clone()),
            ("Client", client_name()),
            ("Threads", self.threads.to_string()),
            ("Duration", time_string(self.duration)),
            ("TPS", self.tps.to_string()),
            ("CPU EER", format!("{:.2}", self.cpu_eer)),
            ("MEM EER", format!("{:.2}", self.mem_eer)),
            ("Req Sent", self.send_times.to_string()),
            ("Bytes Sent", mem_string(self.send_bytes)),
            ("Resp Recv", self.recv_times.to_string()),
            ("Bytes Recv", mem_string(self.recv_bytes)),
            ("Conns", self.connections.to_string()),
            ("SendRate", self.send_rate.to_string()),
            ("Batch", self.batch.to_string()),
            ("Payload", self.payload.to_string()),
        ];
        rows.extend(ps_rows(&self.ps));
        console(BENCH_MULTIPLEX, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_names() {
        let r = BenchRateReport {
            framework: "fib".into(),
            bench_client: CLIENT_NAME.into(),
            threads: 4,
            duration: 10_000_000_000,
            tps: 1,
            cpu_eer: 2.5,
            mem_eer: 1.5,
            send_times: 3,
            send_bytes: 4,
            recv_times: 5,
            recv_bytes: 6,
            connections: 7,
            send_rate: 8,
            batch: 9,
            payload: 10,
            ps: PsStats { cpu_min: 1.0, cpu_avg: 2.0, cpu_max: 3.0, mem_min: 4, mem_avg: 5, mem_max: 6 },
            pprof_on: true,
            pprof: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        for key in [
            r#""Framework":"fib""#,
            r#""BenchClient":"benchcli-rust""#,
            r#""CPUEER":2.5"#,
            r#""MEMEER":1.5"#,
            r#""Conns":7"#,
            r#""Batch":9"#,
            r#""CPUAvg":2.0"#,
            r#""MEMMax":6"#,
            r#""Pprof":true"#,
        ] {
            assert!(json.contains(key), "{key} not in {json}");
        }
        assert_eq!(cpu_eer(10.0, 0.0), 0.0);
        assert_eq!(cpu_eer(10.0, 4.0), 2.5);
        assert_eq!(mem_eer(10.0, 0), 0.0);
        assert_eq!(mem_eer(10.0, 4 * 1024 * 1024), 2.5);
    }
}
