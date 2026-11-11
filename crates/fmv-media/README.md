# Bounded movie decoding

This is an isolated decoder foundation, not a registered FF7 opening or a movie player.
It accepts explicit local FFmpeg/ffprobe executable paths and one owned movie file.
The initial profile supports VP8 video and stereo Vorbis audio, both starting at PTS zero.
No media files belong in this repository.

The worker preflights stream geometry, inventories the decoder's exact frame timestamps,
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

Remaining integration: validate audio-frame PTS/PCM continuity and provide a dedicated,
non-evictable movie voice with an actual consumed-sample clock; upload frames through a
reused GPU texture with an explicitly reviewed display-aspect policy; implement per-thread
PMVIE/MOVIE waits and MVIEF snapshots while concurrent helpers still execute. Original PC
first-frame observation, field/video cadence, audio envelope semantics and camera/video
synchronization remain unverified. This crate does not change any scene, save or admission rule.
