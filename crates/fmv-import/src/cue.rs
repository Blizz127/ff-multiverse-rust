//! Minimal CUE sheet parser: just enough to find the data track and the file that holds it.
//! Untrusted input: size-capped by the caller, no panics, bounded line and track counts.

use std::path::{Path, PathBuf};

/// Largest cue sheet the importer will read.
pub const MAX_CUE_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 20_000;
const MAX_TRACKS: usize = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackMode {
    Audio,
    /// MODE1/2048 (cooked)
    Mode1Cooked,
    /// MODE1/2352 (raw)
    Mode1Raw,
    /// MODE2/2352 (raw, CD-XA)
    Mode2Raw,
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTrack {
    pub number: u32,
    pub mode: TrackMode,
    /// The FILE the track belongs to, exactly as written in the cue sheet.
    pub file: String,
    /// INDEX 01 position in frames (sectors) from the start of the file.
    pub index01_frames: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CueSheet {
    pub tracks: Vec<CueTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CueError {
    #[error("cue sheet is too large or has too many lines/tracks")]
    TooLarge,
    #[error("cue sheet line {line}: {msg}")]
    Syntax { line: usize, msg: &'static str },
    #[error("cue sheet has no tracks")]
    NoTracks,
    #[error("track {number} is not a supported data track ({mode}); only MODE1/2048, MODE1/2352 and MODE2/2352 data tracks can be imported")]
    NotData { number: u32, mode: String },
    #[error("cue sheet FILE entry {0:?} has no usable file name")]
    BadFileName(String),
}

fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut have = false;
    for c in line.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                have = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// `mm:ss:ff` to a frame count.
fn parse_msf(s: &str) -> Option<u32> {
    let mut it = s.split(':');
    let mm: u32 = it.next()?.parse().ok()?;
    let ss: u32 = it.next()?.parse().ok()?;
    let ff: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || ss >= 60 || ff >= 75 {
        return None;
    }
    mm.checked_mul(60)?
        .checked_add(ss)?
        .checked_mul(75)?
        .checked_add(ff)
}

pub fn parse_cue(text: &str) -> Result<CueSheet, CueError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut sheet = CueSheet::default();
    let mut current_file: Option<String> = None;
    for (i, raw) in text.split(['\r', '\n']).enumerate() {
        if i >= MAX_LINES {
            return Err(CueError::TooLarge);
        }
        let line = i + 1;
        let toks = tokenize(raw);
        let Some(keyword) = toks.first() else {
            continue;
        };
        match keyword.to_ascii_uppercase().as_str() {
            "FILE" => {
                let name = toks.get(1).ok_or(CueError::Syntax {
                    line,
                    msg: "FILE without a name",
                })?;
                current_file = Some(name.clone());
            }
            "TRACK" => {
                let number =
                    toks.get(1)
                        .and_then(|t| t.parse::<u32>().ok())
                        .ok_or(CueError::Syntax {
                            line,
                            msg: "TRACK without a number",
                        })?;
                let mode_tok = toks
                    .get(2)
                    .ok_or(CueError::Syntax {
                        line,
                        msg: "TRACK without a mode",
                    })?
                    .to_ascii_uppercase();
                let mode = match mode_tok.as_str() {
                    "AUDIO" => TrackMode::Audio,
                    "MODE1/2048" => TrackMode::Mode1Cooked,
                    "MODE1/2352" => TrackMode::Mode1Raw,
                    "MODE2/2352" => TrackMode::Mode2Raw,
                    other => TrackMode::Unsupported(other.chars().take(32).collect()),
                };
                let file = current_file.clone().ok_or(CueError::Syntax {
                    line,
                    msg: "TRACK before any FILE",
                })?;
                if sheet.tracks.len() >= MAX_TRACKS {
                    return Err(CueError::TooLarge);
                }
                sheet.tracks.push(CueTrack {
                    number,
                    mode,
                    file,
                    index01_frames: None,
                });
            }
            "INDEX" => {
                let idx = toks.get(1).and_then(|t| t.parse::<u32>().ok());
                let pos = toks.get(2).and_then(|t| parse_msf(t));
                match (idx, pos) {
                    (Some(1), Some(p)) => {
                        let track = sheet.tracks.last_mut().ok_or(CueError::Syntax {
                            line,
                            msg: "INDEX before any TRACK",
                        })?;
                        track.index01_frames = Some(p);
                    }
                    (Some(_), Some(_)) => {}
                    _ => {
                        return Err(CueError::Syntax {
                            line,
                            msg: "malformed INDEX line",
                        })
                    }
                }
            }
            _ => {} // REM, TITLE, PERFORMER, PREGAP, FLAGS, CATALOG, ...
        }
    }
    if sheet.tracks.is_empty() {
        return Err(CueError::NoTracks);
    }
    Ok(sheet)
}

impl CueSheet {
    /// Track 1 (or the first track if numbering starts elsewhere), which must be a data track.
    pub fn data_track(&self) -> Result<&CueTrack, CueError> {
        let track = self
            .tracks
            .iter()
            .find(|t| t.number == 1)
            .or_else(|| self.tracks.first())
            .ok_or(CueError::NoTracks)?;
        match &track.mode {
            TrackMode::Audio => Err(CueError::NotData {
                number: track.number,
                mode: "AUDIO".to_string(),
            }),
            TrackMode::Unsupported(m) => Err(CueError::NotData {
                number: track.number,
                mode: m.clone(),
            }),
            _ => Ok(track),
        }
    }
}

/// Resolve a cue `FILE` name next to the cue sheet. Only the last path component is used (cue
/// sheets written on Windows often carry absolute paths), which also rules out `..` traversal.
pub fn resolve_cue_file(cue_path: &Path, name: &str) -> Result<PathBuf, CueError> {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        return Err(CueError::BadFileName(name.chars().take(80).collect()));
    }
    let dir = cue_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(dir.join(leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_ps1_cue() {
        let cue = "FILE \"Final Fantasy VII (Disc 1).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n";
        let sheet = parse_cue(cue).unwrap();
        let t = sheet.data_track().unwrap();
        assert_eq!(t.number, 1);
        assert_eq!(t.mode, TrackMode::Mode2Raw);
        assert_eq!(t.file, "Final Fantasy VII (Disc 1).bin");
        assert_eq!(t.index01_frames, Some(0));
    }

    #[test]
    fn handles_bom_cr_only_unquoted_names_and_audio_tracks() {
        let cue = "\u{feff}REM GENRE x\rFILE disc.bin BINARY\r TRACK 01 MODE1/2352\r INDEX 00 00:00:00\r INDEX 01 00:02:00\r TRACK 02 AUDIO\r INDEX 01 59:59:74\r";
        let sheet = parse_cue(cue).unwrap();
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.data_track().unwrap().mode, TrackMode::Mode1Raw);
        assert_eq!(sheet.data_track().unwrap().index01_frames, Some(150));
        assert_eq!(sheet.tracks[1].mode, TrackMode::Audio);
        let cooked =
            parse_cue("FILE \"a.iso\" BINARY\nTRACK 1 MODE1/2048\nINDEX 01 00:00:00").unwrap();
        assert_eq!(cooked.data_track().unwrap().mode, TrackMode::Mode1Cooked);
    }

    #[test]
    fn multi_file_sheets_keep_each_tracks_file() {
        let cue = "FILE \"t1.bin\" BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\nFILE \"t2.wav\" WAVE\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n";
        let sheet = parse_cue(cue).unwrap();
        assert_eq!(sheet.tracks[0].file, "t1.bin");
        assert_eq!(sheet.tracks[1].file, "t2.wav");
    }

    #[test]
    fn rejects_audio_first_and_unsupported_modes() {
        let audio = parse_cue("FILE a.bin BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00").unwrap();
        assert!(matches!(audio.data_track(), Err(CueError::NotData { .. })));
        let odd = parse_cue("FILE a.bin BINARY\nTRACK 01 MODE2/2336\nINDEX 01 00:00:00").unwrap();
        assert!(matches!(odd.data_track(), Err(CueError::NotData { .. })));
    }

    #[test]
    fn rejects_malformed_sheets_without_panicking() {
        for bad in [
            "",
            "REM only a comment",
            "TRACK 01 MODE2/2352",
            "FILE",
            "FILE a.bin BINARY\nTRACK",
            "FILE a.bin BINARY\nTRACK x MODE2/2352",
            "FILE a.bin BINARY\nTRACK 01",
            "FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01",
            "FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:99:00",
            "FILE a.bin BINARY\nTRACK 01 MODE2/2352\nINDEX 01 99999999999:00:00",
            "FILE a.bin BINARY\nINDEX 01 00:00:00",
            "FILE \"unterminated",
        ] {
            assert!(parse_cue(bad).is_err(), "{bad:?} should fail");
        }
        let many = "FILE a.bin BINARY\n".to_string() + &"TRACK 01 MODE2/2352\n".repeat(100);
        assert_eq!(parse_cue(&many), Err(CueError::TooLarge));
        assert_eq!(
            parse_cue(&"\n".repeat(MAX_LINES + 5)),
            Err(CueError::TooLarge)
        );
    }

    #[test]
    fn cue_file_names_resolve_next_to_the_cue_only() {
        let cue = Path::new("/images/ff7/disc1.cue");
        assert_eq!(
            resolve_cue_file(cue, "disc1.bin").unwrap(),
            Path::new("/images/ff7/disc1.bin")
        );
        // absolute Windows path: only the leaf survives
        assert_eq!(
            resolve_cue_file(cue, "C:\\isos\\ff7\\disc1.bin").unwrap(),
            Path::new("/images/ff7/disc1.bin")
        );
        // traversal is flattened to the leaf
        assert_eq!(
            resolve_cue_file(cue, "../../etc/passwd").unwrap(),
            Path::new("/images/ff7/passwd")
        );
        assert!(resolve_cue_file(cue, "").is_err());
        assert!(resolve_cue_file(cue, "dir/").is_err());
        assert!(resolve_cue_file(cue, "..").is_err());
        assert_eq!(
            resolve_cue_file(Path::new("disc.cue"), "x.bin").unwrap(),
            Path::new("./x.bin")
        );
    }
}
