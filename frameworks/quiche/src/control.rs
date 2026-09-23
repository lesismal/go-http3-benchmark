//! The control routes every server serves on the port after its last
//! benchmark port: /init starts the server sampling its own CPU and memory
//! and answers with its pid, and /ps answers with the samples, in the JSON
//! github.com/lesismal/perf's PSCounter marshals to - {"cpu": [...], "mem":
//! [{"rss": ...}, ...]} - so that the client reads it the way it reads a Go
//! server's. Plain HTTP/1 on a thread of its own, away from the benchmark.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use benchkit::procstat::Sampler;

static SAMPLER: OnceLock<Mutex<Option<Sampler>>> = OnceLock::new();

pub fn start(port: u16) {
    let listener = TcpListener::bind(format!("[::]:{port}"))
        .or_else(|_| TcpListener::bind(format!("0.0.0.0:{port}")))
        .unwrap_or_else(|e| fatalf!("control server on :{port}: {e}"));
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream) {
                        logf!("control request failed: {e}");
                    }
                });
            }
        })
        .expect("spawning the control server");
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((name, value)) = h.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length.min(1 << 20)];
    reader.read_exact(&mut body)?;

    let (status, reply) = match (method.as_str(), path.as_str()) {
        (_, "/init") => (200, init(&body)),
        (_, "/ps") => (200, ps()),
        _ => (404, "404 page not found\n".to_string()),
    };
    let text = if status == 200 { "OK" } else { "Not Found" };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status} {text}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )?;
    stream.flush()
}

#[derive(serde::Deserialize, Default)]
struct InitArgs {
    #[serde(rename = "PsInterval", default)]
    ps_interval: u64,
}

/// Starts sampling once, however many times /init arrives: a client that
/// retried the request can deliver it twice.
fn init(body: &[u8]) -> String {
    let args: InitArgs = serde_json::from_slice(body).unwrap_or_default();
    let pid = std::process::id() as i32;
    let cell = SAMPLER.get_or_init(|| Mutex::new(None));
    let mut sampler = cell.lock().unwrap();
    if sampler.is_none() {
        match Sampler::start(pid, Duration::from_nanos(args.ps_interval)) {
            Ok(s) => *sampler = Some(s),
            Err(e) => logf!("starting the ps sampler failed: {e}"),
        }
    } else {
        logf!("/init called again; the ps counter is already running");
    }
    pid.to_string()
}

fn ps() -> String {
    let (cpu, rss) = match SAMPLER.get().and_then(|m| m.lock().unwrap().as_ref().map(|s| s.since(0, 0))) {
        Some(v) => v,
        None => (Vec::new(), Vec::new()),
    };
    let mem: Vec<serde_json::Value> = rss.into_iter().map(|r| serde_json::json!({ "rss": r })).collect();
    serde_json::json!({ "cpu": cpu, "mem": mem }).to_string()
}
