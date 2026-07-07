//! Bible / Scripture canonical source profile.
//!
//! Implements reference parsing, normalization, and citation rendering for
//! Bible book/chapter/verse references (e.g., "John 3:16", "1 Cor 13:4-7").
//!
//! This is the first concrete profile; the generic data model lives in
//! [`crate::types`] and [`super`].

use super::{
    default_normalize, default_render, ParsedReference, ReferenceConfidence, SourceProfile,
};
use crate::types::{CanonicalLocator, ReferenceComponent};

/// Protestant 66-book canonical order.
const BIBLE_BOOKS: &[(&str, &[&str])] = &[
    ("Genesis", &["gen", "ge", "gn"]),
    ("Exodus", &["ex", "exo", "exod"]),
    ("Leviticus", &["lev", "lv", "le"]),
    ("Numbers", &["num", "nm", "nb"]),
    ("Deuteronomy", &["deut", "dt", "deutronomy"]),
    ("Joshua", &["josh", "jos", "jsh"]),
    ("Judges", &["judg", "jdg", "jdgs"]),
    ("Ruth", &["ru", "rth"]),
    ("1 Samuel", &["1 sam", "1sam", "i sam", "1 sm", "1sa"]),
    ("2 Samuel", &["2 sam", "2sam", "ii sam", "2 sm", "2sa"]),
    ("1 Kings", &["1 kgs", "1kgs", "i kgs", "1 ki", "1ki"]),
    ("2 Kings", &["2 kgs", "2kgs", "ii kgs", "2 ki", "2ki"]),
    ("1 Chronicles", &["1 chron", "1chron", "1 chr", "1ch"]),
    ("2 Chronicles", &["2 chron", "2chron", "2 chr", "2ch"]),
    ("Ezra", &["ezr", "ez"]),
    ("Nehemiah", &["neh", "ne"]),
    ("Esther", &["est", "esth"]),
    ("Job", &["jb"]),
    ("Psalms", &["ps", "psa", "pslm", "psalm"]),
    ("Proverbs", &["prov", "prv", "pr"]),
    ("Ecclesiastes", &["eccles", "eccl", "ec", "qoh"]),
    (
        "Song of Songs",
        &["song", "sos", "sg", "canticles", "song of solomon"],
    ),
    ("Isaiah", &["isa", "is"]),
    ("Jeremiah", &["jer", "je", "jr"]),
    ("Lamentations", &["lam", "lm"]),
    ("Ezekiel", &["ezek", "eze", "ezk"]),
    ("Daniel", &["dan", "dn", "da"]),
    ("Hosea", &["hos", "ho"]),
    ("Joel", &["joel", "jl"]),
    ("Amos", &["am", "amo"]),
    ("Obadiah", &["obad", "ob"]),
    ("Jonah", &["jonah", "jon", "jnh"]),
    ("Micah", &["mic", "mi"]),
    ("Nahum", &["nah", "na"]),
    ("Habakkuk", &["hab", "hk"]),
    ("Zephaniah", &["zeph", "zep", "zp"]),
    ("Haggai", &["hag", "hg"]),
    ("Zechariah", &["zech", "zec", "zc"]),
    ("Malachi", &["mal", "ml"]),
    ("Matthew", &["matt", "mt"]),
    ("Mark", &["mk", "mrk"]),
    ("Luke", &["lk", "luk"]),
    ("John", &["jn", "jhn"]),
    ("Acts", &["ac"]),
    ("Romans", &["rom", "ro", "rm"]),
    ("1 Corinthians", &["1 cor", "1cor", "i cor", "1co"]),
    ("2 Corinthians", &["2 cor", "2cor", "ii cor", "2co"]),
    ("Galatians", &["gal", "ga"]),
    ("Ephesians", &["eph", "ephes"]),
    ("Philippians", &["phil", "php", "pp"]),
    ("Colossians", &["col", "cl"]),
    ("1 Thessalonians", &["1 thess", "1thess", "1 thes", "1th"]),
    ("2 Thessalonians", &["2 thess", "2thess", "2 thes", "2th"]),
    ("1 Timothy", &["1 tim", "1tim", "1 ti", "1ti"]),
    ("2 Timothy", &["2 tim", "2tim", "2 ti", "2ti"]),
    ("Titus", &["tit", "ti"]),
    ("Philemon", &["phlm", "philem", "phm"]),
    ("Hebrews", &["heb", "he"]),
    ("James", &["jas", "jm"]),
    ("1 Peter", &["1 pet", "1pet", "1 pe", "1pe"]),
    ("2 Peter", &["2 pet", "2pet", "2 pe", "2pe"]),
    ("1 John", &["1 john", "1john", "1 jn", "1jn"]),
    ("2 John", &["2 john", "2john", "2 jn", "2jn"]),
    ("3 John", &["3 john", "3john", "3 jn", "3jn"]),
    ("Jude", &["jd"]),
    ("Revelation", &["rev", "revelation", "apocalypse", "apoc"]),
];

