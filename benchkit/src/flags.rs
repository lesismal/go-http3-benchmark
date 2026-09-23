//! A command line read the way Go's flag package reads one, since the scripts
//! drive this client and the Go servers with the same flags: `-name=value`,
//! `-name value`, `--name=value`, and a bare `-name` for a boolean. The last
//! value of a repeated flag wins, which the scripts rely on to let a flag
//! given on the command line override one they put in front of it.

use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone)]
enum Kind {
    Bool,
    Int,
    Str,
    Duration,
}

struct Flag {
    kind: Kind,
    default: String,
    usage: String,
    value: Option<String>,
}

#[derive(Default)]
pub struct FlagSet {
    flags: BTreeMap<String, Flag>,
}

impl FlagSet {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, name: &str, kind: Kind, default: String, usage: &str) {
        self.flags.insert(
            name.to_string(),
            Flag { kind, default, usage: usage.to_string(), value: None },
        );
    }

    pub fn bool(&mut self, name: &str, default: bool, usage: &str) {
        self.add(name, Kind::Bool, default.to_string(), usage);
    }

    pub fn int(&mut self, name: &str, default: i64, usage: &str) {
        self.add(name, Kind::Int, default.to_string(), usage);
    }

    pub fn string(&mut self, name: &str, default: &str, usage: &str) {
        self.add(name, Kind::Str, default.to_string(), usage);
    }

    pub fn duration(&mut self, name: &str, default: &str, usage: &str) {
        self.add(name, Kind::Duration, default.to_string(), usage);
    }

    /// Parses args, which do not include the program name. An error is the
    /// message to print before the usage.
    pub fn parse(&mut self, args: &[String]) -> Result<(), String> {
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            i += 1;
            if !arg.starts_with('-') || arg == "-" {
                return Err(format!("unexpected argument {arg:?}"));
            }
            let body = arg.trim_start_matches('-');
            if body.is_empty() {
                // "--" ends the flags, as it does for Go.
                if i < args.len() {
                    return Err(format!("unexpected argument {:?}", args[i]));
                }
                break;
            }
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            if name == "h" || name == "help" {
                return Err(String::new());
            }
            let flag = self
                .flags
                .get_mut(name)
                .ok_or_else(|| format!("flag provided but not defined: -{name}"))?;
            let value = match (inline, &flag.kind) {
                (Some(v), _) => v,
                (None, Kind::Bool) => "true".to_string(),
                (None, _) => {
                    if i >= args.len() {
                        return Err(format!("flag needs an argument: -{name}"));
                    }
                    i += 1;
                    args[i - 1].clone()
                }
            };
            check(name, &flag.kind, &value)?;
            flag.value = Some(value);
        }
        Ok(())
    }

    fn raw(&self, name: &str) -> &str {
        let flag = self.flags.get(name).unwrap_or_else(|| panic!("flag -{name} is not defined"));
        flag.value.as_deref().unwrap_or(&flag.default)
    }

    pub fn get_bool(&self, name: &str) -> bool {
        parse_bool(self.raw(name)).unwrap_or(false)
    }

    pub fn get_int(&self, name: &str) -> i64 {
        self.raw(name).parse().unwrap_or(0)
    }

    pub fn get_str(&self, name: &str) -> String {
        self.raw(name).to_string()
    }

    pub fn get_duration(&self, name: &str) -> Duration {
        parse_duration(self.raw(name)).unwrap_or_default()
    }

    pub fn usage(&self, program: &str) -> String {
        let mut out = format!("Usage of {program}:\n");
        for (name, flag) in &self.flags {
            let kind = match flag.kind {
                Kind::Bool => "",
                Kind::Int => " int",
                Kind::Str => " string",
                Kind::Duration => " duration",
            };
            out += &format!("  -{name}{kind}\n    \t{} (default {:?})\n", flag.usage, flag.default);
        }
        out
    }
}

fn check(name: &str, kind: &Kind, value: &str) -> Result<(), String> {
    let ok = match kind {
        Kind::Bool => parse_bool(value).is_some(),
        Kind::Int => value.parse::<i64>().is_ok(),
        Kind::Str => true,
        Kind::Duration => parse_duration(value).is_some(),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("invalid value {value:?} for flag -{name}"))
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Parses a duration the way Go's time.ParseDuration does, for the units a
/// command line uses: "5s", "100ms", "1m30s", "1.5s". A bare "0" is zero.
pub fn parse_duration(s: &str) -> Option<Duration> {
    if s == "0" {
        return Some(Duration::ZERO);
    }
    let mut rest = s;
    let mut total = 0f64;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        if num_len == 0 {
            return None;
        }
        let number: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let scale = match &rest[..unit_len] {
            "ns" => 1e-9,
            "us" | "µs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        rest = &rest[unit_len..];
        total += number * scale;
    }
    Some(Duration::from_secs_f64(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> FlagSet {
        let mut f = FlagSet::new();
        f.int("c", 10, "");
        f.bool("check", false, "");
        f.string("f", "quicgo", "");
        f.duration("dt", "5s", "");
        f
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn go_style() {
        let mut f = set();
        f.parse(&args(&["-c=3", "--f", "fib", "-check", "-dt=1m30s", "-c=4"])).unwrap();
        assert_eq!(f.get_int("c"), 4);
        assert_eq!(f.get_str("f"), "fib");
        assert!(f.get_bool("check"));
        assert_eq!(f.get_duration("dt"), Duration::from_secs(90));
    }

    #[test]
    fn defaults_and_errors() {
        let mut f = set();
        f.parse(&[]).unwrap();
        assert_eq!(f.get_int("c"), 10);
        assert_eq!(f.get_duration("dt"), Duration::from_secs(5));
        assert!(set().parse(&args(&["-nope=1"])).is_err());
        assert!(set().parse(&args(&["-c=x"])).is_err());
        assert!(set().parse(&args(&["-check=maybe"])).is_err());
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("5"), None);
    }
}
