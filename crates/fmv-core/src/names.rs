//! Display-name policy: cleaning, admission and the uniqueness key.
//!
//! A display name is shown to every player and is how players tell each other apart (party
//! invites, chat-like labels), so three different questions are answered separately:
//!
//! * [`AvatarAppearance::sanitize_name`](crate::AvatarAppearance::sanitize_name) **cleans** a
//!   name: it removes everything that draws nothing or changes how the text is laid out,
//!   writes it in canonical (NFC) form, collapses white space and clamps the length. It never
//!   judges the writing system, so it is safe as a display filter for names that were
//!   registered long ago.
//! * [`AvatarAppearance::validate_new_name`](crate::AvatarAppearance::validate_new_name) is the
//!   **admission** rule for a name that is about to be registered: cleaning, plus a restriction
//!   on the writing systems it may mix (see "Scripts" below).
//! * [`name_key`] is the **uniqueness key**: two names that a player could mistake for each
//!   other have the same key, so the store refuses the second one.
//!
//! # What cleaning removes
//!
//! Control characters, and every character that renders as nothing or is a layout instruction
//! ([`is_invisible_format`]): the format characters (category Cf), the line and paragraph
//! separators, the Unicode `Default_Ignorable_Code_Point` set (Hangul fillers U+115F, U+1160,
//! U+3164, U+FFA0, the combining grapheme joiner U+034F, variation selectors, Khmer and
//! Mongolian invisibles, tag characters ...) and the blank Braille pattern U+2800. Any other
//! white space (no-break space, ideographic space, the en and em spaces ...) becomes one plain
//! space, and runs of spaces collapse to one. Combining marks must follow a letter and at most
//! [`MAX_MARKS`] may stack on it, so a name cannot grow tall "zalgo" towers or start with a
//! mark that attaches to nothing. A name with nothing visible left is refused.
//!
//! # Scripts
//!
//! Most lookalike impersonation ("Tifa" with a Cyrillic T) needs two writing systems in one
//! name, so a new name must be single-script in the sense of UTS #39: all its letters come
//! from one Unicode script (digits, punctuation, symbols and emoji belong to every script),
//! or from the usual East Asian combinations of Han with Hiragana and Katakana, Bopomofo or
//! Hangul, optionally together with Latin. Only the scripts UAX #31 recommends for identifiers
//! are accepted, which also leaves out the historic and limited-use scripts that exist mostly
//! as Latin lookalikes (Cherokee, Lisu, Runic ...).
//!
//! # The uniqueness key
//!
//! `name_key` cleans the name, applies NFKC (fullwidth and mathematical letters, ligatures,
//! circled and enclosed forms, half-width katakana ... fold to their plain forms), maps a
//! curated set of Cyrillic, Greek, Armenian and Latin lookalikes onto the Latin letter they
//! imitate, lower-cases (with the sharp s written `ss` and every sigma alike), and folds
//! typographic apostrophes and dashes. That makes whole-script lookalikes collide as well:
//! the all-Cyrillic "сор" and the Latin "cop" have the same key.
//!
//! What it deliberately does **not** do: it does not treat ASCII lookalikes such as `I`/`l`
//! or `0`/`O` as equal (that would refuse many honest names) and it does not strip accents
//! ("José" and "Jose" are different players). The confusable table is a hand-curated subset
//! of UTS #39 `confusables.txt`, not the whole file; the script rule above is the primary
//! defence and the table closes the gap it leaves for single-script lookalikes.

use std::fmt;

use icu_normalizer::ComposingNormalizerBorrowed;
use unicode_script::{Script, ScriptExtension, UnicodeScript};

/// Longest display name, in characters.
const MAX_NAME_CHARS: usize = crate::AvatarAppearance::MAX_NAME;

/// Most combining marks that may stack on one base character in a display name.
pub const MAX_MARKS: usize = 3;

static NFC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfc();
static NFKC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfkc();

/// Why a name was not accepted for registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameError {
    /// Nothing visible is left after cleaning.
    Empty,
    /// The name mixes writing systems that can be used to imitate another name (for example
    /// Latin with Cyrillic or Greek).
    MixedScripts,
    /// The name uses a script outside the accepted set, or an unassigned or private-use
    /// character.
    UnsupportedCharacters,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NameError::Empty => "the display name has no visible characters",
            NameError::MixedScripts => {
                "the display name mixes writing systems (for example Latin with Cyrillic or Greek)"
            }
            NameError::UnsupportedCharacters => {
                "the display name uses characters from an unsupported script"
            }
        })
    }
}

impl std::error::Error for NameError {}

