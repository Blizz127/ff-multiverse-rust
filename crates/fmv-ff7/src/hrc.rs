//! HRC: the plain-text skeleton ("hierarchy") of a field character model.
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 385 `.hrc`
//! entries of `field/char.lgp` (the ignored test `char_models_all_parse` in `tests/real_data.rs`
//! asserts these totals): every file parses; they hold 1 to 29 bones each (5 713 in total) and
//! name 4 195 RSD files, every one of which exists in the archive.
//!
//! # Sources
//!
//! * ff7-flat-wiki "HRC" <https://ff7-mods.github.io/ff7-flat-wiki/PSX/HRC.html> (header lines,
//!   four lines per bone, no bone angles).
//! * Makou Reactor `src/core/field/HrcFile.cpp` <https://github.com/myst6re/makoureactor>
//!   (lines before `:BONES ` are skipped, blank and `#` lines are ignored, parents must precede
//!   children, `root` means "no parent"). Only layout facts were taken.
//!
//! # Layout
//!
//! ```text
//! :HEADER_BLOCK 2
//! :SKELETON <name>            e.g. n_cloud_sk
//! :BONES <count>
//!
//! <bone name>                 one four-line record per bone, in file order
//! <parent name | root>
//! <length>                    decimal, e.g. 1.7457236 (always positive in the owned data)
//! <rsd count> <rsd name>...   e.g. `1 AAAB`, or `0 ` for a bone without geometry
//! ```
//!
//! `:BONES 0` is the spelling of a single-bone model (props, weapons): the file then holds exactly
//! one record, named `null` (105 of the 385 owned files). Records run parent-before-child; a bone
//! may have several root-attached chains (`hip`, `l_hip` and `r_hip` all name `root`). The file stores **no angles**: a skeleton with every rotation at
//! zero is a straight line of bones, so the bind pose of a character is the first frame of one of
//! its animations ([`crate::anim`]).
//!
//! The parser is lenient about line endings (CRLF), trailing spaces and blank or `#` lines, and
//! strict about everything that would make the skeleton ambiguous: a parent that is not an
//! earlier bone, a non-finite length, or an RSD count that disagrees with the names listed.

use crate::model::ModelError;

/// Most bones accepted (the owned skeletons have 1 to 29; the cap bounds what a hostile file can
/// make [`Hrc::parse`] allocate).
pub const MAX_BONES: usize = 128;
/// Most RSD (mesh) names accepted on one bone line.
pub const MAX_RSD_PER_BONE: usize = 32;
/// Largest HRC text accepted (the real files are under 2 KiB).
pub const MAX_HRC_BYTES: usize = 256 * 1024;

/// One bone record.
#[derive(Debug, Clone, PartialEq)]
pub struct HrcBone {
    /// The bone's name, as stored (`hip`, `l_uparm`, ...).
    pub name: String,
    /// Index of the parent bone (always less than this bone's own index), or `None` when the
    /// file says `root`.
    pub parent: Option<usize>,
    /// The bone's length, as stored. The children of this bone start at its far end.
    pub length: f32,
    /// Names of the RSD files (without extension, as stored: upper case in the owned data) that
    /// describe this bone's meshes. Usually zero or one.
    pub rsd: Vec<String>,
}

/// A parsed HRC skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct Hrc {
    /// The `:SKELETON` name (empty if the file has no such line).
    pub name: String,
    /// The value of the `:BONES` line. It equals `bones.len()` except for a single-bone "null"
    /// model, which declares 0 and lists one bone (105 of the 385 owned files); animations of
    /// such a model have no per-bone rotations, see [`crate::anim::FieldAnimation::fits`].
    pub declared_bones: usize,
    /// Bones in file order; every bone's parent comes before it.
    pub bones: Vec<HrcBone>,
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "hrc", why: why.into() }
}

impl Hrc {
    /// Parse the text of an HRC file (bytes are read as Latin-1).
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() > MAX_HRC_BYTES {
            return Err(ModelError::TooMany {
                what: "hrc bytes",
                count: bytes.len() as u64,
                limit: MAX_HRC_BYTES as u64,
            });
        }
        let text: String = bytes.iter().map(|&b| char::from(b)).collect();
        let mut lines = text.lines().map(str::trim);

