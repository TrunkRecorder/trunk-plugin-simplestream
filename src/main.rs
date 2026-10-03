//! simplestream — a Trunk Recorder Pro plugin that streams the audio of calls
//! as they're recorded (16-bit PCM, mono) to other programs over UDP or TCP,
//! as Trunk Recorder's simplestream plugin does, in the same packet formats.

mod send;

use std::collections::HashMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trunk_recorder_plugin::{base64, topic, AudioChunk, CallInfo, Host, Manifest, Plugin, Setup};

use send::{Dest, Sender};

#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(default)]
struct Config {
    /// Streams
    ///
    /// Where to send audio, and which. Audio is 8 kHz, 16-bit, mono, little-endian.
    streams: Vec<Stream>,
}

/// Stream
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default)]
#[serde(default)]
struct Stream {
    /// Send to
    ///
    /// udp://host:port or tcp://host:port
    url: String,
    /// Talkgroup
    ///
    /// The talkgroup to stream (or a call it's patched into). 0 streams every talkgroup.
    #[serde(rename = "TGID")]
    tgid: u32,
    /// System
    ///
    /// The short name of the system to stream. Leave it empty for every system.
    #[serde(rename = "shortName")]
    #[schemars(extend("x-system" = true))]
    short_name: String,
    /// Talkgroup in each packet
    ///
    /// Put the talkgroup number (4 bytes, little-endian) before the audio in each packet.
    #[serde(rename = "sendTGID")]
    send_tgid: bool,
    /// JSON in each packet
    ///
    /// Put the call's details (JSON, after its length in 4 bytes, little-endian) before the audio in each packet, instead of the talkgroup.
    #[serde(rename = "sendJSON")]
    send_json: bool,
    /// Packets when calls start
    ///
    /// With JSON: also send a packet of JSON (and no audio) when a call starts.
    #[serde(rename = "sendCallStart")]
    send_call_start: bool,
    /// Packets when calls end
    ///
    /// With JSON: also send a packet of JSON (and no audio) when a call ends.
    #[serde(rename = "sendCallEnd")]
    send_call_end: bool,
    // Trunk Recorder's settings from before `url`.
    #[schemars(skip)]
    address: String,
    #[schemars(skip)]
    port: u16,
    #[serde(rename = "useTCP")]
    #[schemars(skip)]
    use_tcp: bool,
}

impl Stream {
    fn dest(&self) -> Result<Dest, String> {
        if self.url.trim().is_empty() && !self.address.is_empty() {
            return Ok(Dest { tcp: self.use_tcp, host: self.address.clone(), port: self.port });
        }
        Dest::parse(self.url.trim())
    }

    fn wants_system(&self, short_name: &str) -> bool {
        self.short_name.is_empty() || self.short_name == short_name
    }

    /// The talkgroup it streams that the call is on (or patched into), if any.
    fn talkgroup_of(&self, talkgroups: &[u32]) -> Option<u32> {
        if self.tgid == 0 {
            return talkgroups.first().copied();
        }
        talkgroups.contains(&self.tgid).then_some(self.tgid)
    }
}

struct Out {
    stream: Stream,
    sender: Sender,
}

struct SimpleStream {
    outs: Vec<Out>,
    /// The calls recording now.
    calls: HashMap<u32, CallInfo>,
}

impl Plugin for SimpleStream {
    type Config = Config;
    type SystemConfig = trunk_recorder_plugin::NoConfig;

    fn manifest() -> Manifest {
        Manifest {
            name: "simplestream".into(),
            subscribe: vec![topic::AUDIO.into(), topic::CALL_START.into(), topic::CALL_END.into()],
            ..trunk_recorder_plugin::manifest!()
        }
    }

