//! RSD: the plain-text "resource" file that ties one bone's mesh (a `.p` model) to its textures.
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 4 180 `.rsd`
//! entries of `field/char.lgp` (the ignored test `char_models_all_parse` in `tests/real_data.rs`
//! asserts it): every file starts with `@RSD940102`, has exactly one `PLY=`, `MAT=` and `GRP=` line
//! naming the same stem, and every mesh and texture it names exists in the archive.
//!
//! # Sources
//!
//! * Makou Reactor `src/core/field/RsdFile.cpp` <https://github.com/myst6re/makoureactor>
//!   (`PLY=`, `MAT=` and `GRP=` lines name the mesh, `NTEX=` is followed by `TEX[n]=` lines, names
//!   lose their extension and are matched case-insensitively). Only layout facts were taken.
//! * ff7-flat-wiki "HRC" <https://ff7-mods.github.io/ff7-flat-wiki/PSX/HRC.html>: the RSD
//!   originates in the PlayStation Psy-Q 3D library; on PC the `.PLY` mesh became `.p` and the
//!   `.TIM` textures became `.tex`.
//!
//! # Layout
//!
//! ```text
//! @RSD940102
//! PLY=AAAC.PLY        mesh -> the char.lgp entry `aaac.p`
//! MAT=AAAC.MAT        (the same stem as PLY in every owned file; unused here)
//! GRP=AAAC.GRP        (same)
//! NTEX=1              number of textures
//! TEX[0]=AAAD.TIM     -> the char.lgp entry `aaad.tex`
//! ```
//!
//! The mesh stem is not the RSD's own stem (`AAAB.RSD` names `AAAC.PLY` in the example from the
//! owned data). The textures of an RSD are numbered in order of the `TEX[n]` lines; a `.p`
//! group's texture number (see [`crate::pmodel::PGroup::texture_number`]) indexes that list.

use crate::model::ModelError;

/// Most textures accepted on one RSD (real files have at most a few).
pub const MAX_RSD_TEXTURES: usize = 32;
/// Largest RSD text accepted (the real files are about 60 to 120 bytes).
pub const MAX_RSD_BYTES: usize = 64 * 1024;

/// A parsed RSD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rsd {
    /// Stem of the mesh entry, lower case, without extension (`aaac` for `PLY=AAAC.PLY`):
    /// the archive entry is `<p_name>.p`.
    pub p_name: String,
    /// Stems of the textures in `TEX[n]` order, lower case: the entries are `<name>.tex`.
    pub textures: Vec<String>,
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "rsd", why: why.into() }
}

/// `NAME.EXT` (or a bare name) to the lower-case stem; directories are not stripped.
fn stem(value: &str) -> String {
    let value = value.trim();
    let cut = value.rfind('.').map_or(value, |dot| value.get(..dot).unwrap_or(value));
    cut.to_ascii_lowercase()
}

