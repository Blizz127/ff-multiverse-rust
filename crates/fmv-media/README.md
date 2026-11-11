# Bounded movie decoding

This is an isolated decoder foundation, not a registered FF7 opening or a movie player.
It accepts explicit local FFmpeg/ffprobe executable paths and one owned movie file.
The initial profile supports VP8 video and stereo Vorbis audio with explicit zero stream-header
starts. The first decoded audio PTS is separate and can differ from its stream-header start.
No media files belong in this repository.

The worker preflights stream geometry, inventories decoded video and audio frame timestamps,
decodes finite stereo PCM, and streams tightly packed RGBA through a bounded queue.
Autorotation and autoscaling are disabled; changing frame dimensions are refused.
Decoding errors, truncated frames, extra bytes, process failure, cancellation and timeout
cannot emit successful decoder EOF. Stderr is drained independently and capped; reads
remain interruptible even when a pipe has not reached EOF. Only directly owned child
processes are killed/reaped. Tool paths must designate trusted local decoders.

`PlaybackTimeline` selects frames using a supplied clock and exact rational PTS comparisons.
Decoder EOF, final-frame presentation and the last video's duration are separate inputs.
The timeline does not invent a clock or assume every field tick advances a video frame.
It proves video completion only; audio completion is a separate future requirement.

`DecodedAudio::metadata` shares an immutable audio inventory with `MediaMetadata::audio`.
Each `AudioFrameTiming` retains the original signed PTS, sample count, and cumulative stereo
sample-frame offset; `DecodedAudio::chunk` returns that timestamp and its exact PCM bytes.
Missing timestamps, invalid counts, reported rate/channel changes, oversized inventories, and a decoded
PCM count different from the sum of all frame counts fail before `Prepared`. Audio PTS gaps,
overlaps, negative starts, and backward observations remain intact. They do not discard an
otherwise successful decode, and no silence, timestamp normalization or asynchronous resampler
correction is applied.
FFprobe 7.0.2 omits per-frame sample rate; the stream rate is required and any reported frame
rate must agree. Exact sample-count reconciliation does not independently prove every possible
dynamic decoder rate change when that per-frame evidence is absent.

`require_contiguous()` is explicit admission to a simple continuous-clock policy. For PTS `p`,
preceding sample count `C`, sample rate `R`, and source timebase `a/b`, the origin observation is
`(p*a*R - C*b)/(R*b)`. The total observation span must fit within one source tick `a/b`.
The closed bound describes timestamp precision only: it neither proves sub-tick continuity
nor selects a rounding rule or exact origin. Applying a tolerance separately to each frame
would let drift accumulate and is deliberately avoided. Consumers still need an explicit
origin/presentation policy; successful admission does not establish hardware-audible timing.

The captured opening, probed with FFmpeg 7.0.2, has 5,299 decoded audio frames totaling
2,632,960 stereo samples (10,531,840 s16le bytes), exactly matching the PCM output. Its first
decoded PTS is 11 ms despite a zero stream-header start. Its cumulative origin span is about
9.54 ms at a 1 ms timebase, so it **does not pass** simple-clock admission. Its Matroska lacing
and Vorbis priming require a separate, evidenced timing policy; this code does not infer a
128-sample offset or treat 11 ms as authored silence. The owned test still requires successful
complete video/audio decoding and separately asserts this incompatibility. Audio and video
endpoints also differ; audio EOF alone cannot stand for movie presentation EOF.

Run synthetic tests with `cargo test -p fmv-media`. The owned acceptance is explicitly ignored
unless invoked with `FMV_OPENING_MOVIE`, `FMV_FFMPEG`, and `FMV_FFPROBE` set to absolute paths:

```sh
cargo test -p fmv-media --test opening_owned -- --ignored --nocapture
cargo run -p fmv-media --example movie_decode -- /absolute/movie /absolute/ffmpeg /absolute/ffprobe
```

The example drains decoded frames and prints metadata; it neither plays sound nor displays
video. The owned acceptance checks all 1,792 frames of the owner's selected opening movie.
Memory limits cover input metadata, decoded PCM, RGBA buffers and the worker queue, not total
external decoder RSS or frames retained by a consumer. Current nonblocking pipes require Unix.

Remaining integration: establish an explicit audio timing/origin policy for the laced opening,
connect timestamped PCM to a dedicated movie voice without confusing mixer consumption with
hardware-audible timing; upload frames through a
reused GPU texture with an explicitly reviewed display-aspect policy; implement per-thread
PMVIE/MOVIE waits and MVIEF snapshots while concurrent helpers still execute. Original PC
first-frame observation, field/video cadence, audio envelope semantics and camera/video
synchronization remain unverified. This crate does not change any scene, save or admission rule.
