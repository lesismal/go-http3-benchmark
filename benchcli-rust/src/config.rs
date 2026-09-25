//! The frameworks and their ports: the same table as config/config.go, which
//! the Go servers are built from, and frameworks/quiche/src/main.rs. Kept in
//! framework-name order like every other framework list in the repository.
//! config.TestPortsMatch holds this table to the Go one.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};

/// Name, first and last benchmark port, and whether the server is a Go
/// program with pprof routes on its control port.
pub const FRAMEWORKS: &[(&str, u16, u16, bool)] = &[
    ("fib", 3001, 3050, true),
    ("gin", 3101, 3150, true),
    ("quicgo", 3201, 3250, true),
    ("quiche", 3301, 3350, false),
];

pub const ECHO_PATH: &str = "/echo";

pub fn framework_names() -> Vec<&'static str> {
    FRAMEWORKS.iter().map(|f| f.0).collect()
}

fn ports(framework: &str) -> Option<(u16, u16)> {
    FRAMEWORKS
        .iter()
        .find(|f| f.0 == framework)
        .map(|f| (f.1, f.2))
}

pub fn is_framework(framework: &str) -> bool {
    ports(framework).is_some()
}

fn bare_host(ip: &str) -> &str {
    ip.trim_start_matches('[').trim_end_matches(']')
}

/// The UDP addresses the client dials for each of the framework's benchmark
/// ports, resolved once.
pub fn benchmark_addrs(framework: &str, ip: &str) -> Result<Vec<SocketAddr>, String> {
    let (min, max) = ports(framework).ok_or_else(|| format!("unknown framework {framework:?}"))?;
    let host = bare_host(ip);
    let base = (host, min)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}: no address"))?;
    Ok((min..=max)
        .map(|port| SocketAddr::new(base.ip(), port))
        .collect())
}

/// Where the framework's control routes - /init, /ps and pprof - are: TCP, on
/// the port after its last benchmark port.
pub fn control_addr(framework: &str, ip: &str) -> Result<SocketAddr, String> {
    let (_, max) = ports(framework).ok_or_else(|| format!("unknown framework {framework:?}"))?;
    let host = bare_host(ip);
    (host, max + 1)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}: no address"))
}

/// The :authority the client sends, for a server reached as ip: an IPv6
/// address in brackets, as it would be in a URL.
pub fn authority(ip: &str) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]")
    } else {
        ip.to_string()
    }
}

/// Whether the framework's server serves Go's pprof routes, which the client
/// profiles it through with -ep and -rp.
pub fn serves_pprof(framework: &str) -> bool {
    FRAMEWORKS.iter().any(|f| f.0 == framework && f.3)
}

/// Whether a host the client dials is this machine: loopback, unspecified,
/// or an address one of this machine's interfaces carries, which is the one
/// kind of address a socket can bind to.
pub fn is_local_host(ip: &str) -> bool {
    let host = bare_host(ip);
    if host.is_empty() {
        return false;
    }
    let ips: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match (host, 0).to_socket_addrs() {
            Ok(addrs) => addrs.map(|a| a.ip()).collect(),
            Err(_) => return false,
        },
    };
    ips.into_iter()
        .any(|ip| ip.is_loopback() || ip.is_unspecified() || UdpSocket::bind((ip, 0)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addrs() {
        let a = benchmark_addrs("quicgo", "::1").unwrap();
        assert_eq!(a.len(), 50);
        assert_eq!(a[0].to_string(), "[::1]:3201");
        assert_eq!(a[49].to_string(), "[::1]:3250");
        assert_eq!(
            benchmark_addrs("quiche", "::1").unwrap()[0].to_string(),
            "[::1]:3301"
        );
        assert!(serves_pprof("fib") && !serves_pprof("quiche"));
        assert_eq!(
            benchmark_addrs("fib", "[::1]").unwrap()[0].to_string(),
            "[::1]:3001"
        );
        assert_eq!(
            control_addr("fib", "127.0.0.1").unwrap().to_string(),
            "127.0.0.1:3051"
        );
        assert!(benchmark_addrs("gorilla", "127.0.0.1").is_err());
        assert_eq!(authority("::1"), "[::1]");
        assert_eq!(authority("10.0.0.2"), "10.0.0.2");
    }

    #[test]
    fn local() {
        for h in ["127.0.0.1", "::1", "[::1]", "localhost", "0.0.0.0"] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["", "192.0.2.10", "198.51.100.7", "2001:db8::1"] {
            assert!(!is_local_host(h), "{h}");
        }
    }

    #[test]
    fn sorted() {
        let names = framework_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