    fn start(host: Host, setup: Setup<Config, Self::SystemConfig>) -> Result<Self, String> {
        if setup.config.streams.is_empty() {
            return Err("Add a stream: where to send audio, and which talkgroup's.".into());
        }
        let status = send::Status::new(host.clone());
        let mut outs = Vec::new();
        for (i, s) in setup.config.streams.iter().enumerate() {
            let dest = s.dest().map_err(|e| format!("Stream {}: {e}", i + 1))?;
            if !s.short_name.is_empty() && setup.system_named(&s.short_name).is_none() {
                host.warn(format!("stream {}: there's no system called {}", i + 1, s.short_name));
            }
            let what = if s.tgid == 0 { "every talkgroup".to_string() } else { format!("talkgroup {}", s.tgid) };
            let of = if s.short_name.is_empty() { String::new() } else { format!(" of {}", s.short_name) };
            host.info(format!("streaming {what}{of} to {dest}"));
            outs.push(Out { stream: s.clone(), sender: Sender::start(dest, status.clone()) });
        }
        Ok(SimpleStream { outs, calls: HashMap::new() })
    }

    fn call_start(&mut self, call: CallInfo) {
        if !call.recording {
            return;
        }
        let short_name = call.short_name.clone();
        let talkgroups = talkgroups(&call);
        for o in self.outs.iter().filter(|o| o.stream.send_json && o.stream.send_call_start && o.stream.wants_system(&short_name)) {
            if o.stream.talkgroup_of(&talkgroups).is_some() {
                let tags: Vec<&str> = if call.talkgroup_tag.is_empty() { vec![] } else { vec![call.talkgroup_tag.as_str()] };
                let j = json!({
                    "src": src(&call),
                    "src_tag": "",
                    "talkgroup": call.talkgroup,
                    "talkgroup_tag": call.talkgroup_tag,
                    "patched_talkgroups": talkgroups,
                    "patched_talkgroup_tags": tags,
                    "freq": call.freq_hz,
                    "short_name": short_name,
                    "event": "call_start",
                });
                o.sender.send(packet(Some(&j), None, &[]));
            }
        }
        self.calls.insert(call.id, call);
    }

    fn audio(&mut self, chunk: AudioChunk) {
        let pcm = base64::decode(&chunk.pcm);
        let call = self.calls.get(&chunk.call_id);
        let short_name = chunk.short_name.as_str();
        let talkgroups = call.map_or(vec![chunk.talkgroup], talkgroups);
        for o in self.outs.iter().filter(|o| o.stream.wants_system(short_name)) {
            let Some(tg) = o.stream.talkgroup_of(&talkgroups) else { continue };
            let p = if o.stream.send_json {
                let j = json!({
                    "src": call.map_or(-1, src),
                    "src_tag": "",
                    "talkgroup": tg,
                    "patched_talkgroups": talkgroups,
                    "freq": call.map_or(0, |c| c.freq_hz),
                    "short_name": short_name,
                    "audio_sample_rate": chunk.sample_rate,
                    "event": "audio",
                });
                packet(Some(&j), None, &pcm)
            } else {
                packet(None, o.stream.send_tgid.then_some(tg), &pcm)
            };
            o.sender.send(p);
        }
    }

    fn call_end(&mut self, call: CallInfo) {
        let Some(started) = self.calls.remove(&call.id) else { return };
        let short_name = call.short_name.clone();
        // Patches heard during the call, as well as those at its start.
        let mut tgs = talkgroups(&call);
        for t in talkgroups(&started) {
            if !tgs.contains(&t) {
                tgs.push(t);
            }
        }
        for o in self.outs.iter().filter(|o| o.stream.send_json && o.stream.send_call_end && o.stream.wants_system(&short_name)) {
            if o.stream.talkgroup_of(&tgs).is_some() {
                let j = json!({
                    "talkgroup": call.talkgroup,
                    "patched_talkgroups": tgs,
                    "freq": call.freq_hz,
                    "short_name": short_name,
                    "event": "call_end",
                });
                o.sender.send(packet(Some(&j), None, &[]));
            }
        }
    }

