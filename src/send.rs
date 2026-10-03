//! Sending packets to one destination, on a thread of its own so a slow or
//! missing receiver never holds up the plugin: packets that can't be sent
//! right away are dropped (it's live audio). TCP connections are made when
//! the first packet comes, and made again after they break.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use trunk_recorder_plugin::{Endpoint, EndpointState, Host, Metrics, State};

/// Packets waiting for a destination; beyond this they're dropped.
const QUEUE: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub struct Dest {
    pub tcp: bool,
    pub host: String,
    pub port: u16,
}

impl Dest {
    /// `udp://host:port` or `tcp://host:port`.
    pub fn parse(url: &str) -> Result<Dest, String> {
        let (tcp, rest) = match url.split_once("://") {
            Some(("udp", r)) => (false, r),
            Some(("tcp", r)) => (true, r),
            _ => return Err(format!("\"{url}\" isn't udp://host:port or tcp://host:port")),
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = rest.rsplit_once(':').ok_or_else(|| format!("\"{url}\" has no port (udp://host:port)"))?;
        let port: u16 = port.parse().ok().filter(|&p| p != 0).ok_or_else(|| format!("\"{port}\" in \"{url}\" isn't a port"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(format!("\"{url}\" has no host"));
        }
        Ok(Dest { tcp, host: host.to_string(), port })
    }

    fn addrs(&self) -> Result<Vec<SocketAddr>, String> {
        let a: Vec<SocketAddr> = (self.host.as_str(), self.port).to_socket_addrs().map_err(|e| format!("can't find {}: {e}", self.host))?.collect();
        if a.is_empty() {
            return Err(format!("can't find {}", self.host));
        }
        Ok(a)
    }
}

impl fmt::Display for Dest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        write!(f, "{}://{host}:{}", if self.tcp { "tcp" } else { "udp" }, self.port)
    }
}

/// Metrics go to the recorder's dashboard at most this often.
const METRICS_EVERY: Duration = Duration::from_secs(10);

/// What one destination has had.
#[derive(Clone, Copy, Default)]
struct Count {
    packets: u64,
    bytes: u64,
    dropped: u64,
}

/// The plugin's status: the destinations it can't send to now, if any —
/// and, for the dashboard, what each has been sent.
#[derive(Clone)]
pub struct Status {
    host: Host,
    problems: Arc<Mutex<BTreeMap<String, String>>>,
    counts: Arc<Mutex<BTreeMap<String, Count>>>,
    reported: Arc<Mutex<Option<Instant>>>,
}

impl Status {
    pub fn new(host: Host) -> Status {
        Status { host, problems: Default::default(), counts: Default::default(), reported: Default::default() }
    }

    fn count(&self, dest: &str, f: impl FnOnce(&mut Count)) {
        f(self.counts.lock().unwrap().entry(dest.to_string()).or_default());
        let mut r = self.reported.lock().unwrap();
        if r.is_none_or(|t| t.elapsed() >= METRICS_EVERY) {
            *r = Some(Instant::now());
            drop(r);
            self.host.metrics(&self.metrics());
        }
    }

    /// The dashboard's figures: bytes sent, each destination's state, packets sent and dropped.
    pub fn metrics(&self) -> Metrics {
        let counts = self.counts.lock().unwrap().clone();
        let problems = self.problems.lock().unwrap().clone();
        let total = |f: fn(&Count) -> u64| counts.values().map(f).sum::<u64>();
        let endpoints = counts
            .iter()
            .map(|(dest, c)| {
                let problem = problems.get(dest);
                let state = match problem {
                    Some(_) => EndpointState::Down,
                    None if c.packets > 0 => EndpointState::Up,
                    None => EndpointState::Unknown,
                };
                Endpoint { name: dest.clone(), state, last_error: problem.cloned().unwrap_or_default(), ..Default::default() }
            })
            .collect();
        let mut extra = BTreeMap::new();
        extra.insert("packetsSent".to_string(), total(|c| c.packets).into());
        extra.insert("packetsDropped".to_string(), total(|c| c.dropped).into());
        Metrics { bytes_sent: Some(total(|c| c.bytes)), endpoints, extra, ..Default::default() }
    }

    fn set(&self, dest: &str, problem: Option<String>) {
        let mut p = self.problems.lock().unwrap();
        if p.get(dest) == problem.as_ref() {
            return;
        }
        match &problem {
            Some(why) => {
                self.host.warn(why.clone());
                p.insert(dest.to_string(), why.clone());
            }
            None => {
                self.host.info(format!("sending to {dest}"));
                p.remove(dest);
            }
        }
        if p.is_empty() {
            self.host.status(State::Ok, "");
        } else {
            self.host.status(State::Warning, p.values().cloned().collect::<Vec<_>>().join("; "));
        }
    }
}

