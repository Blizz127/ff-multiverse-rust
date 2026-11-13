//! Decode one explicitly selected local movie. This drains faster than presentation
//! and reports timestamps/decoder EOF; it is deliberately not a movie player.

use fmv_media::{AudioRequirement, Event, Limits, MovieWorker, Request, Tools};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let movie = PathBuf::from(
        args.next()
            .ok_or("usage: movie_decode ABS_MOVIE ABS_FFMPEG ABS_FFPROBE [--optional-audio]")?,
    );
    let ffmpeg = PathBuf::from(args.next().ok_or("missing explicit ffmpeg path")?);
    let ffprobe = PathBuf::from(args.next().ok_or("missing explicit ffprobe path")?);
    // --optional-audio: a silent movie (one VP8 stream, no audio) is decoded instead of refused.
    let requirement = match args.next() {
        None => AudioRequirement::Required,
        Some(flag) if flag == "--optional-audio" => AudioRequirement::Optional,
        Some(_) => return Err("unexpected argument".into()),
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let limits = Limits::default();
    let started = Instant::now();
    let deadline = started + limits.lifetime;
    let mut worker = MovieWorker::start_with(Request {
        path: movie,
        tools: Tools { ffmpeg, ffprobe },
        limits,
    }, requirement)?;
    let mut count = 0usize;
    let mut bytes = 0u64;
    loop {
        if Instant::now() >= deadline {
            return Err("worker deadline reached without successful decoded EOF".into());
        }
        match worker.poll()? {
            Some(Event::Prepared { metadata, audio }) => println!(
                "prepared {}x{}, {} decoded timestamps, timebase {}/{}, PCM {} stereo frames at {} Hz; no playback clock started",
                metadata.video.width, metadata.video.height, metadata.video.frames.len(), metadata.video.time_base.numerator,
                metadata.video.time_base.denominator, audio.as_ref().map_or(0, |a| a.frames()), audio.as_ref().map_or(0, |a| a.sample_rate)),
            Some(Event::Frame(frame)) => {
                if frame.ordinal != count { return Err("noncontiguous decoded frames".into()); }
                if [0, 21, 665, 1592].contains(&frame.ordinal) {
                    println!("decoded frame {}: PTS {} duration {} (timestamp inventory; not an executed script cue)", frame.ordinal, frame.timing.pts, frame.timing.duration);
                }
                count += 1;
                bytes += frame.rgba.len() as u64;
            },
            Some(Event::DecodedEof { frames }) => {
                if count != frames { return Err("decoder EOF count mismatch".into()); }
                println!("decoded EOF: {frames} frames, {bytes} RGBA bytes streamed and dropped, {:.3}s decode wall time; presented EOF remains unacknowledged", started.elapsed().as_secs_f64());
                break;
            },
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    Ok(())
}