/// Characters that draw nothing, or change how the text around them is drawn or ordered, so
/// that two different strings can look identical. This is every character that cleaning removes
/// besides control characters: the format characters (Unicode category Cf), the line and
/// paragraph separators U+2028/U+2029 (they split a line for many log parsers), the Unicode
/// `Default_Ignorable_Code_Point` set (which adds the invisible fillers and variation
/// selectors) and the blank Braille pattern U+2800. Written out because `std` has no category
/// query; the table follows Unicode 16.
pub fn is_invisible_format(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00AD                       // soft hyphen
        | 0x034F                     // combining grapheme joiner
        | 0x0600..=0x0605            // Arabic number signs
        | 0x061C                     // Arabic letter mark
        | 0x06DD                     // Arabic end of ayah
        | 0x070F                     // Syriac abbreviation mark
        | 0x0890..=0x0891            // Arabic pound / piastre marks above
        | 0x08E2                     // Arabic disputed end of ayah
        | 0x115F..=0x1160            // Hangul choseong / jungseong fillers
        | 0x17B4..=0x17B5            // Khmer inherent vowels
        | 0x180B..=0x180F            // Mongolian free variation selectors, vowel separator
        | 0x200B..=0x200F            // zero-width space/joiners, LRM, RLM
        | 0x2028..=0x2029            // line and paragraph separators
        | 0x202A..=0x202E            // bidi embeddings and overrides
        | 0x2060..=0x2064            // word joiner, invisible operators
        | 0x2065                     // unassigned, default ignorable
        | 0x2066..=0x206F            // bidi isolates, deprecated format characters
        | 0x2800                     // braille pattern blank
        | 0x3164                     // Hangul filler
        | 0xFE00..=0xFE0F            // variation selectors 1-16
        | 0xFEFF                     // zero-width no-break space (BOM)
        | 0xFFA0                     // halfwidth Hangul filler
        | 0xFFF0..=0xFFFB            // unassigned default ignorables, interlinear annotation
        | 0x110BD | 0x110CD          // Kaithi number signs
        | 0x13430..=0x1343F          // Egyptian hieroglyph format controls
        | 0x16FE4                    // Khitan small script filler
        | 0x1BCA0..=0x1BCA3          // shorthand format controls
        | 0x1D173..=0x1D17A          // musical symbol format controls
        | 0xE0000..=0xE0FFF          // tag characters, variation selectors supplement
    )
}

/// Drop control and invisible characters; turn every other white space into one plain space.
fn strip_invisible(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect()
}

/// A combining mark in the sense that matters for layout: Script=Inherited (the generic
/// diacritics, Arabic vowel signs, kana voicing marks ...). Marks that belong to one script,
/// such as Devanagari vowel signs, are ordinary letters here.
fn is_mark(c: char) -> bool {
    c.script() == Script::Inherited
}

/// Marks must follow a non-space character and at most [`MAX_MARKS`] stack on it; others are
/// dropped.
fn tidy_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    let mut has_base = false;
    for c in s.chars() {
        if is_mark(c) {
            if has_base && run < MAX_MARKS {
                out.push(c);
                run += 1;
            }
        } else {
            out.push(c);
            run = 0;
            has_base = c != ' ';
        }
    }
    out
}

/// Collapse runs of spaces to one and trim both ends.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c == ' ' {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// A name that needs no work: printable ASCII, no leading, trailing or doubled space, short.
fn is_plain_ascii_name(s: &str) -> bool {
    s.len() <= MAX_NAME_CHARS
        && !s.is_empty()
        && s.bytes().all(|b| (0x20..0x7F).contains(&b))
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && !s.contains("  ")
}

/// The cleaning behind `AvatarAppearance::sanitize_name`.
pub(crate) fn clean_name(raw: &str) -> Option<String> {
    if is_plain_ascii_name(raw) {
        return Some(raw.to_owned());
    }
    let stripped = strip_invisible(raw);
    let composed = NFC.normalize(&stripped);
    let tidy = collapse_spaces(&tidy_marks(&composed));
    // Clamp last, then tidy again: the cut can leave a trailing space.
    let clamped: String = tidy.chars().take(MAX_NAME_CHARS).collect();
    let name = collapse_spaces(&clamped);
    (!name.is_empty()).then_some(name)
}

/// The script rule of `AvatarAppearance::validate_new_name`; `name` is already clean.
pub(crate) fn check_scripts(name: &str) -> Result<(), NameError> {
    let folded = NFKC.normalize(name);
    // Both forms must pass: NFKC can turn a character of no script (a mathematical letter)
    // into a Latin one that then sits next to Cyrillic.
    for text in [name, folded.as_ref()] {
        if text.chars().any(|c| !c.script().is_recommended()) {
            return Err(NameError::UnsupportedCharacters);
        }
        if !single_script(text) {
            return Err(NameError::MixedScripts);
        }
    }
    Ok(())
}

