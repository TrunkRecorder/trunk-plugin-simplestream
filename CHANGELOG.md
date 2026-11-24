# Changelog

## [0.1.7]

- Audio packets with JSON now carry `patched_talkgroup_tags` (the call's own talkgroup's tag, then those of the
  talkgroups patched with it; none when no tag is known), as Trunk Recorder's simplestream can, so a receiver that
  names a transmission by them needs no call start packets. A call start's list now has every patched talkgroup's tag,
  not only the call's own.
- `src` and `src_tag` (the radio talking, and its name, from the call's sources) follow the call as it's updated,
  as Trunk Recorder's simplestream does, instead of staying as they were when the call started. `src_tag` was
  always empty before.
- Built with trunk-recorder-plugin 0.15.1, which names the talkgroups patched with a call (`CallInfo.patched`).

## [0.1.5]

- Built with trunk-recorder-plugin 0.5.0, for Trunk Recorder Pro v0.2.0: systems are numbered
  from 0 there, conventional ones too, and the call record names every field the recorder writes.

## [0.1.4]

- A system's sites: Trunk Recorder Pro now has systems with several sites, and
  a call heard on more than one starts and ends once, its live audio coming
  from whichever site's copy has it. Audio from a copy that wasn't the one
  that started now still finds its call (its patched talkgroups, radio and
  frequency), also when that first copy wasn't recorded.
- Built with trunk-recorder-plugin 0.4.0.

## [0.1.3]

- A system can have streams of its own (the recorder's per-system override of the streams); they carry only its audio, and the recorder-wide streams no longer carry it.
- Built with trunk-recorder-plugin 0.3.0.

## [0.1.1]

- A stream's system is chosen from a menu of the recorder's systems, and follows a system when it's renamed.
- Built with trunk-recorder-plugin 0.1.1.

## [0.1.0]

- Streams recording calls' audio over UDP or TCP in Trunk Recorder's
  simplestream packet formats: plain audio, talkgroup first, or JSON first,
  with optional call start and end packets.
- Streams by talkgroup (including calls patched with it) and system.
- Reconnects TCP streams that break; drops audio rather than falling behind.
- Reads Trunk Recorder's settings, including `address`, `port` and `useTCP`.
