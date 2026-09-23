//! One worker: its UDP sockets, the QUIC connections that arrive on them, and
//! the event loop that drives quiche for them, in the shape of quiche's own
//! examples/http3-server.rs.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use quiche::h3::NameValue;

use crate::Settings;

const MAX_DATAGRAM_SIZE: usize = 1350;
/// The length of the connection IDs this server hands out, which is how long
/// the destination connection ID of every short-header packet is.
const CID_LEN: usize = 16;
/// Datagrams read off one socket before the others get their turn.
const READ_BUDGET: usize = 1024;
/// What a request body may grow to before the request is refused.
const MAX_BODY: usize = 16 << 20;

const ECHO_PATH: &[u8] = b"/echo";

/// A self-signed ECDSA P-256 certificate for localhost, written to two
/// temporary PEM files for quiche to load. The Go servers make the same kind
/// at startup; the client does not verify it.
pub fn certificate_files() -> Result<(String, String), String> {
    let names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    let ck = rcgen::generate_simple_self_signed(names).map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let cert = dir.join(format!("go-http3-benchmark-quiche-{pid}.crt"));
    let key = dir.join(format!("go-http3-benchmark-quiche-{pid}.key"));
    std::fs::write(&cert, ck.cert.pem()).map_err(|e| e.to_string())?;
    std::fs::write(&key, ck.signing_key.serialize_pem()).map_err(|e| e.to_string())?;
    Ok((cert.to_string_lossy().into_owned(), key.to_string_lossy().into_owned()))
}

/// The transport settings, as close to the Go servers' as quiche puts them:
/// -streams request streams per connection, -idle idle timeout, and receive
/// windows that let a request body in without waiting for flow control.
pub fn quiche_config(s: &Settings, cert: &str, key: &str) -> Result<quiche::Config, String> {
    let mut c = quiche::Config::new(quiche::PROTOCOL_VERSION).map_err(|e| e.to_string())?;
    c.load_cert_chain_from_pem_file(cert).map_err(|e| format!("{cert}: {e}"))?;
    c.load_priv_key_from_pem_file(key).map_err(|e| format!("{key}: {e}"))?;
    c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL).map_err(|e| e.to_string())?;
    c.set_max_idle_timeout(s.idle.as_millis() as u64);
    c.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
    c.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
    c.set_initial_max_data(16 * 1024 * 1024);
    c.set_initial_max_stream_data_bidi_local(1024 * 1024);
    c.set_initial_max_stream_data_bidi_remote(1024 * 1024);
    c.set_initial_max_stream_data_uni(256 * 1024);
    c.set_initial_max_streams_bidi(s.streams);
    // The client's control and QPACK streams, with room to spare.
    c.set_initial_max_streams_uni(16);
    c.set_disable_active_migration(true);
    c.grease(false);
    Ok(c)
}

struct Req {
    echo: bool,
    body: Vec<u8>,
    too_large: bool,
}

/// A response that did not fit in its stream's window when it was sent.
struct Pending {
    stream: u64,
    headers: Option<Vec<quiche::h3::Header>>,
    body: Vec<u8>,
    off: usize,
}

struct Client {
    conn: quiche::Connection,
    h3: Option<quiche::h3::Connection>,
    sock: usize,
    ids: Vec<quiche::ConnectionId<'static>>,
    reqs: HashMap<u64, Req>,
    pending: Vec<Pending>,
    deadline: Option<Instant>,
    scheduled: Option<Instant>,
    dirty: bool,
}

pub struct Worker {
    poll: Poll,
    sockets: Vec<(UdpSocket, SocketAddr)>,
    config: quiche::Config,
    h3config: quiche::h3::Config,
    payload: usize,
    clients: Vec<Option<Client>>,
    free: Vec<usize>,
    ids: HashMap<quiche::ConnectionId<'static>, usize>,
    timers: BinaryHeap<Reverse<(Instant, usize)>>,
    dirty: Vec<usize>,
    unread: Vec<usize>,
    buf: Vec<u8>,
    out: Vec<u8>,
    body_buf: Vec<u8>,
}

