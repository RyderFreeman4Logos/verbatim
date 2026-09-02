//! Bible / Scripture canonical source profile.
//!
//! Implements reference parsing, normalization, and citation rendering for
//! Bible book/chapter/verse references (e.g., "John 3:16", "1 Cor 13:4-7").
//!
//! This is the first concrete profile; the generic data model lives in
//! [`crate::types`] and [`super`].

pub mod canon_registry;
pub mod versification_registry;

use super::{
    default_normalize, default_render, ParsedReference, ReferenceConfidence, SourceProfile,
};
use crate::types::{CanonicalLocator, ReferenceComponent};
use canon_registry::CanonRegistry;

/// Find a book by its full name or abbreviation through the canonical registry.
fn resolve_book(input: &str) -> Option<(usize, &'static str)> {
    CanonRegistry::resolve(input).map(|book| (book.ordinal as usize, book.name))
}

/// Parse a Bible reference string like "John 3:16", "1 Cor 13:4-7", "John 3:16-18",
/// or a chapter-only / same-book chapter-range reference like "John 3" or "John 3-5".
///
/// Chapter-only and chapter-range references are represented explicitly with a
/// book + chapter (no fabricated verse) and carry `ReferenceConfidence::Low`,
/// below exact-verse references. Returns `None` if the input does not look like
/// a Bible reference, or the range is reversed / malformed.
pub fn parse_bible_reference(input: &str) -> Option<ParsedReference> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Find the last space-separated token that looks like "chapter:verse" or
    // "chapter"; everything before it is the book name. Book names may contain
    // digits and spaces (e.g. "1 Corinthians").
    let colon_pos = input.find(':');
    let space_before_ref = match colon_pos {
        Some(cp) => input[..cp].rfind(' ')?,
        None => input.rfind(' ')?,
    };

    let book_part = input[..space_before_ref].trim();
    let ref_part = input[space_before_ref..].trim();

    let (ordinal, book_name) = resolve_book(book_part)?;

    // Parse the reference part. Returns the start/end chapters plus optional
    // verse bounds (None for chapter-only / chapter-range references).
    let (start_ch, end_ch, verse_start, verse_end) = parse_ref_part(ref_part)?;

    // Reversed chapter ranges fail closed with a stable diagnostic (None).
    if end_ch < start_ch {
        return None;
    }

    let book_comp = |ch: u32| ReferenceComponent {
        level: "chapter".into(),
        value: ch.to_string(),
        ordinal: Some(ch),
    };

    // Start components: book + chapter (+ verse when this is a verse reference).
    let mut start = vec![
        ReferenceComponent {
            level: "book".into(),
            value: book_name.to_string(),
            ordinal: Some(ordinal as u32),
        },
        book_comp(start_ch),
    ];
    if let Some(vs) = verse_start {
        start.push(ReferenceComponent {
            level: "verse".into(),
            value: vs.to_string(),
            ordinal: Some(vs),
        });
    }

    // End components: verse ranges and same-book chapter ranges.
    let is_verse = verse_start.is_some();
    let end = if let Some(ve) = verse_end {
        let mut e = vec![
            ReferenceComponent {
                level: "book".into(),
                value: book_name.to_string(),
                ordinal: Some(ordinal as u32),
            },
            book_comp(end_ch),
        ];
        e.push(ReferenceComponent {
            level: "verse".into(),
            value: ve.to_string(),
            ordinal: Some(ve),
        });
        Some(e)
    } else if end_ch != start_ch {
        Some(vec![
            ReferenceComponent {
                level: "book".into(),
                value: book_name.to_string(),
                ordinal: Some(ordinal as u32),
            },
            book_comp(end_ch),
        ])
    } else {
        None
    };

    let display = if is_verse {
        match verse_end {
            Some(ve) if end_ch == start_ch => {
                format!("{book_name} {start_ch}:{}-{ve}", verse_start.unwrap())
            }
            Some(ve) => format!(
                "{book_name} {start_ch}:{}-{end_ch}:{ve}",
                verse_start.unwrap()
            ),
            None => format!("{book_name} {start_ch}:{}", verse_start.unwrap()),
        }
    } else if end_ch != start_ch {
        format!("{book_name} {start_ch}-{end_ch}")
    } else {
        format!("{book_name} {start_ch}")
    };

    Some(ParsedReference {
        profile_id: "bible".into(),
        raw: input.to_string(),
        start,
        end,
        display,
        confidence: if is_verse {
            ReferenceConfidence::High
        } else {
            // Chapter-only / chapter-range references are plausible but not as
            // unambiguous as an exact verse.
            ReferenceConfidence::Low
        },
    })
}

