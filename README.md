# simplestream for Trunk Recorder Pro

Streams the audio of calls as [Trunk Recorder Pro](https://github.com/TrunkRecorder/trunk-recorder-lite)
records them to other programs, over UDP or TCP. It does what Trunk
Recorder's simplestream plugin does, in the same packet formats, so programs
written for that work with this.

The audio is raw PCM: 16-bit, mono, little-endian, 8 kHz (analog channels
too). That's about 128 kbit/s per call, which is fine on one computer or a
home network but not meant for the internet: the programs receiving it might
play it, mix it, or compress it and stream it on.

## Settings

A list of **streams**. Each sends some of the audio to one place:

| Setting | |
|---|---|
| **Send to** | `udp://host:port` or `tcp://host:port`. |
| **Talkgroup** | The talkgroup to stream. Calls patched with it count too. `0` streams every talkgroup. |
| **System** | The short name of the system to stream. Leave it empty for every system. |
| **Talkgroup in each packet** | Put the talkgroup before the audio in each packet. |
| **JSON in each packet** | Put the call's details before the audio in each packet, instead of the talkgroup. |
| **Packets when calls start** | With JSON: also send a packet of JSON, with no audio, when a call starts. |
| **Packets when calls end** | With JSON: also send one when a call ends. |

Talkgroup numbers repeat between systems, so stream each system to a port of
its own, or turn on JSON (which names the system).

## Packets

Each packet is one slice of audio, with, depending on the settings:

- **Nothing before it.**
- **The talkgroup:** 4 bytes, little-endian, then the audio.
- **JSON:** its length in 4 bytes, little-endian, then the JSON, then the audio:

  ```json
  {"audio_sample_rate":8000,"event":"audio","freq":851012500,"patched_talkgroups":[101],
   "short_name":"county","src":1234,"src_tag":"","talkgroup":101}
  ```

  A call start is the same with `"event":"call_start"` (plus `talkgroup_tag`
  and `patched_talkgroup_tags`) and no audio; a call end has `talkgroup`,
  `patched_talkgroups`, `freq`, `short_name` and `"event":"call_end"`.

`talkgroup` in an audio packet is the talkgroup the stream is for, which is
the patched talkgroup when the call is on another. `src` is the last radio
heard when the call started (`-1` when none was).

`examples/example_audio_player.py` (from Trunk Recorder) plays a UDP stream.
To play one through PulseAudio on Linux, load its TCP module and stream to it:

```sh
pacmd load-module module-simple-protocol-tcp sink=1 playback=true port=9125 format=s16le rate=8000 channels=1
```

with **Send to** `tcp://127.0.0.1:9125`.

## What it does when it can't send

Audio that can't be sent right away is dropped: it's live. A TCP stream
connects when its first audio comes, and connects again (every few seconds)
after the connection breaks; until then the plugin shows a warning.

## Differences from Trunk Recorder

- With **Talkgroup** `0`, a patched call is sent once, not once for each
  talkgroup in the patch.
- `src_tag` is always empty.
- Calls end when the recorder ends them, a little before their files are
  written (where Trunk Recorder sent `call_end`).

## Coming from Trunk Recorder

Trunk Recorder's `streams` can be pasted in as they are, including the older
`address`, `port` and `useTCP` settings. The recorder sends live audio to
plugins that ask for it: there's no `audioStreaming` setting to turn on.

## Building

```sh
cargo build --release
```

## License

GPL-3.0-or-later, like Trunk Recorder.