fn set_socket_buffers(sock: &std::net::UdpSocket) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // What quic-go asks the kernel for on its sockets; the kernel caps it
        // at rmem_max and wmem_max.
        let size: libc::c_int = 7 << 20;
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
    e.kind() == ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::ENOBUFS)
}

impl Worker {
    pub fn new(sockets: Vec<std::net::UdpSocket>, config: quiche::Config, payload: usize) -> Self {
        let poll = Poll::new().unwrap_or_else(|e| fatalf!("poll: {e}"));
        let mut socks = Vec::with_capacity(sockets.len());
        for (i, s) in sockets.into_iter().enumerate() {
            set_socket_buffers(&s);
            s.set_nonblocking(true).unwrap_or_else(|e| fatalf!("set_nonblocking: {e}"));
            let local = s.local_addr().unwrap_or_else(|e| fatalf!("local_addr: {e}"));
            let mut s = UdpSocket::from_std(s);
            poll.registry()
                .register(&mut s, Token(i), Interest::READABLE)
                .unwrap_or_else(|e| fatalf!("register: {e}"));
            socks.push((s, local));
        }
        Self {
            poll,
            sockets: socks,
            config,
            h3config: quiche::h3::Config::new().unwrap_or_else(|e| fatalf!("h3 config: {e}")),
            payload,
            clients: Vec::new(),
            free: Vec::new(),
            ids: HashMap::new(),
            timers: BinaryHeap::new(),
            dirty: Vec::new(),
            unread: Vec::new(),
            buf: vec![0; 65535],
            out: vec![0; MAX_DATAGRAM_SIZE],
            body_buf: vec![0; 65535],
        }
    }

    pub fn run(mut self) {
        let mut events = Events::with_capacity(1024);
        loop {
            let timeout = if !self.unread.is_empty() {
                Some(Duration::ZERO)
            } else {
                self.timers.peek().map(|r| r.0 .0.saturating_duration_since(Instant::now()))
            };
            if let Err(e) = self.poll.poll(&mut events, timeout) {
                if e.kind() != ErrorKind::Interrupted {
                    fatalf!("poll: {e}");
                }
            }
            for ev in events.iter() {
                if !self.unread.contains(&ev.token().0) {
                    self.unread.push(ev.token().0);
                }
            }
            // Sockets read up to their budget stay on the list, since an
            // edge-triggered poll will not report what is still queued.
            let unread = std::mem::take(&mut self.unread);
            for s in unread {
                if !self.read_socket(s) {
                    self.unread.push(s);
                }
            }
            self.fire_timers(Instant::now());
            self.flush_dirty();
        }
    }

    /// Reads a socket until it would block, or the budget is spent: false.
    fn read_socket(&mut self, s: usize) -> bool {
        for _ in 0..READ_BUDGET {
            let (n, from) = match self.sockets[s].0.recv_from(&mut self.buf) {
                Ok(v) => v,
                Err(e) if would_block(&e) => return true,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // An ICMP error for an earlier send, which says nothing about
                // this socket's other peers.
                Err(_) => continue,
            };
            self.on_datagram(s, from, n);
        }
        false
    }