        let mut name = String::new();
        let mut declared: Option<usize> = None;
        for line in lines.by_ref() {
            if let Some(rest) = line.strip_prefix(":SKELETON") {
                name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix(":BONES") {
                declared = Some(
                    rest.trim().parse::<usize>().map_err(|_| invalid(format!("bad bone count {:?}", rest.trim())))?,
                );
                break;
            }
        }
        let declared_bones = declared.ok_or_else(|| invalid("no `:BONES` line"))?;
        if declared_bones > MAX_BONES {
            return Err(ModelError::TooMany {
                what: "hrc bones",
                count: declared_bones as u64,
                limit: MAX_BONES as u64,
            });
        }
        // `:BONES 0` is how a single-bone "null" model (props, weapons) is written: the file still
        // holds one record, named `null` (Makou Reactor's "null HRC fix" reads one record too).
        let count = declared_bones.max(1);

        let mut body = lines.filter(|l| !l.is_empty() && !l.starts_with('#'));
        let mut bones: Vec<HrcBone> = Vec::with_capacity(count);
        for index in 0..count {
            let mut next = |what: &str| {
                body.next().ok_or_else(|| invalid(format!("bone {index}: the file ends before its {what} line")))
            };
            let bone_name = next("name")?.to_string();
            let parent_name = next("parent")?;
            let length_text = next("length")?;
            let rsd_line = next("rsd")?;

            let parent = if parent_name == "root" {
                None
            } else {
                // The most recent earlier bone of that name (a duplicate name shadows the older one).
                let found = bones.iter().rposition(|b| b.name == parent_name);
                Some(found.ok_or_else(|| {
                    invalid(format!("bone {index} ({bone_name}): parent {parent_name:?} is not an earlier bone"))
                })?)
            };
            let length = length_text
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| invalid(format!("bone {index} ({bone_name}): bad length {length_text:?}")))?;

            let mut tokens = rsd_line.split_whitespace();
            let rsd_count: usize = tokens
                .next()
                .and_then(|t| t.parse().ok())
                .ok_or_else(|| invalid(format!("bone {index} ({bone_name}): bad rsd line {rsd_line:?}")))?;
            if rsd_count > MAX_RSD_PER_BONE {
                return Err(ModelError::TooMany {
                    what: "rsd names on a bone",
                    count: rsd_count as u64,
                    limit: MAX_RSD_PER_BONE as u64,
                });
            }
            let rsd: Vec<String> = tokens.map(str::to_string).collect();
            if rsd.len() != rsd_count {
                return Err(invalid(format!(
                    "bone {index} ({bone_name}): rsd line declares {rsd_count} names but lists {}",
                    rsd.len()
                )));
            }
            bones.push(HrcBone { name: bone_name, parent, length, rsd });
        }
        Ok(Self { name, declared_bones, bones })
    }

    /// Names of every RSD referenced by any bone, in bone order (with repeats).
    pub fn rsd_names(&self) -> impl Iterator<Item = &str> {
        self.bones.iter().flat_map(|b| b.rsd.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic skeleton (invented names and lengths) in the documented layout.
    pub(crate) fn sample_text() -> String {
        [
            ":HEADER_BLOCK 2",
            ":SKELETON test_sk",
            ":BONES 4",
            "",
            "hip",
            "root",
            "2.5",
            "1 AAAB",
            "",
            "chest",
            "hip",
            "4.0",
            "2 AAAC AAAD",
            "",
            "l_leg",
            "root",
            "3.25",
            "0 ",
            "",
            "l_foot",
            "l_leg",
            "1.5",
            "1 AAAE",
            "",
        ]
        .join("\r\n")
    }

    #[test]
    fn parses_the_documented_layout() {
        let hrc = Hrc::parse(sample_text().as_bytes()).unwrap();
        assert_eq!(hrc.name, "test_sk");
        assert_eq!(hrc.bones.len(), 4);
        let b = &hrc.bones;
        assert_eq!((b[0].name.as_str(), b[0].parent, b[0].length), ("hip", None, 2.5));
        assert_eq!(b[0].rsd, ["AAAB"]);
        assert_eq!((b[1].name.as_str(), b[1].parent, b[1].length), ("chest", Some(0), 4.0));
        assert_eq!(b[1].rsd, ["AAAC", "AAAD"]);
        assert_eq!((b[2].parent, b[2].rsd.len()), (None, 0), "a second chain hangs off root too");
        assert_eq!((b[3].parent, b[3].length), (Some(2), 1.5));
        assert_eq!(hrc.rsd_names().collect::<Vec<_>>(), ["AAAB", "AAAC", "AAAD", "AAAE"]);
    }

    #[test]
    fn a_null_skeleton_declares_zero_bones_and_lists_one() {
        let hrc =
            Hrc::parse(b":HEADER_BLOCK 2\r\n:SKELETON prop_sk\r\n:BONES 0\r\n\r\nnull\r\nroot\r\n1.0\r\n1 ATEC\r\n")
                .unwrap();
        assert_eq!((hrc.name.as_str(), hrc.declared_bones, hrc.bones.len()), ("prop_sk", 0, 1));
        assert_eq!((hrc.bones[0].name.as_str(), hrc.bones[0].parent, hrc.bones[0].length), ("null", None, 1.0));
        assert_eq!(hrc.bones[0].rsd, ["ATEC"]);
        // a declared zero without its one record is still an error
        assert!(Hrc::parse(b":BONES 0\n").is_err());
        // an ordinary skeleton declares what it lists
        assert_eq!(Hrc::parse(sample_text().as_bytes()).unwrap().declared_bones, 4);
    }

    #[test]
    fn tolerates_comments_blank_lines_and_odd_spacing() {
        let text = "junk before\n:SKELETON  s \n# comment\n:BONES 1\n\n#c\n a \n root \n 1e0 \n 1   X  \n";
        let hrc = Hrc::parse(text.as_bytes()).unwrap();
        assert_eq!(hrc.name, "s");
        assert_eq!((hrc.bones[0].name.as_str(), hrc.bones[0].length), ("a", 1.0));
        assert_eq!(hrc.bones[0].rsd, ["X"]);
        // no :SKELETON line is fine
        let hrc = Hrc::parse(b":BONES 1\na\nroot\n1\n0\n").unwrap();
        assert_eq!(hrc.name, "");
    }

    #[test]
    fn rejects_malformed_skeletons() {
        let one = |body: &str| Hrc::parse(format!(":BONES 2\n{body}").as_bytes());
        assert!(matches!(Hrc::parse(b""), Err(ModelError::Invalid { .. })));
        assert!(matches!(Hrc::parse(b":SKELETON x\n"), Err(ModelError::Invalid { .. })), "no :BONES line");
        assert!(matches!(Hrc::parse(b":BONES x\n"), Err(ModelError::Invalid { .. })));
        assert!(matches!(Hrc::parse(b":BONES 0\n"), Err(ModelError::Invalid { .. })), "the one null record is missing");
        assert!(matches!(Hrc::parse(b":BONES 129\n"), Err(ModelError::TooMany { .. })));
        assert!(matches!(Hrc::parse(b":BONES 99999999999999999999\n"), Err(ModelError::Invalid { .. })));
        // fewer records than declared
        assert!(one("a\nroot\n1\n0\n").is_err());
        // a parent that is not defined earlier (forward reference, self reference, unknown)
        assert!(one("a\nb\n1\n0\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\na\n1\n0\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\nroot\n1\n0\nb\nnobody\n1\n0\n").is_err());
        // bad numbers
        assert!(one("a\nroot\nNaN\n0\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\nroot\ninf\n0\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\nroot\nx\n0\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\nroot\n1\nx\nb\nroot\n1\n0\n").is_err());
        // an rsd count that disagrees with the names, or is absurd
        assert!(one("a\nroot\n1\n2 ONE\nb\nroot\n1\n0\n").is_err());
        assert!(one("a\nroot\n1\n0 EXTRA\nb\nroot\n1\n0\n").is_err());
        assert!(matches!(one("a\nroot\n1\n999 X\nb\nroot\n1\n0\n"), Err(ModelError::TooMany { .. })));
        // oversized input is refused before any parsing
        assert!(matches!(Hrc::parse(&vec![b'x'; MAX_HRC_BYTES + 1]), Err(ModelError::TooMany { .. })));
    }

    #[test]
    fn a_duplicate_name_resolves_to_the_most_recent_bone() {
        let hrc = Hrc::parse(b":BONES 3\na\nroot\n1\n0\na\nroot\n1\n0\nc\na\n1\n0\n").unwrap();
        assert_eq!(hrc.bones[2].parent, Some(1));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let text = sample_text().into_bytes();
        for n in 0..text.len() {
            let _ = Hrc::parse(text.get(..n).unwrap_or(&[]));
        }
        for i in 0..text.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = text.clone();
                if let Some(x) = b.get_mut(i) {
                    *x ^= flip;
                }
                let _ = Hrc::parse(&b);
            }
        }
        let _ = Hrc::parse(&[0xFF, 0xFE, 0x00, b'\n', b':', b'B']);
    }
}