    fn shutdown(&mut self, grace: Duration) {
        let deadline = std::time::Instant::now() + grace;
        for o in self.outs.drain(..) {
            o.sender.finish(deadline);
        }
    }
}

/// The call's talkgroup, then those patched with it (Trunk Recorder's `patched_talkgroups`: never empty).
fn talkgroups(c: &CallInfo) -> Vec<u32> {
    let mut v = vec![c.talkgroup];
    v.extend(c.patched_talkgroups.iter().filter(|&&t| t != c.talkgroup));
    v
}

/// The radio talking (the last heard), or -1.
fn src(c: &CallInfo) -> i64 {
    c.units.last().map_or(-1, |&u| u as i64)
}

/// A packet: [JSON length (u32 LE), JSON] or [talkgroup (u32 LE)], then the samples.
fn packet(j: Option<&Value>, tgid: Option<u32>, pcm: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(pcm.len() + 256);
    if let Some(j) = j {
        let s = j.to_string();
        p.extend_from_slice(&(s.len() as u32).to_le_bytes());
        p.extend_from_slice(s.as_bytes());
    } else if let Some(tg) = tgid {
        p.extend_from_slice(&tg.to_le_bytes());
    }
    p.extend_from_slice(pcm);
    p
}

fn main() {
    trunk_recorder_plugin::run::<SimpleStream>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, UdpSocket};
    use trunk_recorder_plugin::testing;
    use trunk_recorder_plugin::{HostMessage, EXIT_CONFIG};

    fn call(id: u32, tg: u32, patched: Vec<u32>) -> CallInfo {
        CallInfo {
            id,
            system: 0,
            short_name: "sys1".into(),
            talkgroup: tg,
            talkgroup_tag: format!("TG {tg}"),
            freq_hz: 851012500,
            recording: true,
            units: vec![1234],
            patched_talkgroups: patched,
            ..Default::default()
        }
    }

    fn audio(id: u32, tg: u32, samples: &[i16]) -> HostMessage {
        HostMessage::Audio(AudioChunk { short_name: "sys1".into(), ..AudioChunk::new(id, 0, tg, 8000, samples) })
    }

    fn hello(streams: Value) -> HostMessage {
        HostMessage::Hello(testing::hello(&testing::temp_dir("simplestream"), json!({ "streams": streams })))
    }

    fn udp() -> (UdpSocket, String) {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let url = format!("udp://{}", s.local_addr().unwrap());
        (s, url)
    }

    fn recv(s: &UdpSocket) -> Option<Vec<u8>> {
        let mut b = [0u8; 65536];
        s.recv(&mut b).ok().map(|n| b[..n].to_vec())
    }

    /// (JSON, the rest) of a JSON packet.
    fn split(p: &[u8]) -> (Value, Vec<u8>) {
        let n = u32::from_le_bytes(p[..4].try_into().unwrap()) as usize;
        (serde_json::from_slice(&p[4..4 + n]).unwrap(), p[4 + n..].to_vec())
    }

    #[test]
    fn plain_audio_of_one_talkgroup() {
        let (s, url) = udp();
        let out = testing::run::<SimpleStream>([
            hello(json!([{ "url": url, "TGID": 101 }])),
            HostMessage::CallStart(call(1, 101, vec![])),
            HostMessage::CallStart(call(2, 202, vec![])),
            audio(2, 202, &[9, 9]),
            audio(1, 101, &[1, -2, 3]),
        ]);
        assert!(out.ready(), "{:?}", out.messages);
        assert_eq!(recv(&s).unwrap(), [1, 0, 0xfe, 0xff, 3, 0]);
        s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        assert!(recv(&s).is_none());
    }

    #[test]
    fn talkgroup_before_the_audio() {
        let (s, url) = udp();
        testing::run::<SimpleStream>([hello(json!([{ "url": url, "TGID": 0, "sendTGID": true }])), audio(7, 58914, &[5])]);
        assert_eq!(recv(&s).unwrap(), [0x22, 0xe6, 0, 0, 5, 0]);
    }

    /// The dashboard hears what was sent where.
    #[test]
    fn metrics_say_what_was_sent() {
        let (s, url) = udp();
        let out = testing::run::<SimpleStream>([hello(json!([{ "url": url, "TGID": 0, "shortName": "sys1" }])), audio(1, 101, &[1, 2, 3])]);
        assert!(recv(&s).is_some());
        let m = out.metrics();
        let last = m.last().expect("a metrics message");
        assert_eq!(last.endpoints[0].name, url);
        assert_eq!(last.endpoints[0].state, trunk_recorder_plugin::EndpointState::Up);
        assert!(last.bytes_sent.unwrap() > 0);
        assert_eq!(last.extra["packetsSent"], 1);
    }

    #[test]
    fn json_and_call_events() {
        let (s, url) = udp();
        let mut end = call(1, 101, vec![101, 202]);
        end.units = vec![1234, 5678];
        testing::run::<SimpleStream>([
            hello(json!([{ "url": url, "TGID": 202, "shortName": "sys1", "sendJSON": true, "sendCallStart": true, "sendCallEnd": true }])),
            HostMessage::CallStart(call(1, 101, vec![101, 202])),
            audio(1, 101, &[1]),
            HostMessage::CallEnd(end),
        ]);
        let (start, rest) = split(&recv(&s).unwrap());
        assert!(rest.is_empty());
        assert_eq!(start["event"], "call_start");
        assert_eq!(start["talkgroup"], 101);
        assert_eq!(start["patched_talkgroups"], json!([101, 202]));
        assert_eq!(start["src"], 1234);
        assert_eq!(start["short_name"], "sys1");
        let (a, pcm) = split(&recv(&s).unwrap());
        assert_eq!(a["event"], "audio");
        // The talkgroup this stream is for.
        assert_eq!(a["talkgroup"], 202);
        assert_eq!(a["audio_sample_rate"], 8000);
        assert_eq!(a["freq"], 851012500u64);
        assert_eq!(pcm, [1, 0]);
        let (e, _) = split(&recv(&s).unwrap());
        assert_eq!(e["event"], "call_end");
        assert_eq!(e["patched_talkgroups"], json!([101, 202]));
    }

    #[test]
    fn other_systems_are_left_out() {
        let (s, url) = udp();
        testing::run::<SimpleStream>([hello(json!([{ "url": url, "TGID": 0, "shortName": "elsewhere" }])), audio(1, 101, &[1])]);
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        assert!(recv(&s).is_none());
    }

    #[test]
    fn tcp_and_trunk_recorders_old_settings() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = Vec::new();
            c.read_to_end(&mut b).unwrap();
            b
        });
        testing::run::<SimpleStream>([
            hello(json!([{ "address": "127.0.0.1", "port": port, "useTCP": true, "TGID": 101 }])),
            audio(1, 101, &[1, 2]),
            audio(1, 101, &[3]),
        ]);
        assert_eq!(got.join().unwrap(), [1, 0, 2, 0, 3, 0]);
    }

    #[test]
    fn bad_settings() {
        for streams in [json!([]), json!([{ "url": "http://x:1" }]), json!([{ "url": "udp://x" }])] {
            let out = testing::run::<SimpleStream>([hello(streams)]);
            assert_eq!(out.exit_code, EXIT_CONFIG);
        }
    }

    #[test]
    fn the_settings_form() {
        let c = trunk_recorder_plugin::describe::<SimpleStream>().config.unwrap();
        let items = &c["properties"]["streams"]["items"];
        assert_eq!(c["properties"]["streams"]["type"], "array");
        assert_eq!(items["type"], "object");
        assert_eq!(items["title"], "Stream");
        assert_eq!(items["x-order"], json!(["url", "TGID", "shortName", "sendTGID", "sendJSON", "sendCallStart", "sendCallEnd"]));
    }
}