    fn on_datagram(&mut self, s: usize, from: SocketAddr, n: usize) {
        let local = self.sockets[s].1;
        let hdr = match quiche::Header::from_slice(&mut self.buf[..n], CID_LEN) {
            Ok(h) => h,
            Err(_) => return,
        };
        let idx = match self.ids.get(&hdr.dcid) {
            Some(&i) => i,
            None => {
                if hdr.ty != quiche::Type::Initial {
                    return;
                }
                if !quiche::version_is_supported(hdr.version) {
                    if let Ok(len) = quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut self.out) {
                        let _ = self.sockets[s].0.send_to(&self.out[..len], from);
                    }
                    return;
                }
                let mut cid = [0u8; CID_LEN];
                rand::fill(&mut cid[..]);
                let scid = quiche::ConnectionId::from_vec(cid.to_vec());
                let conn = match quiche::accept(&scid, None, local, from, &mut self.config) {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let idx = match self.free.pop() {
                    Some(i) => i,
                    None => {
                        self.clients.push(None);
                        self.clients.len() - 1
                    }
                };
                // The client's first destination ID too, for the Initials it
                // sends again before it has heard from the server.
                let odcid = hdr.dcid.clone().into_owned();
                self.ids.insert(scid.clone(), idx);
                self.ids.insert(odcid.clone(), idx);
                self.clients[idx] = Some(Client {
                    conn,
                    h3: None,
                    sock: s,
                    ids: vec![scid, odcid],
                    reqs: HashMap::new(),
                    pending: Vec::new(),
                    deadline: None,
                    scheduled: None,
                    dirty: false,
                });
                idx
            }
        };
        let Some(c) = self.clients[idx].as_mut() else { return };
        let info = quiche::RecvInfo { from, to: local };
        if c.conn.recv(&mut self.buf[..n], info).is_err() && !c.conn.is_closed() {
            // A datagram quiche would not take; the connection goes on.
        }
        process(c, &self.h3config, &mut self.body_buf, self.payload);
        if !c.dirty {
            c.dirty = true;
            self.dirty.push(idx);
        }
    }

    fn flush_dirty(&mut self) {
        while let Some(idx) = self.dirty.pop() {
            self.flush(idx);
        }
    }

    fn flush(&mut self, idx: usize) {
        let Some(c) = self.clients[idx].as_mut() else { return };
        c.dirty = false;
        let sock = &self.sockets[c.sock].0;
        loop {
            match c.conn.send(&mut self.out) {
                Ok((n, info)) => match sock.send_to(&self.out[..n], info.to) {
                    Ok(_) => {}
                    // quiche counts it as sent, and its loss recovery sends
                    // it again, as after a drop on the wire.
                    Err(e) if would_block(&e) => break,
                    Err(_) => break,
                },
                Err(quiche::Error::Done) => break,
                Err(_) => {
                    let _ = c.conn.close(false, 0x1, b"");
                    break;
                }
            }
        }
        if c.conn.is_closed() {
            self.remove(idx);
            return;
        }
        let d = c.conn.timeout_instant();
        c.deadline = d;
        if let Some(d) = d {
            if c.scheduled.is_none_or(|s| d < s) {
                c.scheduled = Some(d);
                self.timers.push(Reverse((d, idx)));
            }
        }
    }

    fn remove(&mut self, idx: usize) {
        if let Some(c) = self.clients[idx].take() {
            for id in &c.ids {
                if self.ids.get(id) == Some(&idx) {
                    self.ids.remove(id);
                }
            }
            self.free.push(idx);
        }
    }

    fn fire_timers(&mut self, now: Instant) {
        while let Some(&Reverse((t, idx))) = self.timers.peek() {
            if t > now {
                break;
            }
            self.timers.pop();
            let Some(c) = self.clients[idx].as_mut() else { continue };
            if c.scheduled != Some(t) {
                continue;
            }
            c.scheduled = None;
            if c.deadline.is_some_and(|d| d <= now) {
                c.conn.on_timeout();
            }
            if !c.dirty {
                c.dirty = true;
                self.dirty.push(idx);
            }
        }
    }
}

fn header(name: &str, value: &str) -> quiche::h3::Header {
    quiche::h3::Header::new(name.as_bytes(), value.as_bytes())
}