/// Find a book by its full name or abbreviation.
fn resolve_book(input: &str) -> Option<(usize, &'static str)> {
    let normalized = input.trim().to_lowercase();
    for (ordinal, (name, abbrs)) in BIBLE_BOOKS.iter().enumerate() {
        if name.to_lowercase() == normalized {
            return Some((ordinal + 1, name));
        }
        for abbr in *abbrs {
            if *abbr == normalized {
                return Some((ordinal + 1, name));
            }
        }
    }
    None
}

/// Parse a Bible reference string like "John 3:16" or "1 Cor 13:4-7" or "John 3:16-18".
///
/// Returns `None` if the input does not look like a Bible reference.
pub fn parse_bible_reference(input: &str) -> Option<ParsedReference> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Try to match the book name. Book names may contain digits and spaces
    // (e.g., "1 Corinthians"). We need to greedily try longer prefixes first.
    // Strategy: find the last space-separated token that looks like "chapter:verse"
    // or "chapter", then everything before it is the book name.

    let colon_pos = input.find(':');
    let space_before_ref = match colon_pos {
        Some(cp) => input[..cp].rfind(' ')?,
        None => input.rfind(' ')?,
    };

    let book_part = input[..space_before_ref].trim();
    let ref_part = input[space_before_ref..].trim();

    let (ordinal, book_name) = resolve_book(book_part)?;

    // Parse the reference part: "3:16", "3:16-18", "3", "3:16-4:2"
    let (chapter, verse_start, verse_end, cross_chapter) = parse_ref_part(ref_part)?;

    // Build components
    let start = vec![
        ReferenceComponent {
            level: "book".into(),
            value: book_name.to_string(),
            ordinal: Some(ordinal as u32),
        },
        ReferenceComponent {
            level: "chapter".into(),
            value: chapter.to_string(),
            ordinal: Some(chapter),
        },
        ReferenceComponent {
            level: "verse".into(),
            value: verse_start.to_string(),
            ordinal: Some(verse_start),
        },
    ];

    let end = verse_end.map(|end_verse| {
        let end_chapter = cross_chapter.unwrap_or(chapter);
        vec![
            ReferenceComponent {
                level: "book".into(),
                value: book_name.to_string(),
                ordinal: Some(ordinal as u32),
            },
            ReferenceComponent {
                level: "chapter".into(),
                value: end_chapter.to_string(),
                ordinal: Some(end_chapter),
            },
            ReferenceComponent {
                level: "verse".into(),
                value: end_verse.to_string(),
                ordinal: Some(end_verse),
            },
        ]
    });

    let display = if let Some(end_verse) = verse_end {
        let end_ch = cross_chapter.unwrap_or(chapter);
        if end_ch == chapter {
            format!("{book_name} {chapter}:{verse_start}-{end_verse}")
        } else {
            format!("{book_name} {chapter}:{verse_start}-{end_ch}:{end_verse}")
        }
    } else {
        format!("{book_name} {chapter}:{verse_start}")
    };

    Some(ParsedReference {
        profile_id: "bible".into(),
        raw: input.to_string(),
        start,
        end,
        display,
        confidence: ReferenceConfidence::High,
    })
}

/// Parse "3:16", "3:16-18", "3", "3:16-4:2"
/// Returns (chapter, verse_start, verse_end, cross_chapter_end)
fn parse_ref_part(s: &str) -> Option<(u32, u32, Option<u32>, Option<u32>)> {
    let s = s.trim();

    if let Some(colon) = s.find(':') {
        let chapter: u32 = s[..colon].trim().parse().ok()?;
        let rest = s[colon + 1..].trim();

        if let Some(dash) = rest.find('-') {
            let start: u32 = rest[..dash].trim().parse().ok()?;
            let end_part = rest[dash + 1..].trim();
            // Check for "3:16-4:2" (cross-chapter)
            if let Some(end_colon) = end_part.find(':') {
                let end_chapter: u32 = end_part[..end_colon].trim().parse().ok()?;
                let end_verse: u32 = end_part[end_colon + 1..].trim().parse().ok()?;
                Some((chapter, start, Some(end_verse), Some(end_chapter)))
            } else {
                let end_verse: u32 = end_part.parse().ok()?;
                Some((chapter, start, Some(end_verse), None))
            }
        } else {
            let verse: u32 = rest.parse().ok()?;
            Some((chapter, verse, None, None))
        }
    } else {
        // Just a chapter number, no verse — "John 3"
        let chapter: u32 = s.parse().ok()?;
        // Chapter-only is lower confidence (too ambiguous)
        // We return it but mark it so the caller can decide
        // For now return verse 1 to have a complete reference
        Some((chapter, 1, None, None))
    }
}

