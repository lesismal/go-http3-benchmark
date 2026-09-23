//! The load generator: worker threads, each an event loop over its own share
//! of the connections.
//!
//! Every QUIC connection has a UDP socket of its own, connected to one of the
//! framework's fifty ports. That is what a real client population looks like
//! to a server, and it is also what fib needs, since it tells its connections
//! apart by the peer's address rather than by connection ID. Each worker
//! polls its sockets with mio, drives quiche's timers itself, and runs
//! whatever benchmark the main thread last sent it; a benchmark ends for the
//! main thread when every worker has reported back.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};
use quiche::h3::NameValue;

use crate::stats::Latencies;

const MAX_DATAGRAM_SIZE: usize = 1350;
const WAKER: Token = Token(usize::MAX);
/// How many batches a connection may have unanswered in BenchMultiplex
/// before it is skipped for a tick, as in the Go benchmarks' rate test.
pub const MAX_BATCHES_IN_FLIGHT: u64 = 4;
/// H3_NO_ERROR, what the client closes its connections with at the end.
const H3_NO_ERROR: u64 = 0x100;

pub struct EngineConfig {
    pub addrs: Vec<SocketAddr>,
    pub authority: String,
    pub dial_timeout: Duration,
    pub dial_retries: usize,
    pub dial_retry_interval: Duration,
    pub idle_timeout: Duration,
    pub check: bool,
}

/// A token bucket, shared by the workers, for -el and -rl.
pub struct Limiter {
    rate: f64,
    state: Mutex<(f64, Instant)>,
}

impl Limiter {
    pub fn new(per_second: usize) -> Self {
        Self { rate: per_second as f64, state: Mutex::new((per_second as f64, Instant::now())) }
    }

    /// Takes n tokens, or says how long until there will be enough.
    fn take(&self, n: usize) -> Result<(), Duration> {
        let mut st = self.state.lock().unwrap();
        let now = Instant::now();
        st.0 = (st.0 + now.duration_since(st.1).as_secs_f64() * self.rate).min(self.rate.max(n as f64));
        st.1 = now;
        if st.0 >= n as f64 {
            st.0 -= n as f64;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((n as f64 - st.0) / self.rate))
        }
    }
}

pub struct EchoJob {
    pub times: u64,
    pub issued: AtomicU64,
    pub payloads: Vec<Arc<Vec<u8>>>,
    /// Whether latencies and failures are recorded: false for the warmup.
    pub measure: bool,
    pub limiter: Option<Limiter>,
}

pub struct RateJob {
    pub duration: Duration,
    pub batch: usize,
    pub tick: Duration,
    pub payload: Arc<Vec<u8>>,
    pub limiter: Option<Limiter>,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct RateCounts {
    pub send_times: u64,
    pub send_bytes: u64,
    pub recv_times: u64,
    pub recv_bytes: u64,
    pub answered: u64,
}

pub enum Command {
    Dial { count: usize, concurrency: usize },
    Echo { job: Arc<EchoJob>, concurrency: usize },
    Rate(Arc<RateJob>),
    Stop,
}

pub enum Outcome {
    Dial(Latencies),
    Echo(Latencies),
    Rate(RateCounts),
}

struct WorkerHandle {
    tx: Sender<Command>,
    waker: Arc<Waker>,
    thread: Option<JoinHandle<()>>,
    ready: Arc<AtomicUsize>,
}

pub struct Engine {
    workers: Vec<WorkerHandle>,
    results: Receiver<(usize, Outcome)>,
    /// Connections established and answered so far, over every worker, for
    /// the progress lines of the Connections benchmark.
    pub connected: Arc<AtomicI64>,
}

impl Engine {
    pub fn new(threads: usize, cfg: EngineConfig) -> Result<Self, String> {
        let cfg = Arc::new(cfg);
        let (rtx, results) = channel();
        let connected = Arc::new(AtomicI64::new(0));
        let addr_counter = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::with_capacity(threads);
        for id in 0..threads {
            let poll = Poll::new().map_err(|e| e.to_string())?;
            let waker = Arc::new(Waker::new(poll.registry(), WAKER).map_err(|e| e.to_string())?);
            let (tx, rx) = channel();
            let ready = Arc::new(AtomicUsize::new(0));
            let (cfg, rtx, connected, addr_counter, r) =
                (cfg.clone(), rtx.clone(), connected.clone(), addr_counter.clone(), ready.clone());
            let thread = std::thread::Builder::new()
                .name(format!("worker-{id}"))
                .spawn(move || match Worker::new(id, cfg, poll, rx, rtx, connected, addr_counter, r) {
                    Ok(mut w) => w.run(),
                    Err(e) => fatalf!("worker {id}: {e}"),
                })
                .map_err(|e| e.to_string())?;
            workers.push(WorkerHandle { tx, waker, thread: Some(thread), ready });
        }
        Ok(Self { workers, results, connected })
    }

    pub fn threads(&self) -> usize {
        self.workers.len()
    }

    /// The connections each worker holds that are up and serving.
    pub fn ready_counts(&self) -> Vec<usize> {
        self.workers.iter().map(|w| w.ready.load(Ordering::Relaxed)).collect()
    }