/// Reads the requests the connection has received and answers the ones that
/// have arrived whole.
fn process(c: &mut Client, h3config: &quiche::h3::Config, buf: &mut [u8], payload: usize) {
    if c.h3.is_none() && (c.conn.is_established() || c.conn.is_in_early_data()) {
        match quiche::h3::Connection::with_transport(&mut c.conn, h3config) {
            Ok(h3) => c.h3 = Some(h3),
            Err(_) => return,
        }
    }
    let Client { conn, h3, reqs, pending, .. } = c;
    let Some(h3) = h3.as_mut() else { return };
    loop {
        match h3.poll(conn) {
            Ok((sid, quiche::h3::Event::Headers { list, .. })) => {
                let mut echo = false;
                let mut length = payload;
                for h in &list {
                    match h.name() {
                        b":path" => echo = h.value() == ECHO_PATH,
                        b"content-length" => {
                            length = std::str::from_utf8(h.value()).ok().and_then(|v| v.parse().ok()).unwrap_or(payload)
                        }
                        _ => {}
                    }
                }
                reqs.insert(sid, Req { echo, body: Vec::with_capacity(length.min(MAX_BODY)), too_large: false });
            }
            Ok((sid, quiche::h3::Event::Data)) => {
                while let Ok(n) = h3.recv_body(conn, sid, buf) {
                    if let Some(r) = reqs.get_mut(&sid) {
                        if r.body.len() + n > MAX_BODY {
                            r.too_large = true;
                        } else if !r.too_large {
                            r.body.extend_from_slice(&buf[..n]);
                        }
                    }
                }
            }
            Ok((sid, quiche::h3::Event::Finished)) => {
                if let Some(r) = reqs.remove(&sid) {
                    let (status, body) = if r.too_large {
                        ("413", b"request body too large\n".to_vec())
                    } else if r.echo {
                        // The request's own buffer goes back as the response.
                        ("200", r.body)
                    } else {
                        ("404", b"404 page not found\n".to_vec())
                    };
                    let ctype = if status == "200" { "application/octet-stream" } else { "text/plain; charset=utf-8" };
                    let headers = vec![
                        header(":status", status),
                        header("content-type", ctype),
                        header("content-length", &body.len().to_string()),
                    ];
                    respond(h3, conn, pending, sid, headers, body);
                }
            }
            Ok((sid, quiche::h3::Event::Reset(_))) => {
                reqs.remove(&sid);
                pending.retain(|p| p.stream != sid);
            }
            Ok((_, quiche::h3::Event::GoAway)) | Ok((_, quiche::h3::Event::PriorityUpdate)) => {}
            Err(quiche::h3::Error::Done) => break,
            Err(_) => break,
        }
    }
    // Responses waiting for their stream's window, which the client's
    // acknowledgements may just have opened.
    pending.retain_mut(|p| resume(h3, conn, p));
}

fn respond(
    h3: &mut quiche::h3::Connection, conn: &mut quiche::Connection, pending: &mut Vec<Pending>, stream: u64,
    headers: Vec<quiche::h3::Header>, body: Vec<u8>,
) {
    let mut p = Pending { stream, headers: Some(headers), body, off: 0 };
    if resume(h3, conn, &mut p) {
        pending.push(p);
    }
}

/// Sends what is left of a response, and says whether anything still is.
fn resume(h3: &mut quiche::h3::Connection, conn: &mut quiche::Connection, p: &mut Pending) -> bool {
    if let Some(headers) = &p.headers {
        match h3.send_response(conn, p.stream, headers, p.body.is_empty()) {
            Ok(()) => p.headers = None,
            Err(quiche::h3::Error::StreamBlocked) => return true,
            Err(_) => return false,
        }
    }
    if p.body.is_empty() {
        return false;
    }
    match h3.send_body(conn, p.stream, &p.body[p.off..], true) {
        Ok(n) => {
            p.off += n;
            p.off < p.body.len()
        }
        Err(quiche::h3::Error::Done) => true,
        Err(_) => false,
    }
}
