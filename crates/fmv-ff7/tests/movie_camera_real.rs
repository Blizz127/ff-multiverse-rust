//! Counts and raw-value checks for the owner's PC moviecam.lgp. No game data is embedded
//! or printed. Run explicitly with FMV_FF7_DATA pointing at the game's data directory:
//!
//! `cargo test -p fmv-ff7 --test movie_camera_real -- --ignored --nocapture`

use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::movie_camera::{MovieCameraError, MovieCameraTrack, MOVIE_CAMERA_RECORD_BYTES};
use std::path::PathBuf;

#[test]
#[ignore = "needs the owner's FF7 Steam moviecam.lgp (FMV_FF7_DATA)"]
fn mainplr_and_ontrain_preserve_all_records_without_inventing_camera_poses() {
    let Some(data) = std::env::var_os("FMV_FF7_DATA").map(PathBuf::from) else {
        eprintln!("FMV_FF7_DATA is not set; skipping owned movie-camera check");
        return;
    };
    let mut archive = LgpArchive::open_path(data.join("cd").join("moviecam.lgp")).unwrap();
    for (name, frames, ones) in [("mainplr.cam", 197, 190), ("ontrain.cam", 192, 192)] {
        let bytes = archive.read(name).unwrap();
        assert_eq!(bytes.len(), frames * MOVIE_CAMERA_RECORD_BYTES, "{name}: exact member length");
        let track = MovieCameraTrack::parse(&bytes).unwrap();
        assert_eq!(track.frame_count(), frames, "{name}: record count");
        for index in 0..frames {
            let record = track.frame(index).unwrap();
            // Only boolean assertions: a failure must not dump the owner's record values.
            assert!(record.basis == [[0; 3]; 3], "{name} frame {index}: stored zero basis");
            assert!(record.word_12 == 0, "{name} frame {index}: stored word_12");
            assert!(record.translation == [0; 3], "{name} frame {index}: stored zero translation");
            assert!(record.pan == [0; 2], "{name} frame {index}: stored zero pan");
            assert!(record.zoom == 0, "{name} frame {index}: stored zero zoom");
            assert!(record.word_26 == u16::from(index < ones), "{name} frame {index}: opaque final-word pattern");
        }
        assert_eq!(track.frame(frames), Err(MovieCameraError::FrameOutOfRange { index: frames, frames }));
        eprintln!("{name}: {frames} complete records checked; no usable camera pose inferred");
    }
}