    /// Sends one command to each worker and waits for every outcome, calling
    /// tick once a second while it waits. Outcomes come back in worker order.
    pub fn run(&self, commands: Vec<Command>, mut tick: impl FnMut(u64)) -> Vec<Outcome> {
        assert_eq!(commands.len(), self.workers.len());
        for (w, cmd) in self.workers.iter().zip(commands) {
            let _ = w.tx.send(cmd);
            let _ = w.waker.wake();
        }
        let mut outcomes: Vec<Option<Outcome>> = (0..self.workers.len()).map(|_| None).collect();
        let mut pending = self.workers.len();
        let mut next = Instant::now() + Duration::from_secs(1);
        let mut seconds = 0;
        while pending > 0 {
            match self.results.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok((id, outcome)) => {
                    outcomes[id] = Some(outcome);
                    pending -= 1;
                }
                Err(RecvTimeoutError::Timeout) => {
                    seconds += 1;
                    tick(seconds);
                    next += Duration::from_secs(1);
                }
                Err(RecvTimeoutError::Disconnected) => fatalf!("a worker exited"),
            }
        }
        outcomes.into_iter().map(|o| o.unwrap()).collect()
    }

    /// Closes every connection and stops the workers.
    pub fn stop(mut self) {
        for w in &self.workers {
            let _ = w.tx.send(Command::Stop);
            let _ = w.waker.wake();
        }
        for w in &mut self.workers {
            if let Some(t) = w.thread.take() {
                let _ = t.join();
            }
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum State {
    /// Handshaking, or waiting for the answer to its first request.
    Connecting,
    Ready,
    /// Closed or failed; it is dialed again before it is used again.
    Broken,
}

#[derive(Clone, Copy)]
enum Purpose {
    /// A connection of the Connections benchmark.
    New,
    /// A broken connection dialed again for a BenchEcho round trip that
    /// started at start.
    RedialEcho { start: Instant },
    /// A broken connection dialed again before BenchMultiplex starts.
    RedialRate,
}

#[derive(Clone, Copy)]
struct DialJob {
    start: Instant,
    attempt: usize,
    purpose: Purpose,
    /// The slot the connection is dialed into again, for a redial.
    slot: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReqKind {
    Dial,
    Echo { start: Instant },
    Rate,
}

struct Req {
    kind: ReqKind,
    expect: Arc<Vec<u8>>,
    status: u16,
    got: usize,
    mismatch: bool,
}

struct PendingBody {
    stream: u64,
    body: Arc<Vec<u8>>,
    off: usize,
}

struct Conn {
    sock: UdpSocket,
    q: quiche::Connection,
    h3: Option<quiche::h3::Connection>,
    local: SocketAddr,
    peer: SocketAddr,
    state: State,
    dial: Option<DialJob>,
    attempt_deadline: Option<Instant>,
    streams: HashMap<u64, Req>,
    pending: Vec<PendingBody>,
    queued: VecDeque<(ReqKind, Arc<Vec<u8>>)>,
    deadline: Option<Instant>,
    scheduled: Option<Instant>,
    last_active: Instant,
    dirty: bool,
    in_flight: u64,
    goaway: bool,
    /// A send or receive failed at the socket, which on a connected UDP
    /// socket means the port answered with ICMP unreachable.
    socket_error: Option<String>,
}

enum SendError {
    /// No room on the connection yet: flow control, the congestion window or
    /// the server's stream limit.
    Blocked(String),
    Failed(String),
}

enum ConnEvent {
    Established,
    Response { kind: ReqKind, result: Result<usize, String> },
    Closed(String),
}

enum Phase {
    Idle,
    Dial { remaining: usize, active: usize, concurrency: usize, lat: Latencies },
    Echo {
        job: Arc<EchoJob>,
        slots: usize,
        in_flight: usize,
        idle: VecDeque<usize>,
        exhausted: bool,
        wait_until: Option<Instant>,
        lat: Latencies,
    },
    RatePrep { job: Arc<RateJob>, redials: usize },
    Rate {
        job: Arc<RateJob>,
        conns: Vec<usize>,
        deadline: Instant,
        next_tick: Instant,
        grace_until: Option<Instant>,
        counts: RateCounts,
    },
}

struct Worker {
    id: usize,
    cfg: Arc<EngineConfig>,
    poll: Poll,
    rx: Receiver<Command>,
    results: Sender<(usize, Outcome)>,
    connected: Arc<AtomicI64>,
    addr_counter: Arc<AtomicUsize>,
    ready: Arc<AtomicUsize>,

    qconfig: quiche::Config,
    h3config: quiche::h3::Config,
    server_name: Option<String>,
    get_headers: Vec<quiche::h3::Header>,
    post_headers: Vec<quiche::h3::Header>,
    post_len: usize,
    empty: Arc<Vec<u8>>,

    slots: Vec<Option<Conn>>,
    free: Vec<usize>,
    timers: BinaryHeap<Reverse<(Instant, usize)>>,
    retries: VecDeque<(Instant, DialJob)>,
    dirty: Vec<usize>,
    phase: Phase,
    next_keepalive: Instant,
    keepalive: Duration,

    buf: Vec<u8>,
    out: Vec<u8>,
    dropped: u64,
}

fn new_quiche_config(idle: Duration) -> Result<quiche::Config, String> {
    let mut c = quiche::Config::new(quiche::PROTOCOL_VERSION).map_err(|e| e.to_string())?;
    c.verify_peer(false);
    c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL).map_err(|e| e.to_string())?;
    c.set_max_idle_timeout(idle.as_millis() as u64);
    c.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
    c.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
    // Receive windows for the responses, which quiche only allocates as data
    // arrives: generous next to what a batch of echoes needs, so that flow
    // control never paces a server.
    c.set_initial_max_data(16 * 1024 * 1024);
    c.set_initial_max_stream_data_bidi_local(1024 * 1024);
    c.set_initial_max_stream_data_bidi_remote(256 * 1024);
    c.set_initial_max_stream_data_uni(256 * 1024);
    // The server opens no request streams, only its control and QPACK ones.
    c.set_initial_max_streams_bidi(16);
    c.set_initial_max_streams_uni(16);
    c.set_disable_active_migration(true);
    // No GREASE: the extra stream, settings and frames it adds to every
    // connection are work for the server that measures nothing.
    c.grease(false);
    Ok(c)
}

fn header(name: &str, value: &str) -> quiche::h3::Header {
    quiche::h3::Header::new(name.as_bytes(), value.as_bytes())
}

fn request_headers(method: &str, authority: &str, len: Option<usize>) -> Vec<quiche::h3::Header> {
    let mut h = vec![
        header(":method", method),
        header(":scheme", "https"),
        header(":authority", authority),
        header(":path", crate::config::ECHO_PATH),
    ];
    if let Some(len) = len {
        h.push(header("content-type", "application/octet-stream"));
        h.push(header("content-length", &len.to_string()));
    }
    h
}

fn set_socket_buffers(sock: &UdpSocket) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // macOS starts a UDP socket with a 9KB send buffer, which one batch
        // of echoes overflows; Linux caps what is asked at rmem_max and
        // wmem_max. Memory is only taken as datagrams wait in it.
        let size: libc::c_int = 1 << 20;
        for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::SOL_SOCKET,
                    opt,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }
    }
}

