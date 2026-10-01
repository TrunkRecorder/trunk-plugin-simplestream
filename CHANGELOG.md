# Changelog

## [0.1.0]

- Streams recording calls' audio over UDP or TCP in Trunk Recorder's
  simplestream packet formats: plain audio, talkgroup first, or JSON first,
  with optional call start and end packets.
- Streams by talkgroup (including calls patched with it) and system.
- Reconnects TCP streams that break; drops audio rather than falling behind.
- Reads Trunk Recorder's settings, including `address`, `port` and `useTCP`.