pub struct Sender {
    tx: SyncSender<Vec<u8>>,
    thread: JoinHandle<()>,
    name: String,
    status: Status,
}

impl Sender {
    pub fn start(dest: Dest, status: Status) -> Sender {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let (name, st) = (dest.to_string(), status.clone());
        let thread = std::thread::Builder::new().name(format!("send {dest}")).spawn(move || run(&dest, rx, &st)).expect("thread");
        Sender { tx, thread, name, status }
    }

    /// Queue a packet (dropped when the destination is behind).
    pub fn send(&self, p: Vec<u8>) {
        if self.tx.try_send(p).is_err() {
            self.status.count(&self.name, |c| c.dropped += 1);
        }
    }

    /// Send what's queued, until `deadline`.
    pub fn finish(self, deadline: Instant) {
        drop(self.tx);
        while !self.thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.thread.is_finished() {
            let _ = self.thread.join();
        }
    }
}

fn run(dest: &Dest, rx: Receiver<Vec<u8>>, status: &Status) {
    let name = dest.to_string();
    let mut retry_at = Instant::now();
    if dest.tcp {
        let mut conn: Option<TcpStream> = None;
        for p in rx {
            if conn.is_none() && Instant::now() >= retry_at {
                match connect(dest) {
                    Ok(c) => {
                        status.set(&name, None);
                        conn = Some(c);
                    }
                    Err(e) => {
                        status.set(&name, Some(format!("can't connect to {name}: {e}")));
                        retry_at = Instant::now() + Duration::from_secs(5);
                    }
                }
            }
            if let Some(c) = conn.as_mut() {
                match c.write_all(&p) {
                    Ok(()) => status.count(&name, |c| {
                        c.packets += 1;
                        c.bytes += p.len() as u64;
                    }),
                    Err(e) => {
                        status.set(&name, Some(format!("{name}: {e}")));
                        conn = None;
                        retry_at = Instant::now() + Duration::from_secs(1);
                    }
                }
            }
        }
        if let Some(c) = conn {
            let _ = c.shutdown(Shutdown::Both);
        }
    } else {
        let mut target: Option<(UdpSocket, SocketAddr)> = None;
        for p in rx {
            if target.is_none() && Instant::now() >= retry_at {
                match udp(dest) {
                    Ok(t) => target = Some(t),
                    Err(e) => {
                        status.set(&name, Some(e));
                        retry_at = Instant::now() + Duration::from_secs(30);
                    }
                }
            }
            if let Some((s, to)) = &target {
                match s.send_to(&p, to) {
                    Ok(_) => {
                        status.set(&name, None);
                        status.count(&name, |c| {
                            c.packets += 1;
                            c.bytes += p.len() as u64;
                        });
                    }
                    Err(e) => status.set(&name, Some(format!("{name}: {e}"))),
                }
            }
        }
    }
}

fn connect(dest: &Dest) -> Result<TcpStream, String> {
    let mut last = String::new();
    for a in dest.addrs()? {
        match TcpStream::connect_timeout(&a, Duration::from_secs(3)) {
            Ok(c) => {
                let _ = c.set_nodelay(true);
                let _ = c.set_write_timeout(Some(Duration::from_secs(2)));
                return Ok(c);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn udp(dest: &Dest) -> Result<(UdpSocket, SocketAddr), String> {
    let to = dest.addrs()?[0];
    let local = if to.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let s = UdpSocket::bind(local).map_err(|e| format!("can't open a UDP socket: {e}"))?;
    Ok((s, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(Dest::parse("udp://127.0.0.1:9123").unwrap(), Dest { tcp: false, host: "127.0.0.1".into(), port: 9123 });
        assert_eq!(Dest::parse("tcp://audio.local:9000/").unwrap(), Dest { tcp: true, host: "audio.local".into(), port: 9000 });
        assert_eq!(Dest::parse("udp://[::1]:5").unwrap().host, "::1");
        assert_eq!(Dest::parse("udp://[::1]:5").unwrap().to_string(), "udp://[::1]:5");
        assert!(Dest::parse("127.0.0.1:9123").is_err());
        assert!(Dest::parse("udp://host").is_err());
        assert!(Dest::parse("udp://host:0").is_err());
        assert!(Dest::parse("udp://:5").is_err());
    }
}