fn would_block(e: &std::io::Error) -> bool {
    // ENOBUFS is what a full UDP send buffer says on macOS.
    e.kind() == ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::ENOBUFS)
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: usize, cfg: Arc<EngineConfig>, poll: Poll, rx: Receiver<Command>,
        results: Sender<(usize, Outcome)>, connected: Arc<AtomicI64>, addr_counter: Arc<AtomicUsize>,
        ready: Arc<AtomicUsize>,
    ) -> Result<Self, String> {
        let qconfig = new_quiche_config(cfg.idle_timeout)?;
        let h3config = quiche::h3::Config::new().map_err(|e| e.to_string())?;
        // SNI only for a host name; an address is not one.
        let server_name = if cfg.authority.parse::<std::net::IpAddr>().is_err() && !cfg.authority.starts_with('[') {
            Some(cfg.authority.clone())
        } else {
            None
        };
        let get_headers = request_headers("GET", &cfg.authority, None);
        let keepalive = (cfg.idle_timeout / 4).max(Duration::from_secs(1));
        Ok(Self {
            id,
            poll,
            rx,
            results,
            connected,
            addr_counter,
            ready,
            qconfig,
            h3config,
            server_name,
            get_headers,
            post_headers: Vec::new(),
            post_len: usize::MAX,
            empty: Arc::new(Vec::new()),
            slots: Vec::new(),
            free: Vec::new(),
            timers: BinaryHeap::new(),
            retries: VecDeque::new(),
            dirty: Vec::new(),
            phase: Phase::Idle,
            next_keepalive: Instant::now() + Duration::from_secs(1),
            keepalive,
            buf: vec![0; 65535],
            out: vec![0; MAX_DATAGRAM_SIZE],
            dropped: 0,
            cfg,
        })
    }

    fn run(&mut self) {
        let mut events = Events::with_capacity(4096);
        let mut readable = Vec::new();
        loop {
            let timeout = self.next_wakeup().map(|t| t.saturating_duration_since(Instant::now()));
            if let Err(e) = self.poll.poll(&mut events, timeout) {
                if e.kind() != ErrorKind::Interrupted {
                    fatalf!("worker {}: poll: {e}", self.id);
                }
            }
            let mut stop = false;
            readable.clear();
            for ev in events.iter() {
                if ev.token() == WAKER {
                    while let Ok(cmd) = self.rx.try_recv() {
                        if matches!(cmd, Command::Stop) {
                            stop = true;
                        } else {
                            self.start(cmd);
                        }
                    }
                } else {
                    readable.push(ev.token().0);
                }
            }
            for &idx in &readable {
                self.on_readable(idx);
            }
            let now = Instant::now();
            self.fire_timers(now);
            self.fire_retries(now);
            self.drive(now);
            self.keepalive(now);
            self.flush_dirty();
            if stop {
                self.close_all();
                if self.dropped > 0 {
                    logf!("worker {}: {} datagrams dropped at a full socket buffer", self.id, self.dropped);
                }
                return;
            }
        }
    }

    fn next_wakeup(&mut self) -> Option<Instant> {
        let mut t = Some(self.next_keepalive);
        let mut min = |x: Option<Instant>| {
            if let Some(x) = x {
                t = Some(t.map_or(x, |t: Instant| t.min(x)));
            }
        };
        min(self.timers.peek().map(|r| r.0 .0));
        min(self.retries.front().map(|r| r.0));
        match &self.phase {
            Phase::Echo { wait_until, .. } => min(*wait_until),
            Phase::Rate { deadline, next_tick, grace_until, .. } => {
                min(Some(if grace_until.is_some() {
                    grace_until.unwrap()
                } else if next_tick < deadline {
                    *next_tick
                } else {
                    *deadline
                }))
            }
            _ => {}
        }
        t
    }

    // -- commands ----------------------------------------------------------

    fn start(&mut self, cmd: Command) {
        match cmd {
            Command::Dial { count, concurrency } => {
                self.phase = Phase::Dial { remaining: count, active: 0, concurrency, lat: Latencies::default() };
            }
            Command::Echo { job, concurrency } => {
                let len = job.payloads.first().map_or(0, |p| p.len());
                if len != self.post_len {
                    self.post_headers = request_headers("POST", &self.cfg.authority, Some(len));
                    self.post_len = len;
                }
                let idle: VecDeque<usize> =
                    self.slots.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).collect();
                let slots = concurrency.min(idle.len());
                self.phase = Phase::Echo {
                    job,
                    slots,
                    in_flight: 0,
                    idle,
                    exhausted: slots == 0,
                    wait_until: None,
                    lat: Latencies::default(),
                };
            }
            Command::Rate(job) => {
                let len = job.payload.len();
                if len != self.post_len {
                    self.post_headers = request_headers("POST", &self.cfg.authority, Some(len));
                    self.post_len = len;
                }
                let broken: Vec<usize> = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.as_ref().is_some_and(|c| c.state == State::Broken))
                    .map(|(i, _)| i)
                    .collect();
                let now = Instant::now();
                for &idx in &broken {
                    self.dial(DialJob { start: now, attempt: 1, purpose: Purpose::RedialRate, slot: Some(idx) });
                }
                self.phase = Phase::RatePrep { job, redials: broken.len() };
            }
            Command::Stop => {}
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        self.phase = Phase::Idle;
        let _ = self.results.send((self.id, outcome));
    }

    // -- the benchmarks ----------------------------------------------------

    fn drive(&mut self, now: Instant) {
        match &mut self.phase {
            Phase::Idle => {}
            Phase::Dial { remaining, active, concurrency, lat } => {
                if *remaining == 0 && *active == 0 {
                    let lat = std::mem::take(lat);
                    self.finish(Outcome::Dial(lat));
                    return;
                }
                let mut starts = 0;
                while *remaining > 0 && *active < *concurrency {
                    *remaining -= 1;
                    *active += 1;
                    starts += 1;
                }
                for _ in 0..starts {
                    self.dial(DialJob { start: now, attempt: 1, purpose: Purpose::New, slot: None });
                }
            }
            Phase::Echo { .. } => self.drive_echo(now),
            Phase::RatePrep { job, redials } => {
                if *redials == 0 {
                    let job = job.clone();
                    let conns: Vec<usize> = self
                        .slots
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.as_ref().is_some_and(|c| c.state == State::Ready))
                        .map(|(i, _)| i)
                        .collect();
                    for &idx in &conns {
                        self.slots[idx].as_mut().unwrap().in_flight = 0;
                    }
                    self.phase = Phase::Rate {
                        deadline: now + job.duration,
                        next_tick: now + job.tick,
                        job,
                        conns,
                        grace_until: None,
                        counts: RateCounts::default(),
                    };
                }
            }
            Phase::Rate { .. } => self.drive_rate(now),
        }
    }

    fn drive_echo(&mut self, now: Instant) {
        loop {
            let Phase::Echo { job, slots, in_flight, idle, exhausted, wait_until, lat } = &mut self.phase else {
                return;
            };
            if *exhausted && *in_flight == 0 {
                let lat = std::mem::take(lat);
                self.finish(Outcome::Echo(lat));
                return;
            }
            if *exhausted || *in_flight >= *slots {
                return;
            }
            if let Some(t) = *wait_until {
                if now < t {
                    return;
                }
                *wait_until = None;
            }
            if let Some(limiter) = &job.limiter {
                if let Err(wait) = limiter.take(1) {
                    *wait_until = Some(now + wait);
                    return;
                }
            }
            let Some(idx) = idle.pop_front() else { return };
            let n = job.issued.fetch_add(1, Ordering::Relaxed);
            if n >= job.times {
                idle.push_front(idx);
                *exhausted = true;
                continue;
            }
            *in_flight += 1;
            let payload = job.payloads[n as usize % job.payloads.len()].clone();
            let start = Instant::now();
            if self.slots[idx].as_ref().unwrap().state == State::Broken {
                self.dial(DialJob { start, attempt: 1, purpose: Purpose::RedialEcho { start }, slot: Some(idx) });
                continue;
            }
            if let Err(e) = self.send_request(idx, ReqKind::Echo { start }, payload) {
                self.echo_done(idx, Err(e), start);
            }
        }
    }

    fn echo_done(&mut self, idx: usize, result: Result<usize, String>, start: Instant) {
        let now = Instant::now();
        if let Phase::Echo { job, in_flight, idle, lat, .. } = &mut self.phase {
            *in_flight -= 1;
            idle.push_back(idx);
            if job.measure {
                match result {
                    Ok(_) => lat.ok(now.duration_since(start)),
                    Err(e) => lat.fail(e),
                }
            }
        }
    }

    fn drive_rate(&mut self, now: Instant) {
        let Phase::Rate { job, deadline, next_tick, grace_until, counts, .. } = &mut self.phase else { return };
        if let Some(grace) = *grace_until {
            // The last batch has had its tick to come back in, or has.
            if counts.answered >= counts.send_times || now >= grace {
                let counts = *counts;
                self.finish(Outcome::Rate(counts));
            }
            return;
        }
        if now >= *deadline {
            *grace_until = Some(now + job.tick);
            return;
        }
        if now < *next_tick {
            return;
        }
        // A tick the loop was too busy to take is dropped, as a Go ticker
        // drops it, rather than sent late on top of the next one.
        while *next_tick <= now {
            *next_tick += job.tick;
        }
        self.rate_tick();
    }

    fn rate_tick(&mut self) {
        let Phase::Rate { job, conns, .. } = &mut self.phase else { return };
        let job = job.clone();
        let conns = std::mem::take(conns);
        let batch = job.batch;
        let limit = batch as u64 * MAX_BATCHES_IN_FLIGHT;
        let mut sent = 0u64;
        for &idx in &conns {
            let Some(c) = self.slots[idx].as_mut() else { continue };
            if c.state != State::Ready || c.goaway || c.in_flight + batch as u64 > limit {
                continue;
            }
            // The server's MAX_STREAMS, which -streams sets for both.
            if c.q.peer_streams_left_bidi() < batch as u64 {
                continue;
            }
            if let Some(l) = &job.limiter {
                if l.take(batch).is_err() {
                    continue;
                }
            }
            for _ in 0..batch {
                if self.send_request(idx, ReqKind::Rate, job.payload.clone()).is_err() {
                    break;
                }
                self.slots[idx].as_mut().unwrap().in_flight += 1;
                sent += 1;
            }
        }
        if let Phase::Rate { conns: c, counts, .. } = &mut self.phase {
            *c = conns;
            counts.send_times += sent;
            counts.send_bytes += sent * job.payload.len() as u64;
        }
    }

    // -- dialing -----------------------------------------------------------

    fn alloc_slot(&mut self) -> usize {
        if let Some(i) = self.free.pop() {
            i
        } else {
            self.slots.push(None);
            self.slots.len() - 1
        }
    }

    fn dial(&mut self, job: DialJob) {
        if let Err(e) = self.try_dial(job) {
            self.dial_failed_job(job, e);
        }
    }

    fn try_dial(&mut self, job: DialJob) -> Result<(), String> {
        let addrs = &self.cfg.addrs;
        let peer = addrs[self.addr_counter.fetch_add(1, Ordering::Relaxed) % addrs.len()];
        let bind: SocketAddr =
            if peer.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
        let mut sock = UdpSocket::bind(bind).map_err(|e| format!("bind: {e}"))?;
        sock.connect(peer).map_err(|e| format!("connect {peer}: {e}"))?;
        set_socket_buffers(&sock);
        let local = sock.local_addr().map_err(|e| e.to_string())?;
        let mut scid = [0u8; quiche::MAX_CONN_ID_LEN];
        rand::fill(&mut scid[..]);
        let scid = quiche::ConnectionId::from_ref(&scid);
        let q = quiche::connect(self.server_name.as_deref(), &scid, local, peer, &mut self.qconfig)
            .map_err(|e| format!("quiche connect: {e}"))?;

        let idx = match job.slot {
            Some(i) => {
                if let Some(mut old) = self.slots[i].take() {
                    let _ = self.poll.registry().deregister(&mut old.sock);
                    if old.state == State::Ready {
                        self.ready.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                i
            }
            None => self.alloc_slot(),
        };
        self.poll
            .registry()
            .register(&mut sock, Token(idx), Interest::READABLE)
            .map_err(|e| format!("register: {e}"))?;
        let now = Instant::now();
        self.slots[idx] = Some(Conn {
            sock,
            q,
            h3: None,
            local,
            peer,
            state: State::Connecting,
            dial: Some(DialJob { slot: Some(idx), ..job }),
            attempt_deadline: Some(now + self.cfg.dial_timeout),
            streams: HashMap::new(),
            pending: Vec::new(),
            queued: VecDeque::new(),
            deadline: None,
            scheduled: None,
            last_active: now,
            dirty: false,
            in_flight: 0,
            goaway: false,
            socket_error: None,
        });
        self.flush(idx);
        Ok(())
    }

    fn dial_ok(&mut self, idx: usize) {
        let c = self.slots[idx].as_mut().unwrap();
        let Some(job) = c.dial.take() else { return };
        c.state = State::Ready;
        c.attempt_deadline = None;
        self.ready.fetch_add(1, Ordering::Relaxed);
        match job.purpose {
            Purpose::New => {
                self.connected.fetch_add(1, Ordering::Relaxed);
                if let Phase::Dial { active, lat, .. } = &mut self.phase {
                    *active -= 1;
                    lat.ok(job.start.elapsed());
                }
            }
            Purpose::RedialEcho { start } => {
                let payload = match &self.phase {
                    Phase::Echo { job, .. } => job.payloads[0].clone(),
                    _ => return,
                };
                if let Err(e) = self.send_request(idx, ReqKind::Echo { start }, payload) {
                    self.echo_done(idx, Err(e), start);
                }
            }
            Purpose::RedialRate => {
                if let Phase::RatePrep { redials, .. } = &mut self.phase {
                    *redials -= 1;
                }
            }
        }
    }

    /// A dial attempt failed on the connection in slot idx.
    fn dial_failed(&mut self, idx: usize, err: String) {
        let Some(c) = self.slots[idx].as_mut() else { return };
        let Some(job) = c.dial.take() else { return };
        c.state = State::Broken;
        c.attempt_deadline = None;
        c.streams.clear();
        c.pending.clear();
        c.queued.clear();
        if let Purpose::New = job.purpose {
            // A new connection that never came up leaves nothing behind.
            if let Some(mut old) = self.slots[idx].take() {
                let _ = self.poll.registry().deregister(&mut old.sock);
            }
            self.free.push(idx);
        }
        let job = match job.purpose {
            Purpose::New => DialJob { slot: None, ..job },
            _ => job,
        };
        self.dial_failed_job(job, err);
    }

    fn dial_failed_job(&mut self, job: DialJob, err: String) {
        if job.attempt < self.cfg.dial_retries {
            let retry = DialJob { attempt: job.attempt + 1, ..job };
            self.retries.push_back((Instant::now() + self.cfg.dial_retry_interval, retry));
            return;
        }
        match job.purpose {
            Purpose::New => {
                if let Phase::Dial { active, lat, .. } = &mut self.phase {
                    *active -= 1;
                    lat.fail(err);
                }
            }
            Purpose::RedialEcho { start } => {
                if let Some(idx) = job.slot {
                    self.echo_done(idx, Err(format!("redial: {err}")), start);
                }
            }
            Purpose::RedialRate => {
                logf!("BenchMultiplex: redial failed, leaving the connection out: {err}");
                if let Phase::RatePrep { redials, .. } = &mut self.phase {
                    *redials -= 1;
                }
            }
        }
    }

    fn fire_retries(&mut self, now: Instant) {
        while let Some(&(t, job)) = self.retries.front() {
            if t > now {
                break;
            }
            self.retries.pop_front();
            self.dial(job);
        }
    }

    // -- requests ----------------------------------------------------------

    /// Sends a request, or for a BenchEcho round trip or a first GET that
    /// the connection has no room for yet, queues it on the connection until
    /// it has: quiche bounds what a stream may send by flow control and by
    /// the congestion window, and an HTTP/1 client would block in its write
    /// the same way. BenchMultiplex requests are not queued, since that
    /// benchmark skips a connection that cannot take a batch rather than build
    /// a backlog in front of the server.
    fn send_request(&mut self, idx: usize, kind: ReqKind, body: Arc<Vec<u8>>) -> Result<(), String> {
        let c = self.slots[idx].as_mut().unwrap();
        if kind != ReqKind::Rate && !c.queued.is_empty() {
            c.queued.push_back((kind, body));
            return Ok(());
        }
        match self.try_send(idx, kind, body.clone()) {
            Ok(()) => Ok(()),
            Err(SendError::Blocked(e)) if kind == ReqKind::Rate => Err(e),
            Err(SendError::Blocked(_)) => {
                self.slots[idx].as_mut().unwrap().queued.push_back((kind, body));
                Ok(())
            }
            Err(SendError::Failed(e)) => Err(e),
        }
    }

    fn try_send(&mut self, idx: usize, kind: ReqKind, body: Arc<Vec<u8>>) -> Result<(), SendError> {
        let c = self.slots[idx].as_mut().unwrap();
        let Some(h3) = c.h3.as_mut() else { return Err(SendError::Failed("no HTTP/3 connection".into())) };
        let headers = if kind == ReqKind::Dial { &self.get_headers } else { &self.post_headers };
        let stream = match h3.send_request(&mut c.q, headers, body.is_empty()) {
            Ok(s) => s,
            Err(e @ (quiche::h3::Error::StreamBlocked | quiche::h3::Error::TransportError(quiche::Error::StreamLimit))) => {
                return Err(SendError::Blocked(format!("send request: {e}")));
            }
            Err(e) => {
                // The connection is in no state to carry requests; closing it
                // sends it down the path every broken connection takes.
                let _ = c.q.close(true, 0x102, b"");
                mark_dirty(&mut self.dirty, c, idx);
                return Err(SendError::Failed(format!("send request: {e}")));
            }
        };
        if !body.is_empty() {
            let off = match h3.send_body(&mut c.q, stream, &body, true) {
                Ok(n) => n,
                Err(quiche::h3::Error::Done) => 0,
                Err(e) => {
                    let _ = c.q.close(true, 0x102, b"");
                    mark_dirty(&mut self.dirty, c, idx);
                    return Err(SendError::Failed(format!("send body: {e}")));
                }
            };
            if off < body.len() {
                c.pending.push(PendingBody { stream, body: body.clone(), off });
            }
        }
        let expect = if kind == ReqKind::Dial { self.empty.clone() } else { body };
        c.streams.insert(stream, Req { kind, expect, status: 0, got: 0, mismatch: false });
        mark_dirty(&mut self.dirty, c, idx);
        Ok(())
    }

    /// Sends the requests queued on a connection for as long as it has room.
    fn send_queued(&mut self, idx: usize) {
        loop {
            let Some(c) = self.slots[idx].as_mut() else { return };
            if c.q.is_closed() {
                return;
            }
            let Some((kind, body)) = c.queued.pop_front() else { return };
            match self.try_send(idx, kind, body.clone()) {
                Ok(()) => {}
                Err(SendError::Blocked(_)) => {
                    self.slots[idx].as_mut().unwrap().queued.push_front((kind, body));
                    return;
                }
                Err(SendError::Failed(e)) => self.response(idx, kind, Err(e)),
            }
        }
    }

    // -- I/O ---------------------------------------------------------------

    fn on_readable(&mut self, idx: usize) {
        let Some(c) = self.slots.get_mut(idx).and_then(|s| s.as_mut()) else { return };
        let mut got = false;
        loop {
            match c.sock.recv(&mut self.buf) {
                Ok(n) => {
                    got = true;
                    let info = quiche::RecvInfo { from: c.peer, to: c.local };
                    let _ = c.q.recv(&mut self.buf[..n], info);
                }
                Err(e) if would_block(&e) => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    c.socket_error = Some(format!("recv: {e}"));
                    break;
                }
            }
        }
        if got {
            c.last_active = Instant::now();
        }
        self.process(idx);
    }

    fn flush(&mut self, idx: usize) {
        let Some(c) = self.slots.get_mut(idx).and_then(|s| s.as_mut()) else { return };
        c.dirty = false;
        let mut sent = false;
        loop {
            match c.q.send(&mut self.out) {
                Ok((n, _)) => match c.sock.send(&self.out[..n]) {
                    Ok(_) => sent = true,
                    Err(e) if would_block(&e) => {
                        // quiche counts it as sent; its loss recovery sends
                        // it again, as it would after a drop on the wire.
                        self.dropped += 1;
                        break;
                    }
                    Err(e) => {
                        c.socket_error = Some(format!("send: {e}"));
                        break;
                    }
                },
                Err(quiche::Error::Done) => break,
                Err(e) => {
                    let _ = c.q.close(false, 0x1, b"send failed");
                    c.socket_error = Some(format!("quiche send: {e}"));
                    break;
                }
            }
        }
        let now = Instant::now();
        if sent {
            c.last_active = now;
        }
        let failed = c.socket_error.is_some();
        self.schedule(idx);
        if failed {
            self.process(idx);
        }
    }

    fn flush_dirty(&mut self) {
        while let Some(idx) = self.dirty.pop() {
            if self.slots.get(idx).is_some_and(|s| s.as_ref().is_some_and(|c| c.dirty)) {
                self.flush(idx);
            }
        }
    }

    fn schedule(&mut self, idx: usize) {
        let Some(c) = self.slots[idx].as_mut() else { return };
        let mut d = c.q.timeout_instant();
        if let Some(a) = c.attempt_deadline {
            d = Some(d.map_or(a, |d| d.min(a)));
        }
        c.deadline = d;
        if let Some(d) = d {
            if c.scheduled.is_none_or(|s| d < s) {
                c.scheduled = Some(d);
                self.timers.push(Reverse((d, idx)));
            }
        }
    }

    fn fire_timers(&mut self, now: Instant) {
        while let Some(&Reverse((t, idx))) = self.timers.peek() {
            if t > now {
                break;
            }
            self.timers.pop();
            let Some(c) = self.slots.get_mut(idx).and_then(|s| s.as_mut()) else { continue };
            if c.scheduled != Some(t) {
                continue;
            }
            c.scheduled = None;
            match c.deadline {
                Some(d) if d <= now => {}
                _ => {
                    self.schedule(idx);
                    continue;
                }
            }
            if c.state == State::Connecting && c.attempt_deadline.is_some_and(|a| a <= now) {
                let reason = if c.q.is_established() { "timeout waiting for the first response" } else { "handshake timeout" };
                self.dial_failed(idx, reason.to_string());
                continue;
            }
            c.q.on_timeout();
            mark_dirty(&mut self.dirty, c, idx);
            self.process(idx);
        }
    }

    fn keepalive(&mut self, now: Instant) {
        if now < self.next_keepalive {
            return;
        }
        self.next_keepalive = now + Duration::from_secs(1);
        for idx in 0..self.slots.len() {
            let Some(c) = self.slots[idx].as_mut() else { continue };
            if c.state == State::Ready && c.streams.is_empty() && now.duration_since(c.last_active) >= self.keepalive {
                let _ = c.q.send_ack_eliciting();
                mark_dirty(&mut self.dirty, c, idx);
            }
        }
    }

    fn close_all(&mut self) {
        for idx in 0..self.slots.len() {
            if let Some(c) = self.slots[idx].as_mut() {
                if !c.q.is_closed() {
                    let _ = c.q.close(true, H3_NO_ERROR, b"");
                    self.flush(idx);
                }
            }
        }
    }

    // -- the HTTP/3 side of a connection -------------------------------------

    fn process(&mut self, idx: usize) {
        let check = self.cfg.check;
        let Some(c) = self.slots.get_mut(idx).and_then(|s| s.as_mut()) else { return };
        let events = process_conn(c, &self.h3config, &mut self.buf, check);
        mark_dirty(&mut self.dirty, c, idx);
        for ev in events {
            match ev {
                ConnEvent::Established => {
                    if let Err(e) = self.send_request(idx, ReqKind::Dial, self.empty.clone()) {
                        self.dial_failed(idx, e);
                    }
                }
                ConnEvent::Response { kind, result } => self.response(idx, kind, result),
                ConnEvent::Closed(reason) => {
                    let Some(c) = self.slots[idx].as_mut() else { continue };
                    match c.state {
                        State::Connecting => self.dial_failed(idx, reason),
                        State::Ready => {
                            c.state = State::Broken;
                            self.ready.fetch_sub(1, Ordering::Relaxed);
                        }
                        State::Broken => {}
                    }
                }
            }
        }
        self.send_queued(idx);
    }

    fn response(&mut self, idx: usize, kind: ReqKind, result: Result<usize, String>) {
        match kind {
            ReqKind::Dial => match result {
                Ok(_) => self.dial_ok(idx),
                Err(e) => self.dial_failed(idx, e),
            },
            ReqKind::Echo { start } => self.echo_done(idx, result, start),
            ReqKind::Rate => {
                if let Some(c) = self.slots[idx].as_mut() {
                    c.in_flight = c.in_flight.saturating_sub(1);
                }
                // Responses to a benchmark that has already been counted are
                // left out of it, as the Go client stops reading them.
                if let Phase::Rate { counts, .. } = &mut self.phase {
                    match result {
                        Ok(n) => {
                            counts.answered += 1;
                            counts.recv_times += 1;
                            counts.recv_bytes += n as u64;
                        }
                        Err(e) if e.starts_with("status ") || e.starts_with("response body") => counts.answered += 1,
                        Err(_) => {}
                    }
                }
            }
        }
    }
}

