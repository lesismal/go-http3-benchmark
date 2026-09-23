//! The cloudflare/quiche HTTP/3 server: quiche's own QUIC and HTTP/3
//! (quiche::h3) over mio, answering POST /echo with the request body, byte for
//! byte, with a Content-Length, as the Go servers do.
//!
//! quiche does no I/O and runs no threads of its own, so what is quiche's
//! here is the protocol; the event loop around it is the smallest one that
//! serves many connections on many cores. Each worker thread owns an equal
//! share of the fifty benchmark ports, a UDP socket each, and every
//! connection that arrives on them: a connection never moves between threads,
//! and threads share nothing. The client spreads its connections over the
//! ports evenly, so the shares carry about the same load.
//!
//! It takes the same flags as the Go servers, and serves /init and /ps on a
//! TCP control port the way they do; pprof is Go's, and is not there.

#[macro_use]
extern crate benchkit;

mod control;
mod server;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use benchkit::flags::FlagSet;

const FRAMEWORK: &str = "quiche";
/// The benchmark ports, as config/config.go and benchcli-rust list them;
/// the control port is the one after the last.
const PORTS: (u16, u16) = (13001, 13050);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub struct Settings {
    pub payload: usize,
    pub streams: u64,
    pub idle: Duration,
}

fn main() {
    let mut f = FlagSet::new();
    // The flags script/benchmark.sh forwards to every server.
    f.int("b", 1024, "expected request body size, which sizes the servers' read buffers");
    f.int("m", 2 << 30, "memory limit: the Go servers' GOMEMLIMIT, which a Rust server has no counterpart of");
    f.int("streams", 100, "max concurrent request streams per connection");
    f.duration("idle", "120s", "QUIC max idle timeout");
    f.int("t", 0, "worker threads, 0 for one per CPU this process may run on");
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = f.parse(&args[1..]) {
        if !e.is_empty() {
            eprintln!("{e}");
        }
        eprint!("{}", f.usage(&args[0]));
        std::process::exit(2);
    }
    let settings = Settings {
        payload: f.get_int("b").max(1) as usize,
        streams: f.get_int("streams").max(1) as u64,
        idle: f.get_duration("idle"),
    };
    logf!(
        "{FRAMEWORK} server: payload={}, streams={}, idle={:?}, memory limit={} (not applied)",
        settings.payload,
        settings.streams,
        settings.idle,
        f.get_int("m")
    );

    control::start(PORTS.1 + 1);

    let ports: Vec<u16> = (PORTS.0..=PORTS.1).collect();
    let threads = match f.get_int("t") {
        n if n > 0 => n as usize,
        _ => std::thread::available_parallelism().map_or(1, |n| n.get()),
    }
    .min(ports.len());

    let (cert, key) = match server::certificate_files() {
        Ok(v) => v,
        Err(e) => fatalf!("generating the TLS certificate failed: {e}"),
    };
    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        // Thread t takes ports t, t+threads, t+2*threads and so on.
        let mine: Vec<u16> = ports.iter().copied().skip(t).step_by(threads).collect();
        let sockets: Vec<UdpSocket> = mine.iter().map(|&p| bind(p)).collect();
        let config = match server::quiche_config(&settings, &cert, &key) {
            Ok(c) => c,
            Err(e) => fatalf!("quiche config: {e}"),
        };
        let payload = settings.payload;
        handles.push(
            std::thread::Builder::new()
                .name(format!("worker-{t}"))
                .spawn(move || server::Worker::new(sockets, config, payload).run())
                .expect("spawning a worker"),
        );
    }
    // Every config has read them.
    let _ = std::fs::remove_file(&cert);
    let _ = std::fs::remove_file(&key);
    logf!(
        "{FRAMEWORK} server: listening on {} UDP ports, :{} to :{}, {threads} threads",
        ports.len(),
        PORTS.0,
        PORTS.1
    );

    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    while !INTERRUPTED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    logf!("{FRAMEWORK} server: exit");
    std::process::exit(0);
}

/// Binds every interface, both families where the kernel has IPv6, as the
/// Go servers' ":port" does.
fn bind(port: u16) -> UdpSocket {
    let v6: SocketAddr = format!("[::]:{port}").parse().unwrap();
    let v4: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
    match UdpSocket::bind(v6).or_else(|_| UdpSocket::bind(v4)) {
        Ok(s) => s,
        Err(e) => fatalf!("listen :{port} failed: {e}"),
    }
}