/// The East Asian script combinations that real names use, each with Latin allowed on top.
const CJK_FAMILIES: [&[Script]; 3] = [
    &[Script::Han, Script::Hiragana, Script::Katakana],
    &[Script::Han, Script::Bopomofo],
    &[Script::Han, Script::Hangul],
];

/// UTS #39 "highly restrictive": one script (Common and Inherited belong to every script), or
/// one CJK family, either optionally with Latin.
fn single_script(text: &str) -> bool {
    if !ScriptExtension::for_str(text).is_empty() {
        return true;
    }
    CJK_FAMILIES.iter().any(|family| {
        text.chars().all(|c| {
            let ext = c.script_extension();
            ext.contains_script(Script::Latin) || family.iter().any(|s| ext.contains_script(*s))
        })
    })
}

/// The Latin text a lookalike character imitates. The table is case-sensitive on purpose:
/// capital В looks like `B`, but its small form в does not look like `b`. Escapes keep the
/// source itself free of lookalikes.
fn confusable(c: char) -> Option<&'static str> {
    Some(match u32::from(c) {
        // Latin letters that imitate other Latin letters; the sharp s and the sigma are plain
        // case folds that `to_lowercase` does not do.
        0x0131 => "i",           // dotless i
        0x0261 => "g",           // script g
        0x01C0 => "l",           // dental click
        0x00DF | 0x1E9E => "ss", // sharp s
        0x03C2 => "\u{3C3}",     // final sigma -> sigma

        // Greek capitals
        0x0391 => "A",
        0x0392 => "B",
        0x0395 => "E",
        0x0396 => "Z",
        0x0397 => "H",
        0x0399 => "I",
        0x039A => "K",
        0x039C => "M",
        0x039D => "N",
        0x039F => "O",
        0x03A1 => "P",
        0x03A4 => "T",
        0x03A5 => "Y",
        0x03A7 => "X",
        0x03F9 => "C", // capital lunate sigma
        0x037F => "J", // capital yot
        // Greek small
        0x03BF => "o",
        0x03BD => "v",
        0x03B9 => "i",
        0x03BA => "k",
        0x03B1 => "a",
        0x03C1 => "p",
        0x03C5 => "u",
        0x03F2 => "c", // lunate sigma
        0x03F3 => "j", // yot

        // Cyrillic capitals
        0x0410 => "A",
        0x0412 => "B",
        0x0415 => "E",
        0x041A => "K",
        0x041C => "M",
        0x041D => "H",
        0x041E => "O",
        0x0420 => "P",
        0x0421 => "C",
        0x0422 => "T",
        0x0425 => "X",
        0x0406 => "I", // Ukrainian I
        0x0408 => "J",
        0x0405 => "S",
        0x0474 => "V", // izhitsa
        0x051A => "Q",
        0x051C => "W",
        0x04C0 => "l", // palochka
        // Cyrillic small
        0x0430 => "a",
        0x0435 => "e",
        0x043E => "o",
        0x0440 => "p",
        0x0441 => "c",
        0x0445 => "x",
        0x0443 => "y",
        0x0456 => "i",
        0x0458 => "j",
        0x0455 => "s",
        0x0475 => "v",
        0x0501 => "d",
        0x051B => "q",
        0x051D => "w",
        0x04BB => "h",
        0x04CF => "l", // small palochka

        // Armenian
        0x0555 => "O",
        0x054D => "U",
        0x0585 => "o",
        0x057D => "u",
        0x0578 => "n",

        // Typographic apostrophes and dashes
        0x0060 | 0x00B4 | 0x02BC | 0x2018 | 0x2019 | 0x2032 => "'",
        0x2010 | 0x2012..=0x2015 | 0x2212 => "-",
        _ => return None,
    })
}

/// Apply [`confusable`] to every character.
fn fold_confusables(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match confusable(c) {
            Some(replacement) => out.push_str(replacement),
            None => out.push(c),
        }
    }
    out
}