impl Rsd {
    /// Parse the text of an RSD file (bytes are read as Latin-1).
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() > MAX_RSD_BYTES {
            return Err(ModelError::TooMany {
                what: "rsd bytes",
                count: bytes.len() as u64,
                limit: MAX_RSD_BYTES as u64,
            });
        }
        let text: String = bytes.iter().map(|&b| char::from(b)).collect();
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());

        let mut ply: Option<String> = None;
        let mut other: Option<String> = None;
        let mut textures: Vec<String> = Vec::new();
        let mut counted = false;
        while let Some(line) = lines.next() {
            if let Some(v) = line.strip_prefix("PLY=") {
                ply.get_or_insert_with(|| stem(v));
            } else if let Some(v) = line.strip_prefix("MAT=").or_else(|| line.strip_prefix("GRP=")) {
                other.get_or_insert_with(|| stem(v));
            } else if let Some(v) = line.strip_prefix("NTEX=") {
                if counted {
                    continue;
                }
                counted = true;
                let n: usize = v.trim().parse().map_err(|_| invalid(format!("bad NTEX value {:?}", v.trim())))?;
                if n > MAX_RSD_TEXTURES {
                    return Err(ModelError::TooMany {
                        what: "rsd textures",
                        count: n as u64,
                        limit: MAX_RSD_TEXTURES as u64,
                    });
                }
                for i in 0..n {
                    let line = lines.next().ok_or_else(|| invalid(format!("the file ends before TEX[{i}]")))?;
                    let prefix = format!("TEX[{i}]=");
                    let value = line
                        .strip_prefix(prefix.as_str())
                        .ok_or_else(|| invalid(format!("expected {prefix}, found {line:?}")))?;
                    textures.push(stem(value));
                }
            }
        }
        let p_name = [ply, other]
            .into_iter()
            .flatten()
            .find(|n| !n.is_empty())
            .ok_or_else(|| invalid("no PLY=, MAT= or GRP= line names a mesh"))?;
        Ok(Self { p_name, textures })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_layout() {
        let text = "@RSD940102\r\nPLY=ABCD.PLY\r\nMAT=ABCD.MAT\r\nGRP=ABCD.GRP\r\nNTEX=2\r\nTEX[0]=WXYZ.TIM\r\nTEX[1]=Wxyy.tim\r\n";
        let rsd = Rsd::parse(text.as_bytes()).unwrap();
        assert_eq!(rsd.p_name, "abcd");
        assert_eq!(rsd.textures, ["wxyz", "wxyy"]);
        let none = Rsd::parse(b"@RSD940102\nPLY=AAAC.PLY\nMAT=AAAC.MAT\nGRP=AAAC.GRP\nNTEX=0\n").unwrap();
        assert_eq!((none.p_name.as_str(), none.textures.len()), ("aaac", 0));
    }

    #[test]
    fn the_ply_line_wins_and_a_mat_or_grp_line_is_the_fallback() {
        let r = Rsd::parse(b"MAT=ONE.MAT\nPLY=TWO.PLY\nGRP=THREE.GRP\nNTEX=0\n").unwrap();
        assert_eq!(r.p_name, "two");
        let r = Rsd::parse(b"MAT=ONE.MAT\nGRP=THREE.GRP\nNTEX=0\n").unwrap();
        assert_eq!(r.p_name, "one");
        assert!(Rsd::parse(b"@RSD940102\nNTEX=0\n").is_err(), "no mesh named");
        assert!(Rsd::parse(b"PLY=.PLY\nNTEX=0\n").is_err(), "an empty mesh name is no name");
        assert!(Rsd::parse(b"").is_err());
        // a name without extension keeps its whole stem
        assert_eq!(Rsd::parse(b"PLY=bare\n").unwrap().p_name, "bare");
    }

    #[test]
    fn rejects_broken_texture_lists() {
        assert!(Rsd::parse(b"PLY=A.PLY\nNTEX=x\n").is_err());
        assert!(matches!(Rsd::parse(b"PLY=A.PLY\nNTEX=33\n"), Err(ModelError::TooMany { .. })));
        assert!(Rsd::parse(b"PLY=A.PLY\nNTEX=2\nTEX[0]=B.TIM\n").is_err(), "list cut short");
        assert!(Rsd::parse(b"PLY=A.PLY\nNTEX=2\nTEX[1]=B.TIM\nTEX[0]=C.TIM\n").is_err(), "out of order");
        assert!(Rsd::parse(b"PLY=A.PLY\nNTEX=1\nGRP=X.GRP\n").is_err(), "texture line must follow");
        assert!(matches!(Rsd::parse(&vec![b'x'; MAX_RSD_BYTES + 1]), Err(ModelError::TooMany { .. })));
        // a second NTEX line is ignored, not re-read
        let r = Rsd::parse(b"PLY=A.PLY\nNTEX=1\nTEX[0]=B.TIM\nNTEX=1\nTEX[0]=C.TIM\n").unwrap();
        assert_eq!(r.textures, ["b"]);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let text = b"@RSD940102\nPLY=AAAC.PLY\nMAT=AAAC.MAT\nGRP=AAAC.GRP\nNTEX=1\nTEX[0]=AAAD.TIM\n".to_vec();
        for n in 0..text.len() {
            let _ = Rsd::parse(text.get(..n).unwrap_or(&[]));
        }
        for i in 0..text.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = text.clone();
                if let Some(x) = b.get_mut(i) {
                    *x ^= flip;
                }
                let _ = Rsd::parse(&b);
            }
        }
    }
}
