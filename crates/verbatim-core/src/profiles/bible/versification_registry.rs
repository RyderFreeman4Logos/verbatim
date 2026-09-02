//! Versioned Protestant Bible versification bounds.
//!
//! Book identity remains owned by [`super::canon_registry::CanonRegistry`].
//! This registry only owns reference geometry, so the two contracts can evolve
//! independently without duplicating the book table.

use super::canon_registry::{CanonRegistry, VERSION as CANON_VERSION};

/// Stable version token for the Protestant versification contract.
pub const VERSION: &str = "protestant-66/v1";

/// A canonical verse coordinate returned by a bounded lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerseAddress {
    pub book_id: &'static str,
    pub chapter: u16,
    pub verse: u16,
}

/// One explicit source-to-target mapping entry.
///
/// The walking skeleton contains the identity mapping for John 3:16. Split and
/// merge mappings can be added without changing the lookup API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerseMapping {
    pub source: VerseAddress,
    pub targets: &'static [VerseAddress],
}

/// Versioned bounds and mappings for the Protestant book universe.
#[derive(Debug, Clone, Copy)]
pub struct VersificationRegistry;

const JOHN_CHAPTER_MAX_VERSES: &[u16] = &[
    51, 25, 36, 54, 47, 71, 53, 59, 41, 42, 57, 50, 38, 31, 27, 33, 26, 40, 42, 31, 25,
];
const JOHN_316: VerseAddress = VerseAddress {
    book_id: "JHN",
    chapter: 3,
    verse: 16,
};
const JOHN_316_TARGETS: &[VerseAddress] = &[JOHN_316];

/// Explicit mapping table. It is deliberately small in this walking skeleton.
pub const MAPPINGS: &[VerseMapping] = &[VerseMapping {
    source: JOHN_316,
    targets: JOHN_316_TARGETS,
}];

impl VersificationRegistry {
    /// The version token for this registry's data contract.
    pub const fn version() -> &'static str {
        VERSION
    }

    /// Resolve only the supported version; unknown IDs fail closed.
    pub fn by_id(id: &str) -> Option<Self> {
        (id.trim() == VERSION).then_some(Self)
    }

    /// This walking skeleton is paired only with the existing Protestant canon.
    pub fn compatible_with_canon(canon_id: &str) -> bool {
        canon_id.trim() == CANON_VERSION
    }

    /// Return a verse only when its book, chapter, and verse are bounded.
    ///
    /// All book IDs are checked through `CanonRegistry`; John has exact
    /// chapter bounds here. Other Protestant books use a conservative bounded
    /// skeleton until their complete chapter matrix is introduced.
    pub fn lookup(book_id: &str, chapter: u16, verse: u16) -> Option<VerseAddress> {
        let book = CanonRegistry::by_id(book_id)?;
        if chapter == 0 || verse == 0 {
            return None;
        }
        if book.id == "JHN" {
            let max_verse = JOHN_CHAPTER_MAX_VERSES
                .get(chapter.checked_sub(1)? as usize)
                .copied()?;
            return (verse <= max_verse).then_some(VerseAddress {
                book_id: book.id,
                chapter,
                verse,
            });
        }
        // The complete matrix is intentionally deferred; keep the lookup
        // bounded for every known Protestant book in this walking skeleton.
        (chapter <= 150 && verse <= 176).then_some(VerseAddress {
            book_id: book.id,
            chapter,
            verse,
        })
    }

    /// Return the explicit mapping for a source coordinate, if present.
    pub fn mapping(source: VerseAddress) -> Option<&'static VerseMapping> {
        MAPPINGS.iter().find(|mapping| mapping.source == source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_version_and_rejects_unknown_ids() {
        assert!(VersificationRegistry::by_id(VERSION).is_some());
        assert!(VersificationRegistry::by_id("unknown").is_none());
        assert!(VersificationRegistry::compatible_with_canon(CANON_VERSION));
        assert!(!VersificationRegistry::compatible_with_canon("unknown"));
    }

    #[test]
    fn lookup_is_bounded_and_reuses_canon_book_ids() {
        assert_eq!(VersificationRegistry::lookup("JHN", 3, 16), Some(JOHN_316));
        assert!(VersificationRegistry::lookup("JHN", 3, 37).is_none());
        assert!(VersificationRegistry::lookup("JHN", 22, 1).is_none());
        assert!(VersificationRegistry::lookup("XYZ", 3, 16).is_none());
    }

    #[test]
    fn mapping_table_contains_identity_example() {
        assert_eq!(
            VersificationRegistry::mapping(JOHN_316).unwrap().targets,
            JOHN_316_TARGETS
        );
    }
}