/// The uniqueness key of a display name.
///
/// Two names that a player could mistake for each other have the same key, so the store keeps
/// at most one account per key. The name is cleaned first (exactly like
/// `AvatarAppearance::sanitize_name`, so `name_key(raw) == name_key(sanitized)`), then
/// compatibility-normalized (NFKC), folded across the curated lookalike table, lower-cased and
/// normalized once more. See the module documentation for what is and is not folded.
///
/// The key is for comparing, not for showing, and it is not a name: it can be longer than
/// [`AvatarAppearance::MAX_NAME`](crate::AvatarAppearance::MAX_NAME) (NFKC expands some
/// characters) and must not be fed back into `name_key`. A name with nothing visible has the
/// empty key.
pub fn name_key(name: &str) -> String {
    let Some(clean) = clean_name(name) else {
        return String::new();
    };
    let folded = NFKC.normalize(&clean);
    let lowered = fold_confusables(&folded).to_lowercase();
    // Lower-casing and folding can leave text that is no longer NFKC, and fold results can
    // collapse white space into a new double space; both must not make keys differ.
    let renormalized = NFKC.normalize(&fold_confusables(&lowered)).into_owned();
    collapse_spaces(&renormalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AvatarAppearance;

    fn clean(raw: &str) -> Option<String> {
        AvatarAppearance::sanitize_name(raw)
    }

    /// Every invisible filler the audit named, and the rest of the default-ignorable set.
    const FILLERS: &[(u32, &str)] = &[
        (0x3164, "Hangul filler"),
        (0xFFA0, "halfwidth Hangul filler"),
        (0x2800, "braille blank"),
        (0x034F, "combining grapheme joiner"),
        (0xFE0F, "variation selector 16"),
        (0xFE00, "variation selector 1"),
        (0xE0100, "variation selector 17"),
        (0xE01EF, "variation selector 256"),
        (0x115F, "Hangul choseong filler"),
        (0x1160, "Hangul jungseong filler"),
        (0x17B4, "Khmer inherent AQ"),
        (0x17B5, "Khmer inherent AA"),
        (0x180B, "Mongolian free variation selector one"),
        (0x180F, "Mongolian free variation selector four"),
        (0x2065, "unassigned default ignorable"),
        (0xFFF0, "unassigned default ignorable"),
        (0x16FE4, "Khitan small script filler"),
        (0xE0001, "language tag"),
        (0xE0041, "tag A"),
        (0xE0FFF, "last default ignorable"),
        (0x200B, "zero width space"),
        (0x2060, "word joiner"),
        (0x00AD, "soft hyphen"),
        (0xFEFF, "BOM"),
    ];

    #[test]
    fn every_invisible_filler_is_dropped_wherever_it_sits() {
        for &(cp, what) in FILLERS {
            let ch = char::from_u32(cp).unwrap();
            assert!(is_invisible_format(ch), "U+{cp:04X} {what}");
            for raw in [
                format!("Tifa{ch}"),
                format!("{ch}Tifa"),
                format!("Ti{ch}fa"),
                format!("T{ch}{ch}ifa"),
            ] {
                assert_eq!(
                    clean(&raw).as_deref(),
                    Some("Tifa"),
                    "U+{cp:04X} {what} in {raw:?}"
                );
                assert_eq!(name_key(&raw), name_key("Tifa"), "U+{cp:04X} {what}");
            }
            // A name made of nothing but that character has nothing visible.
            assert_eq!(clean(&ch.to_string()), None, "U+{cp:04X} {what} alone");
            assert_eq!(
                clean(&format!(" {ch}{ch} ")),
                None,
                "U+{cp:04X} {what} with spaces"
            );
        }
        // The Hangul fillers together, and a mix of all of them.
        assert_eq!(clean("\u{115F}\u{1160}"), None);
        let all: String = FILLERS
            .iter()
            .filter_map(|&(cp, _)| char::from_u32(cp))
            .collect();
        assert_eq!(clean(&all), None);
        assert_eq!(clean(&format!("A{all}B")).as_deref(), Some("AB"));
    }

    #[test]
    fn the_filler_set_does_not_swallow_ordinary_characters() {
        for ch in [
            'A',
            'z',
            '0',
            ' ',
            '-',
            '\'',
            '\u{00E9}',
            '\u{30AF}',
            '\u{D55C}',
            '\u{1F338}',
            '\u{FF21}',
            '\u{0301}',
            '\u{3000}',
            '\u{00A0}',
        ] {
            assert!(!is_invisible_format(ch), "U+{:04X}", u32::from(ch));
        }
        // Hangul syllables and the visible jamo next to the fillers survive.
        assert_eq!(
            clean("\u{D55C}\u{AE00}").as_deref(),
            Some("\u{D55C}\u{AE00}")
        );
        assert_eq!(
            clean("\u{1100}\u{1161}").as_deref(),
            Some("\u{AC00}"),
            "NFC composes the jamo pair"
        );
    }

    #[test]
    fn white_space_is_collapsed_and_made_plain() {
        assert_eq!(clean("Cloud  Strife").as_deref(), Some("Cloud Strife"));
        assert_eq!(
            clean("Cloud\u{00A0}Strife").as_deref(),
            Some("Cloud Strife")
        );
        assert_eq!(
            clean("Cloud\u{3000}\u{2003}  \u{2009}Strife").as_deref(),
            Some("Cloud Strife")
        );
        assert_eq!(
            clean("  \u{3000}Cloud Strife \u{00A0}").as_deref(),
            Some("Cloud Strife")
        );
        assert_eq!(name_key("Cloud  Strife"), name_key("Cloud Strife"));
        assert_eq!(name_key("Cloud\u{00A0}Strife"), name_key("Cloud Strife"));
        // Control white space is dropped, as before (it is not a separator).
        assert_eq!(clean("Cl\toud\n").as_deref(), Some("Cloud"));
        // The clamp can end on a space: it is trimmed again.
        let name = format!("{} b", "a".repeat(AvatarAppearance::MAX_NAME - 1));
        assert_eq!(
            clean(&name),
            Some("a".repeat(AvatarAppearance::MAX_NAME - 1))
        );
    }

    #[test]
    fn names_are_written_in_nfc() {
        let nfd = "A\u{0308}rger";
        let nfc = "\u{00C4}rger";
        assert_eq!(clean(nfd).as_deref(), Some(nfc));
        assert_eq!(clean(nfc).as_deref(), Some(nfc));
        assert_eq!(name_key(nfd), name_key(nfc));
        // A combining grapheme joiner between the letter and its accent no longer splits them.
        assert_eq!(clean("A\u{034F}\u{0308}rger").as_deref(), Some(nfc));
        // Vietnamese stacks two marks: untouched after composition.
        assert_eq!(clean("Vi\u{1EC7}t").as_deref(), Some("Vi\u{1EC7}t"));
        assert_eq!(
            clean("Vie\u{0323}\u{0302}t").as_deref(),
            Some("Vi\u{1EC7}t")
        );
    }

    #[test]
    fn combining_marks_need_a_base_and_cannot_tower() {
        let tower = format!("T{}", "\u{0301}".repeat(23));
        let cleaned = clean(&tower).unwrap();
        assert_eq!(cleaned.chars().count(), 1 + MAX_MARKS);
        assert!(cleaned.starts_with('T'));
        // Marks with nothing to attach to are dropped, and a name of marks only is empty.
        assert_eq!(clean("\u{0301}\u{0302}"), None);
        assert_eq!(clean("\u{0301}Tifa").as_deref(), Some("Tifa"));
        assert_eq!(clean("Ti \u{0301}fa").as_deref(), Some("Ti fa"));
        // Marks that belong to a script (Devanagari vowel signs) are letters, not towers.
        let hindi = "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}";
        assert_eq!(clean(hindi).as_deref(), Some(hindi));
    }

    #[test]
    fn the_name_is_clamped_after_cleaning_and_cleaning_is_idempotent() {
        let padded: String = std::iter::repeat_n('\u{3164}', 40)
            .chain("x".repeat(30).chars())
            .collect();
        let cleaned = clean(&padded).unwrap();
        assert_eq!(cleaned, "x".repeat(AvatarAppearance::MAX_NAME));

        let corpus = [
            "Cloud",
            "  Cloud \u{3000} Strife ",
            "A\u{0308}rger",
            "T\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}ifa",
            "\u{FF34}\u{FF49}\u{FF46}\u{FF41}",
            "Tifa\u{3164}\u{2800}",
            "\u{0422}ifa",
            "a b c d e f g h i j k l m n o p q r s t u v w x y z",
            "\u{1F338}\u{FE0F} Aerith \u{1F338}",
            "\u{30AF}\u{30E9}\u{30A6}\u{30C9}\u{3099}",
            "\u{D55C}\u{AE00}   x",
            "\u{FDFA}\u{FDFA}",
            "\u{1E9E}stra\u{00DF}e",
        ];
        for raw in corpus {
            let once = clean(raw);
            if let Some(once) = once {
                assert!(
                    once.chars().count() <= AvatarAppearance::MAX_NAME,
                    "{raw:?}"
                );
                assert_eq!(
                    clean(&once),
                    Some(once.clone()),
                    "not idempotent: {raw:?} -> {once:?}"
                );
                // The key of the raw text is the key of its clean form.
                assert_eq!(name_key(raw), name_key(&once), "{raw:?}");
            } else {
                assert_eq!(name_key(raw), "", "{raw:?}");
            }
        }
    }

    #[test]
    fn fullwidth_mathematical_and_enclosed_letters_share_the_plain_key() {
        let tifa = name_key("Tifa");
        assert_eq!(tifa, "tifa");
        for variant in [
            "\u{FF34}\u{FF49}\u{FF46}\u{FF41}",     // fullwidth
            "\u{1D413}\u{1D422}\u{1D41F}\u{1D41A}", // mathematical bold
            "\u{1D54B}\u{1D55A}\u{1D557}\u{1D552}", // double-struck
            "\u{24E3}\u{24D8}\u{24D5}\u{24D0}",     // circled small letters
            "\u{FB01}",                             // fi ligature, compared with "fi" below
        ] {
            if variant.chars().count() == 1 {
                assert_eq!(name_key(variant), "fi");
            } else {
                assert_eq!(name_key(variant), tifa, "{variant:?}");
            }
        }
        // The stored (clean) form keeps the player's styling; only the key folds it.
        assert_eq!(
            clean("\u{FF34}\u{FF49}\u{FF46}\u{FF41}").as_deref(),
            Some("\u{FF34}\u{FF49}\u{FF46}\u{FF41}")
        );
        // Halfwidth katakana folds to the fullwidth forms.
        assert_eq!(
            name_key("\u{FF83}\u{FF68}\u{FF8C}\u{FF67}"),
            name_key("\u{30C6}\u{30A3}\u{30D5}\u{30A1}")
        );
    }

    #[test]
    fn cyrillic_t_in_tifa_is_rejected_and_still_collides() {
        let spoof = "\u{0422}ifa";
        assert_eq!(
            AvatarAppearance::validate_new_name(spoof),
            Err(NameError::MixedScripts)
        );
        assert_eq!(
            AvatarAppearance::validate_new_name("Tifa").as_deref(),
            Ok("Tifa")
        );
        // sanitize_name is only a cleaner: it does not judge scripts, so display stays total.
        assert_eq!(clean(spoof).as_deref(), Some(spoof));
        // A lookup by the spoofed spelling lands on the genuine name.
        assert_eq!(name_key(spoof), name_key("Tifa"));
    }

    #[test]
    fn whole_script_lookalikes_collide_with_the_latin_name() {
        // All Cyrillic: passes the script rule, so the key has to catch it.
        let cop = "\u{0441}\u{043E}\u{0440}";
        assert_eq!(AvatarAppearance::validate_new_name(cop).as_deref(), Ok(cop));
        assert_eq!(name_key(cop), name_key("cop"));
        assert_eq!(
            name_key("\u{0410}\u{0412}\u{0415}\u{041A}"),
            name_key("ABEK")
        );
        // All Greek capitals.
        assert_eq!(
            name_key("\u{039F}\u{03A1}\u{039F}\u{039D}"),
            name_key("OPON")
        );
        // Greek small letters: omicron, nu and iota look like o, v and i.
        assert_eq!(name_key("\u{03BF}\u{03BD}\u{03B9}"), name_key("ovi"));
        // The table is case-sensitive: small ve does not look like b.
        assert_ne!(name_key("\u{0432}"), name_key("b"));
        // Dotless i, script g.
        assert_eq!(name_key("\u{0131}ce"), name_key("ice"));
        assert_eq!(name_key("\u{0261}il"), name_key("gil"));
    }

    #[test]
    fn case_folds_cover_sharp_s_and_sigma() {
        assert_eq!(name_key("Stra\u{00DF}e"), name_key("STRASSE"));
        assert_eq!(name_key("\u{1E9E}"), name_key("ss"));
        // Capital sigma lower-cases to the final form at the end of a word; typing the medial
        // form there must not make another player.
        assert_eq!(
            name_key("\u{03A3}\u{039F}\u{03A6}\u{0399}\u{0391}\u{03A3}"),
            name_key("\u{03C3}\u{03BF}\u{03C6}\u{03B9}\u{03B1}\u{03C3}")
        );
        assert_eq!(
            name_key("\u{03C3}\u{03BF}\u{03C6}\u{03B9}\u{03B1}\u{03C2}"),
            name_key("\u{03C3}\u{03BF}\u{03C6}\u{03B9}\u{03B1}\u{03C3}")
        );
        assert_eq!(name_key("\u{00C4}RGER"), name_key("\u{00E4}rger"));
        assert_eq!(name_key("CLOUD"), name_key("cloud"));
    }

    #[test]
    fn punctuation_lookalikes_fold() {
        assert_eq!(name_key("O\u{2019}Neil"), name_key("O'Neil"));
        assert_eq!(name_key("O`Neil"), name_key("O'Neil"));
        assert_eq!(name_key("Jean\u{2013}Luc"), name_key("Jean-Luc"));
        assert_eq!(name_key("Jean\u{2212}Luc"), name_key("Jean-Luc"));
        // But different punctuation stays different.
        assert_ne!(name_key("Jean-Luc"), name_key("Jean_Luc"));
        assert_ne!(name_key("Jean Luc"), name_key("JeanLuc"));
    }

    #[test]
    fn what_the_key_deliberately_keeps_apart() {
        assert_ne!(
            name_key("Jos\u{00E9}"),
            name_key("Jose"),
            "accents make a different name"
        );
        assert_ne!(name_key("Cl0ud"), name_key("Cloud"));
        assert_ne!(
            name_key("CIoud"),
            name_key("Cloud"),
            "ASCII lookalikes are a documented limit"
        );
        assert_ne!(name_key("Cloud2"), name_key("Cloud"));
    }

    #[test]
    fn the_key_of_nothing_visible_is_empty() {
        for raw in ["", " ", "\u{3164}", "\u{200B}\u{FEFF}", "\u{0301}"] {
            assert_eq!(name_key(raw), "", "{raw:?}");
        }
    }

    #[test]
    fn the_key_stays_bounded_for_expanding_characters() {
        let worst = "\u{FDFA}".repeat(AvatarAppearance::MAX_NAME + 10);
        let key = name_key(&worst);
        // 24 clamped characters, each at most 18 after NFKC.
        assert!(
            key.chars().count() <= AvatarAppearance::MAX_NAME * 18,
            "{}",
            key.chars().count()
        );
    }

    fn accepts(raw: &str) {
        assert!(
            AvatarAppearance::validate_new_name(raw).is_ok(),
            "{raw:?} should be accepted"
        );
    }

    fn rejects(raw: &str, why: NameError) {
        assert_eq!(
            AvatarAppearance::validate_new_name(raw),
            Err(why),
            "{raw:?}"
        );
    }

    #[test]
    fn single_script_names_are_accepted() {
        for raw in [
            "Cloud",
            "Red XIII",
            "O'Neil-2",
            "Aerith \u{1F338}",
            "1234",
            "\u{1F338}",
            "---",
            "\u{00C4}rger",
            "\u{041A}\u{043B}\u{0430}\u{0443}\u{0434}", // Cyrillic
            "\u{039A}\u{03BB}\u{03B1}\u{03C5}\u{03B4}", // Greek
            "\u{30AF}\u{30E9}\u{30A6}\u{30C9}",         // Katakana
            "\u{30AF}\u{30E9}\u{30A6}\u{30C9}2",        // Katakana and a digit
            "\u{30C6}\u{30A3}\u{30D5}\u{30A1}LOVE",     // Katakana and Latin
            "Cloud\u{592A}\u{90CE}",                    // Latin and Han
            "\u{592A}\u{90CE}\u{3055}\u{3093}",         // Han and Hiragana
            "\u{D55C}\u{AE00}Cloud",                    // Hangul and Latin
            "\u{6CE8}\u{97F3}\u{3105}\u{3106}",         // Han and Bopomofo
            "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",         // Hebrew
            "\u{0645}\u{062D}\u{0645}\u{062F}",         // Arabic
            "\u{0E2A}\u{0E27}\u{0E31}\u{0E2A}\u{0E14}\u{0E35}", // Thai
            "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}", // Devanagari
        ] {
            accepts(raw);
        }
    }

    #[test]
    fn mixed_script_names_are_rejected() {
        for raw in [
            "\u{0422}ifa",                                    // Cyrillic T + Latin
            "Cl\u{043E}ud",                                   // Cyrillic o in Cloud
            "\u{0391}erith",                                  // Greek capital alpha in Aerith
            "Cloud \u{03A3}\u{03BF}\u{03C6}\u{03AF}\u{03B1}", // Latin + Greek
            "\u{041A}\u{03BB}\u{0430}",                       // Cyrillic + Greek
            "Ali \u{0639}\u{0644}\u{064A}",                   // Latin + Arabic
            "\u{05E9}\u{05DC}Cloud",                          // Hebrew + Latin
            "\u{30AF}\u{D55C}",                               // Katakana + Hangul
            "\u{3042}\u{3105}",                               // Hiragana + Bopomofo
            "\u{0E2A}\u{0928}",                               // Thai + Devanagari
            "Tifa\u{0661}",                                   // Latin + Arabic-Indic digit one
        ] {
            rejects(raw, NameError::MixedScripts);
        }
        // NFKC can turn a character of no script into a Latin letter beside Cyrillic.
        rejects("\u{1D413}\u{0438}\u{0444}\u{0430}", NameError::MixedScripts);
    }

    #[test]
    fn exotic_scripts_and_unassigned_characters_are_refused() {
        rejects("\u{13A0}\u{13A1}", NameError::UnsupportedCharacters); // Cherokee
        rejects("\u{A4EE}\u{A4F8}", NameError::UnsupportedCharacters); // Lisu
        rejects("\u{16A0}\u{16A2}", NameError::UnsupportedCharacters); // Runic
        rejects("\u{E000}", NameError::UnsupportedCharacters); // private use
        rejects("Tifa\u{0378}", NameError::UnsupportedCharacters); // unassigned
        rejects("\u{3164}", NameError::Empty);
        rejects("   ", NameError::Empty);
        rejects("", NameError::Empty);
    }

    #[test]
    fn validate_returns_the_cleaned_name() {
        assert_eq!(
            AvatarAppearance::validate_new_name("  Cloud\u{3164}  Strife\n").as_deref(),
            Ok("Cloud Strife")
        );
        assert_eq!(
            AvatarAppearance::validate_new_name("A\u{0308}rger").as_deref(),
            Ok("\u{00C4}rger")
        );
        assert_eq!(
            NameError::MixedScripts.to_string(),
            "the display name mixes writing systems (for example Latin with Cyrillic or Greek)"
        );
    }

    /// Characters that exercise every rule: fillers, marks, spaces, scripts, lookalikes.
    fn troublesome_alphabet() -> Vec<char> {
        let mut pool: Vec<char> = "Tifa Cloud x-'_0O1lI".chars().collect();
        for cp in [
            0x3164u32, 0x2800, 0x034F, 0xFE0F, 0xFFA0, 0x115F, 0x200B, 0x202E, 0x00A0, 0x3000,
            0x2003, 0x0009, 0x000A, 0x0301, 0x0308, 0x0323, 0x0302, 0x00C4, 0x0041, 0x0422, 0x0430,
            0x03BF, 0x0391, 0x30AF, 0x592A, 0x3042, 0xD55C, 0x1F338, 0xFF34, 0x1D413, 0x00DF,
            0x03C2, 0x03A3, 0xFDFA, 0x2019, 0x2013, 0x0E2A, 0x0661, 0xE000, 0x0378, 0x1100, 0x1161,
            0x3099,
        ] {
            pool.push(char::from_u32(cp).unwrap());
        }
        pool
    }

    #[test]
    fn the_laws_hold_on_pseudo_random_text() {
        let pool = troublesome_alphabet();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..30_000 {
            let len = (next() % 40) as usize;
            let raw: String = (0..len)
                .map(|_| pool[(next() % pool.len() as u64) as usize])
                .collect();
            match clean(&raw) {
                Some(cleaned) => {
                    assert!(
                        cleaned.chars().count() <= AvatarAppearance::MAX_NAME,
                        "{raw:?}"
                    );
                    assert_eq!(
                        clean(&cleaned).as_deref(),
                        Some(cleaned.as_str()),
                        "not idempotent: {raw:?}"
                    );
                    assert!(
                        !cleaned.starts_with(' ')
                            && !cleaned.ends_with(' ')
                            && !cleaned.contains("  "),
                        "{raw:?} -> {cleaned:?}"
                    );
                    assert!(
                        cleaned
                            .chars()
                            .all(|c| !c.is_control() && !is_invisible_format(c)),
                        "{raw:?} -> {cleaned:?}"
                    );
                    assert_eq!(name_key(&raw), name_key(&cleaned), "{raw:?}");
                    assert!(!name_key(&raw).is_empty(), "{raw:?}");
                    // Admission either returns the cleaned name or refuses; it never rewrites.
                    if let Ok(admitted) = AvatarAppearance::validate_new_name(&raw) {
                        assert_eq!(admitted, cleaned, "{raw:?}");
                    }
                }
                None => {
                    assert_eq!(name_key(&raw), "", "{raw:?}");
                    assert_eq!(
                        AvatarAppearance::validate_new_name(&raw),
                        Err(NameError::Empty),
                        "{raw:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn invisible_set_matches_the_default_ignorable_property_in_the_ranges_we_rely_on() {
        // Spot-check the boundaries of the ranges written out above.
        for cp in [
            0xE0000u32, 0xE0001, 0xE0020, 0xE007F, 0xE0080, 0xE00FF, 0xE0100, 0xE01EF, 0xE01F0,
            0xE0FFF,
        ] {
            assert!(is_invisible_format(char::from_u32(cp).unwrap()), "U+{cp:X}");
        }
        for cp in [0xE1000u32, 0xDFFFF] {
            assert!(
                !is_invisible_format(char::from_u32(cp).unwrap()),
                "U+{cp:X}"
            );
        }
        for cp in [0x180B, 0x180C, 0x180D, 0x180E, 0x180F] {
            assert!(is_invisible_format(char::from_u32(cp).unwrap()), "U+{cp:X}");
        }
        for cp in [0xFFEF, 0xFFFC, 0xFFFD] {
            assert!(
                !is_invisible_format(char::from_u32(cp).unwrap()),
                "U+{cp:X}"
            );
        }
        for cp in [0xFFF0, 0xFFF8, 0xFFF9, 0xFFFB] {
            assert!(is_invisible_format(char::from_u32(cp).unwrap()), "U+{cp:X}");
        }
    }
}
