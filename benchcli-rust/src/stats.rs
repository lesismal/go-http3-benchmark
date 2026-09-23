//! What github.com/lesismal/perf's Calculator works out of a benchmark's
//! latencies, computed the same way, so that the numbers in this client's
//! reports mean what they mean in the Go benchmarks': TPS is successes per
//! second of the whole run, Avg is the mean of the successes, and TPn is the
//! latency at index floor(n/100 * successes) of the sorted successes.

use std::collections::HashMap;
use std::time::Duration;

#[derive(Default)]
pub struct Latencies {
    /// One entry per successful operation, in nanoseconds.
    pub costs: Vec<i64>,
    pub success: i64,
    pub failed: i64,
    pub errors: HashMap<String, usize>,
}

impl Latencies {
    pub fn ok(&mut self, cost: Duration) {
        self.costs.push(cost.as_nanos() as i64);
        self.success += 1;
    }

    pub fn fail(&mut self, err: impl Into<String>) {
        self.failed += 1;
        *self.errors.entry(err.into()).or_default() += 1;
    }

    pub fn merge(&mut self, other: Latencies) {
        self.costs.extend(other.costs);
        self.success += other.success;
        self.failed += other.failed;
        for (k, v) in other.errors {
            *self.errors.entry(k).or_default() += v;
        }
    }
}

pub struct Summary {
    pub tps: i64,
    pub min: i64,
    pub avg: i64,
    pub max: i64,
    pub tp50: i64,
    pub tp75: i64,
    pub tp90: i64,
    pub tp95: i64,
    pub tp99: i64,
}

pub fn summarize(l: &mut Latencies, used: Duration) -> Summary {
    l.costs.sort_unstable();
    let costs = &l.costs;
    let tpn = |p: usize| -> i64 {
        if costs.is_empty() {
            return 0;
        }
        let idx = ((p as f64 / 100.0) * costs.len() as f64) as usize;
        costs[idx.min(costs.len() - 1)]
    };
    let avg = if costs.is_empty() {
        0
    } else {
        (costs.iter().map(|&c| c as i128).sum::<i128>() / costs.len() as i128) as i64
    };
    let secs = used.as_secs_f64();
    Summary {
        tps: if secs > 0.0 { (l.success as f64 / secs) as i64 } else { 0 },
        min: costs.first().copied().unwrap_or(0),
        avg,
        max: costs.last().copied().unwrap_or(0),
        tp50: tpn(50),
        tp75: tpn(75),
        tp90: tpn(90),
        tp95: tpn(95),
        tp99: tpn(99),
    }
}

/// perf.I2TimeString: "1.23s", "4.56ms", "7.89us" or "12ns".
pub fn time_string(ns: i64) -> String {
    if ns / 1_000_000_000 >= 1 {
        format!("{:.2}s", ns as f64 / 1e9)
    } else if ns / 1_000_000 >= 1 {
        format!("{:.2}ms", ns as f64 / 1e6)
    } else if ns / 1_000 >= 1 {
        format!("{:.2}us", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}

/// perf.I2MemString: "1.23G", "4.56M" or "7.89K".
pub fn mem_string(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b / GB >= 1.0 {
        format!("{:.2}G", b / GB)
    } else if b / MB >= 1.0 {
        format!("{:.2}M", b / MB)
    } else {
        format!("{:.2}K", b / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut l = Latencies::default();
        for i in (1..=100).rev() {
            l.ok(Duration::from_nanos(i));
        }
        l.fail("x");
        let s = summarize(&mut l, Duration::from_secs(2));
        assert_eq!((s.min, s.max, s.avg), (1, 100, 50));
        assert_eq!((s.tp50, s.tp90, s.tp99), (51, 91, 100));
        assert_eq!(s.tps, 50);
        assert_eq!(l.errors["x"], 1);
    }

    #[test]
    fn strings() {
        assert_eq!(time_string(1_500_000_000), "1.50s");
        assert_eq!(time_string(2_500_000), "2.50ms");
        assert_eq!(time_string(9_040), "9.04us");
        assert_eq!(time_string(12), "12ns");
        assert_eq!(mem_string(193 * 1024 * 1024), "193.00M");
    }
}
