//! Log lines in the Go side's format: "20060102 15:04.05.000 message", on
//! stderr, so that the client's log reads like the servers' and the report
//! step's.

use std::io::Write;

pub const SHORT_LINE: &str = "--------------------------------------------------------------\n";
pub const LONG_LINE: &str =
    "----------------------------------------------------------------------------------------------------\n";

pub fn now_string() -> String {
    chrono::Local::now().format("%Y%m%d %H:%M.%S%.3f").to_string()
}

pub fn print(s: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(s.as_bytes());
}

#[macro_export]
macro_rules! logf {
    ($($arg:tt)*) => {
        $crate::logging::print(&format!("{} {}\n", $crate::logging::now_string(), format!($($arg)*)))
    };
}

#[macro_export]
macro_rules! fatalf {
    ($($arg:tt)*) => {{
        $crate::logf!($($arg)*);
        std::process::exit(1)
    }};
}