fn mark_dirty(dirty: &mut Vec<usize>, c: &mut Conn, idx: usize) {
    if !c.dirty {
        c.dirty = true;
        dirty.push(idx);
    }
}

/// Reads whatever the connection has for the application: the handshake
/// completing, responses, and the connection closing. It returns what
/// happened rather than acting on it, so that the worker can act with the
/// connection no longer borrowed.
fn process_conn(c: &mut Conn, h3config: &quiche::h3::Config, buf: &mut [u8], check: bool) -> Vec<ConnEvent> {
    let mut events = Vec::new();
    if let Some(err) = c.socket_error.take() {
        if !c.q.is_closed() {
            let _ = c.q.close(false, 0x1, b"");
        }
        fail_streams(c, &err, &mut events);
        events.push(ConnEvent::Closed(err));
        return events;
    }
    if c.q.is_closed() {
        let reason = close_reason(&c.q);
        fail_streams(c, &reason, &mut events);
        events.push(ConnEvent::Closed(reason));
        return events;
    }
    if c.h3.is_none() && c.q.is_established() {
        match quiche::h3::Connection::with_transport(&mut c.q, h3config) {
            Ok(h3) => {
                c.h3 = Some(h3);
                if c.state == State::Connecting {
                    events.push(ConnEvent::Established);
                }
            }
            Err(e) => {
                let _ = c.q.close(true, 0x101, b"");
                events.push(ConnEvent::Closed(format!("HTTP/3 setup: {e}")));
                return events;
            }
        }
    }
    let Conn { q, h3, streams, pending, goaway, .. } = c;
    let Some(h3) = h3.as_mut() else { return events };
    loop {
        match h3.poll(q) {
            Ok((sid, quiche::h3::Event::Headers { list, .. })) => {
                if let Some(req) = streams.get_mut(&sid) {
                    for h in &list {
                        if h.name() == b":status" {
                            req.status = std::str::from_utf8(h.value()).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                        }
                    }
                }
            }
            Ok((sid, quiche::h3::Event::Data)) => {
                while let Ok(n) = h3.recv_body(q, sid, buf) {
                    if let Some(req) = streams.get_mut(&sid) {
                        if check && !req.mismatch {
                            let end = req.got + n;
                            if end > req.expect.len() || req.expect[req.got..end] != buf[..n] {
                                req.mismatch = true;
                            }
                        }
                        req.got += n;
                    }
                }
            }
            Ok((sid, quiche::h3::Event::Finished)) => {
                if let Some(req) = streams.remove(&sid) {
                    let result = if req.status != 200 {
                        Err(format!("status {}", req.status))
                    } else if req.got != req.expect.len() {
                        Err(format!("response body is {} bytes, want {}", req.got, req.expect.len()))
                    } else if req.mismatch {
                        Err("response body is not equal to the request's".to_string())
                    } else {
                        Ok(req.got)
                    };
                    events.push(ConnEvent::Response { kind: req.kind, result });
                }
            }
            Ok((sid, quiche::h3::Event::Reset(code))) => {
                pending.retain(|p| p.stream != sid);
                if let Some(req) = streams.remove(&sid) {
                    events.push(ConnEvent::Response { kind: req.kind, result: Err(format!("stream reset: {code:#x}")) });
                }
            }
            Ok((_, quiche::h3::Event::GoAway)) => *goaway = true,
            Ok((_, quiche::h3::Event::PriorityUpdate)) => {}
            Err(quiche::h3::Error::Done) => break,
            Err(e) => {
                // H3_GENERAL_PROTOCOL_ERROR
                let _ = q.close(true, 0x101, format!("{e}").as_bytes());
                break;
            }
        }
    }
    // Bodies that did not fit in the stream's window when they were sent.
    pending.retain_mut(|p| match h3.send_body(q, p.stream, &p.body[p.off..], true) {
        Ok(n) => {
            p.off += n;
            p.off < p.body.len()
        }
        Err(quiche::h3::Error::Done) => true,
        Err(_) => false,
    });
    if q.is_closed() {
        let reason = close_reason(q);
        fail_streams(c, &reason, &mut events);
        events.push(ConnEvent::Closed(reason));
    }
    events
}

fn fail_streams(c: &mut Conn, reason: &str, events: &mut Vec<ConnEvent>) {
    c.pending.clear();
    for (kind, _) in c.queued.drain(..) {
        events.push(ConnEvent::Response { kind, result: Err(format!("connection closed: {reason}")) });
    }
    for (_, req) in c.streams.drain() {
        events.push(ConnEvent::Response { kind: req.kind, result: Err(format!("connection closed: {reason}")) });
    }
}

fn close_reason(q: &quiche::Connection) -> String {
    if q.is_timed_out() {
        return "idle timeout".to_string();
    }
    if let Some(e) = q.peer_error() {
        return format!("closed by the server: code {:#x} {}", e.error_code, String::from_utf8_lossy(&e.reason));
    }
    if let Some(e) = q.local_error() {
        return format!("closed here: code {:#x} {}", e.error_code, String::from_utf8_lossy(&e.reason));
    }
    "closed".to_string()
}