/// Parse a reference part like "3", "3-5", "3:16", "3:16-18", "3:16-4:2".
///
/// Returns `(start_chapter, end_chapter, verse_start, verse_end)`. For chapter
/// references `verse_start`/`verse_end` are `None`; `end_chapter` equals
/// `start_chapter` for a single chapter or holds the range end for "3-5".
fn parse_ref_part(s: &str) -> Option<(u32, u32, Option<u32>, Option<u32>)> {
    let s = s.trim();

    if let Some(colon) = s.find(':') {
        let chapter: u32 = s[..colon].trim().parse().ok()?;
        let rest = s[colon + 1..].trim();

        if let Some(dash) = rest.find('-') {
            let start: u32 = rest[..dash].trim().parse().ok()?;
            let end_part = rest[dash + 1..].trim();
            // "3:16-4:2" (cross-chapter verse range)
            if let Some(end_colon) = end_part.find(':') {
                let end_chapter: u32 = end_part[..end_colon].trim().parse().ok()?;
                let end_verse: u32 = end_part[end_colon + 1..].trim().parse().ok()?;
                Some((chapter, end_chapter, Some(start), Some(end_verse)))
            } else {
                let end_verse: u32 = end_part.parse().ok()?;
                Some((chapter, chapter, Some(start), Some(end_verse)))
            }
        } else {
            let verse: u32 = rest.parse().ok()?;
            Some((chapter, chapter, Some(verse), None))
        }
    } else if let Some(dash) = s.find('-') {
        // "3-5" (same-book chapter range)
        let start: u32 = s[..dash].trim().parse().ok()?;
        let end: u32 = s[dash + 1..].trim().parse().ok()?;
        Some((start, end, None, None))
    } else {
        // "3" (chapter only)
        let chapter: u32 = s.trim().parse().ok()?;
        Some((chapter, chapter, None, None))
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
    fn chapter_only_is_typed_without_verse() {
        let parsed = parse_bible_reference("John 3").unwrap();
        assert_eq!(parsed.display, "John 3");
        assert!(!parsed.display.contains(':'));
        assert_eq!(parsed.start.len(), 2);
        assert_eq!(parsed.start[0].value, "John");
        assert_eq!(parsed.start[1].value, "3");
        assert_eq!(parsed.start[1].level, "chapter");
        assert!(parsed.end.is_none());
        // Never fabricate a verse component.
        assert!(!parsed.start.iter().any(|c| c.level == "verse"));
        // Confidence is below an exact verse.
        assert_eq!(parsed.confidence, ReferenceConfidence::Low);
    }

    #[test]
    fn chapter_range_is_typed_without_verse() {
        let parsed = parse_bible_reference("John 3-5").unwrap();
        assert_eq!(parsed.display, "John 3-5");
        assert!(!parsed.display.contains(':'));
        assert_eq!(parsed.start.len(), 2);
        assert_eq!(parsed.start[1].value, "3");
        let end = parsed.end.unwrap();
        assert_eq!(end.len(), 2);
        assert_eq!(end[0].value, "John");
        assert_eq!(end[1].value, "5");
        assert_eq!(end[1].level, "chapter");
        assert!(!end.iter().any(|c| c.level == "verse"));
        assert_eq!(parsed.confidence, ReferenceConfidence::Low);
    }

    #[test]
    fn chapter_reference_aliases_resolve_without_inventing_verse() {
        for input in ["John 3", "john 3", " John   3 ", "Jn 3", "John 3-5"] {
            let parsed = parse_bible_reference(input)
                .unwrap_or_else(|| panic!("{input} must parse as a typed chapter reference"));
            assert!(
                !parsed.start.iter().any(|c| c.level == "verse"),
                "{input} must not fabricate a verse"
            );
            assert!(
                !parsed.display.contains(':'),
                "{input}: {0}",
                parsed.display
            );
        }
        assert_eq!(parse_bible_reference("Jn 3").unwrap().display, "John 3");
        assert_eq!(
            parse_bible_reference("1 Cor 13").unwrap().display,
            "1 Corinthians 13"
        );
    }

    #[test]
    fn public_registry_keeps_chapter_reference_fail_closed() {
        // The public gate still rejects low-confidence chapter-only references,
        // while the profile itself represents them as typed chapter ranges.
        let profile = BibleProfile::new();
        let registry = crate::profiles::ProfileRegistry::new();
        for input in ["John 3", "john 3", "Jn 3", "John 3-5"] {
            assert!(profile.parse_reference(input).is_some(), "{input}");
            assert!(registry.try_parse(input).is_none(), "{input}");
        }
    }

    #[test]
    fn reversed_and_malformed_chapter_ranges_fail_closed() {
        assert!(parse_bible_reference("John 5-3").is_none());
        assert!(parse_bible_reference("John 3:").is_none());
        assert!(parse_bible_reference("John :16").is_none());
        assert!(parse_bible_reference("John 3-").is_none());
        assert!(parse_bible_reference("John -3").is_none());
        assert!(parse_bible_reference("John x").is_none());
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