/// Bible source profile.
pub struct BibleProfile;

impl BibleProfile {
    pub fn new() -> Self {
        Self
    }
}

impl SourceProfile for BibleProfile {
    fn id(&self) -> &str {
        "bible"
    }

    fn parse_reference(&self, input: &str) -> Option<ParsedReference> {
        parse_bible_reference(input)
    }

    fn render_citation(&self, locator: &CanonicalLocator) -> String {
        if locator.start.is_empty() {
            return String::new();
        }
        // Use the display field if present
        if !locator.display.is_empty() {
            return locator.display.clone();
        }
        default_render(&locator.start)
    }

    fn normalize(&self, components: &[ReferenceComponent]) -> String {
        default_normalize(components)
    }
}

impl Default for BibleProfile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_verse() {
        let parsed = parse_bible_reference("John 3:16").unwrap();
        assert_eq!(parsed.display, "John 3:16");
        assert_eq!(parsed.start.len(), 3);
        assert_eq!(parsed.start[0].value, "John");
        assert_eq!(parsed.start[0].ordinal, Some(43));
        assert_eq!(parsed.start[1].value, "3");
        assert_eq!(parsed.start[2].value, "16");
        assert!(parsed.end.is_none());
    }

    #[test]
    fn parse_verse_range() {
        let parsed = parse_bible_reference("John 3:16-18").unwrap();
        assert_eq!(parsed.display, "John 3:16-18");
        let end = parsed.end.unwrap();
        assert_eq!(end[2].value, "18");
    }

    #[test]
    fn parse_cross_chapter_range() {
        let parsed = parse_bible_reference("Genesis 1:31-2:3").unwrap();
        assert_eq!(parsed.display, "Genesis 1:31-2:3");
        assert_eq!(parsed.start[1].value, "1");
        assert_eq!(parsed.start[2].value, "31");
        let end = parsed.end.unwrap();
        assert_eq!(end[1].value, "2");
        assert_eq!(end[2].value, "3");
    }

    #[test]
    fn parse_abbreviated_book() {
        let parsed = parse_bible_reference("1 Cor 13:4-7").unwrap();
        assert_eq!(parsed.display, "1 Corinthians 13:4-7");
        assert_eq!(parsed.start[0].value, "1 Corinthians");
        assert_eq!(parsed.start[0].ordinal, Some(46));
    }

    #[test]
    fn parse_jn_abbreviation() {
        let parsed = parse_bible_reference("Jn 3:16").unwrap();
        assert_eq!(parsed.display, "John 3:16");
    }

    #[test]
    fn parse_gen_abbreviation() {
        let parsed = parse_bible_reference("Gen 1:1").unwrap();
        assert_eq!(parsed.display, "Genesis 1:1");
    }

    #[test]
    fn reject_non_reference() {
        assert!(parse_bible_reference("love is patient").is_none());
        assert!(parse_bible_reference("section 3 is confusing").is_none());
        assert!(parse_bible_reference("").is_none());
    }

    #[test]
    fn reject_unknown_book() {
        assert!(parse_bible_reference("Book 3:16").is_none());
        assert!(parse_bible_reference("XX 3:16").is_none());
    }

    #[test]
    fn registry_finds_bible_profile() {
        let registry = crate::profiles::ProfileRegistry::new();
        let profile = registry.get("bible");
        assert!(profile.is_some());
    }

    #[test]
    fn registry_parses_bible_reference() {
        let registry = crate::profiles::ProfileRegistry::new();
        let parsed = registry.try_parse("John 3:16");
        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap().profile_id, "bible");
    }

    #[test]
    fn registry_rejects_low_confidence() {
        let registry = crate::profiles::ProfileRegistry::new();
        assert!(registry.try_parse("hello world").is_none());
    }
}
