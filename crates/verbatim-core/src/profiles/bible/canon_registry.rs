//! Versioned canonical registry of Protestant Bible books.
//!
//! The registry is the single source of truth for book identity, aliases,
//! and canonical order. It replaces the hard-coded table that previously
//! lived in this module's parent. The version token pins this registry's
//! data contract so future registries (versification, localized alias
//! packs, deuterocanon) can coexist without ambiguity.

/// Stable version token for this registry's data contract.
pub const VERSION: &str = "protestant-66/v1";

/// One canonical book: stable ID, display name, aliases, and 1-based ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonBook {
    /// Stable USFM/OSIS-style identifier, e.g. `GEN`, `JHN`, `REV`.
    pub id: &'static str,
    /// Display name, e.g. `1 Corinthians`.
    pub name: &'static str,
    /// Recognized abbreviations and alternate names, lowercased.
    pub aliases: &'static [&'static str],
    /// 1-based position in Protestant canonical order.
    pub ordinal: u16,
}

/// Versioned registry of the 66 Protestant canonical books in order.
#[derive(Debug, Clone, Copy)]
pub struct CanonRegistry;

impl CanonRegistry {
    /// The 66 entries in canonical order.
    pub const BOOKS: &'static [CanonBook] = &[
        CanonBook {
            id: "GEN",
            name: "Genesis",
            aliases: &["gen", "ge", "gn"],
            ordinal: 1,
        },
        CanonBook {
            id: "EXO",
            name: "Exodus",
            aliases: &["ex", "exo", "exod"],
            ordinal: 2,
        },
        CanonBook {
            id: "LEV",
            name: "Leviticus",
            aliases: &["lev", "lv", "le"],
            ordinal: 3,
        },
        CanonBook {
            id: "NUM",
            name: "Numbers",
            aliases: &["num", "nm", "nb"],
            ordinal: 4,
        },
        CanonBook {
            id: "DEU",
            name: "Deuteronomy",
            aliases: &["deut", "dt", "deutronomy"],
            ordinal: 5,
        },
        CanonBook {
            id: "JOS",
            name: "Joshua",
            aliases: &["josh", "jos", "jsh"],
            ordinal: 6,
        },
        CanonBook {
            id: "JDG",
            name: "Judges",
            aliases: &["judg", "jdg", "jdgs"],
            ordinal: 7,
        },
        CanonBook {
            id: "RUT",
            name: "Ruth",
            aliases: &["ru", "rth"],
            ordinal: 8,
        },
        CanonBook {
            id: "1SA",
            name: "1 Samuel",
            aliases: &["1 sam", "1sam", "i sam", "1 sm", "1sa"],
            ordinal: 9,
        },
        CanonBook {
            id: "2SA",
            name: "2 Samuel",
            aliases: &["2 sam", "2sam", "ii sam", "2 sm", "2sa"],
            ordinal: 10,
        },
        CanonBook {
            id: "1KI",
            name: "1 Kings",
            aliases: &["1 kgs", "1kgs", "i kgs", "1 ki", "1ki"],
            ordinal: 11,
        },
        CanonBook {
            id: "2KI",
            name: "2 Kings",
            aliases: &["2 kgs", "2kgs", "ii kgs", "2 ki", "2ki"],
            ordinal: 12,
        },
        CanonBook {
            id: "1CH",
            name: "1 Chronicles",
            aliases: &["1 chron", "1chron", "1 chr", "1ch"],
            ordinal: 13,
        },
        CanonBook {
            id: "2CH",
            name: "2 Chronicles",
            aliases: &["2 chron", "2chron", "2 chr", "2ch"],
            ordinal: 14,
        },
        CanonBook {
            id: "EZR",
            name: "Ezra",
            aliases: &["ezr", "ez"],
            ordinal: 15,
        },
        CanonBook {
            id: "NEH",
            name: "Nehemiah",
            aliases: &["neh", "ne"],
            ordinal: 16,
        },
        CanonBook {
            id: "EST",
            name: "Esther",
            aliases: &["est", "esth"],
            ordinal: 17,
        },
        CanonBook {
            id: "JOB",
            name: "Job",
            aliases: &["jb"],
            ordinal: 18,
        },
        CanonBook {
            id: "PSA",
            name: "Psalms",
            aliases: &["ps", "psa", "pslm", "psalm"],
            ordinal: 19,
        },
        CanonBook {
            id: "PRO",
            name: "Proverbs",
            aliases: &["prov", "prv", "pr"],
            ordinal: 20,
        },
        CanonBook {
            id: "ECC",
            name: "Ecclesiastes",
            aliases: &["eccles", "eccl", "ec", "qoh"],
            ordinal: 21,
        },
        CanonBook {
            id: "SNG",
            name: "Song of Songs",
            aliases: &["song", "sos", "sg", "canticles", "song of solomon"],
            ordinal: 22,
        },
        CanonBook {
            id: "ISA",
            name: "Isaiah",
            aliases: &["isa", "is"],
            ordinal: 23,
        },
        CanonBook {
            id: "JER",
            name: "Jeremiah",
            aliases: &["jer", "je", "jr"],
            ordinal: 24,
        },
        CanonBook {
            id: "LAM",
            name: "Lamentations",
            aliases: &["lam", "lm"],
            ordinal: 25,
        },
        CanonBook {
            id: "EZK",
            name: "Ezekiel",
            aliases: &["ezek", "eze", "ezk"],
            ordinal: 26,
        },
        CanonBook {
            id: "DAN",
            name: "Daniel",
            aliases: &["dan", "dn", "da"],
            ordinal: 27,
        },
        CanonBook {
            id: "HOS",
            name: "Hosea",
            aliases: &["hos", "ho"],
            ordinal: 28,
        },
        CanonBook {
            id: "JOL",
            name: "Joel",
            aliases: &["joel", "jl"],
            ordinal: 29,
        },
        CanonBook {
            id: "AMO",
            name: "Amos",
            aliases: &["am", "amo"],
            ordinal: 30,
        },
        CanonBook {
            id: "OBA",
            name: "Obadiah",
            aliases: &["obad", "ob"],
            ordinal: 31,
        },
        CanonBook {
            id: "JON",
            name: "Jonah",
            aliases: &["jonah", "jon", "jnh"],
            ordinal: 32,
        },
        CanonBook {
            id: "MIC",
            name: "Micah",
            aliases: &["mic", "mi"],
            ordinal: 33,
        },
        CanonBook {
            id: "NAM",
            name: "Nahum",
            aliases: &["nah", "na"],
            ordinal: 34,
        },
        CanonBook {
            id: "HAB",
            name: "Habakkuk",
            aliases: &["hab", "hk"],
            ordinal: 35,
        },
        CanonBook {
            id: "ZEP",
            name: "Zephaniah",
            aliases: &["zeph", "zep", "zp"],
            ordinal: 36,
        },
        CanonBook {
            id: "HAG",
            name: "Haggai",
            aliases: &["hag", "hg"],
            ordinal: 37,
        },
        CanonBook {
            id: "ZEC",
            name: "Zechariah",
            aliases: &["zech", "zec", "zc"],
            ordinal: 38,
        },
        CanonBook {
            id: "MAL",
            name: "Malachi",
            aliases: &["mal", "ml"],
            ordinal: 39,
        },
        CanonBook {
            id: "MAT",
            name: "Matthew",
            aliases: &["matt", "mt"],
            ordinal: 40,
        },
        CanonBook {
            id: "MRK",
            name: "Mark",
            aliases: &["mk", "mrk"],
            ordinal: 41,
        },
        CanonBook {
            id: "LUK",
            name: "Luke",
            aliases: &["lk", "luk"],
            ordinal: 42,
        },
        CanonBook {
            id: "JHN",
            name: "John",
            aliases: &["jn", "jhn"],
            ordinal: 43,
        },
        CanonBook {
            id: "ACT",
            name: "Acts",
            aliases: &["ac"],
            ordinal: 44,
        },
        CanonBook {
            id: "ROM",
            name: "Romans",
            aliases: &["rom", "ro", "rm"],
            ordinal: 45,
        },
        CanonBook {
            id: "1CO",
            name: "1 Corinthians",
            aliases: &["1 cor", "1cor", "i cor", "1co"],
            ordinal: 46,
        },
        CanonBook {
            id: "2CO",
            name: "2 Corinthians",
            aliases: &["2 cor", "2cor", "ii cor", "2co"],
            ordinal: 47,
        },
        CanonBook {
            id: "GAL",
            name: "Galatians",
            aliases: &["gal", "ga"],
            ordinal: 48,
        },
        CanonBook {
            id: "EPH",
            name: "Ephesians",
            aliases: &["eph", "ephes"],
            ordinal: 49,
        },
        CanonBook {
            id: "PHP",
            name: "Philippians",
            aliases: &["phil", "php", "pp"],
            ordinal: 50,
        },
        CanonBook {
            id: "COL",
            name: "Colossians",
            aliases: &["col", "cl"],
            ordinal: 51,
        },
        CanonBook {
            id: "1TH",
            name: "1 Thessalonians",
            aliases: &["1 thess", "1thess", "1 thes", "1th"],
            ordinal: 52,
        },
        CanonBook {
            id: "2TH",
            name: "2 Thessalonians",
            aliases: &["2 thess", "2thess", "2 thes", "2th"],
            ordinal: 53,
        },
        CanonBook {
            id: "1TI",
            name: "1 Timothy",
            aliases: &["1 tim", "1tim", "1 ti", "1ti"],
            ordinal: 54,
        },
        CanonBook {
            id: "2TI",
            name: "2 Timothy",
            aliases: &["2 tim", "2tim", "2 ti", "2ti"],
            ordinal: 55,
        },
        CanonBook {
            id: "TIT",
            name: "Titus",
            aliases: &["tit", "ti"],
            ordinal: 56,
        },
        CanonBook {
            id: "PHM",
            name: "Philemon",
            aliases: &["phlm", "philem", "phm"],
            ordinal: 57,
        },
        CanonBook {
            id: "HEB",
            name: "Hebrews",
            aliases: &["heb", "he"],
            ordinal: 58,
        },
        CanonBook {
            id: "JAS",
            name: "James",
            aliases: &["jas", "jm"],
            ordinal: 59,
        },
        CanonBook {
            id: "1PE",
            name: "1 Peter",
            aliases: &["1 pet", "1pet", "1 pe", "1pe"],
            ordinal: 60,
        },
        CanonBook {
            id: "2PE",
            name: "2 Peter",
            aliases: &["2 pet", "2pet", "2 pe", "2pe"],
            ordinal: 61,
        },
        CanonBook {
            id: "1JN",
            name: "1 John",
            aliases: &["1 john", "1john", "1 jn", "1jn"],
            ordinal: 62,
        },
        CanonBook {
            id: "2JN",
            name: "2 John",
            aliases: &["2 john", "2john", "2 jn", "2jn"],
            ordinal: 63,
        },
        CanonBook {
            id: "3JN",
            name: "3 John",
            aliases: &["3 john", "3john", "3 jn", "3jn"],
            ordinal: 64,
        },
        CanonBook {
            id: "JUD",
            name: "Jude",
            aliases: &["jd"],
            ordinal: 65,
        },
        CanonBook {
            id: "REV",
            name: "Revelation",
            aliases: &["rev", "revelation", "apocalypse", "apoc"],
            ordinal: 66,
        },
    ];

    /// The version token for this registry's data contract.
    pub const fn version() -> &'static str {
        VERSION
    }

    /// All books in canonical order.
    pub const fn books() -> &'static [CanonBook] {
        Self::BOOKS
    }

    /// Resolve a book by display name or alias (case-insensitive).
    /// Unknown input fails closed with `None`.
    pub fn resolve(input: &str) -> Option<&'static CanonBook> {
        let normalized = input.trim().to_lowercase();
        Self::BOOKS.iter().find(|book| {
            book.name.to_lowercase() == normalized
                || book.aliases.iter().any(|alias| *alias == normalized)
        })
    }

    /// Resolve a book by its stable ID (case-insensitive).
    /// Unknown IDs fail closed with `None`.
    pub fn by_id(id: &str) -> Option<&'static CanonBook> {
        let normalized = id.trim().to_lowercase();
        Self::BOOKS
            .iter()
            .find(|book| book.id.to_lowercase() == normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_66_books_in_protestant_order() {
        let books = CanonRegistry::books();
        assert_eq!(books.len(), 66);
        assert_eq!(books.first().unwrap().id, "GEN");
        assert_eq!(books.first().unwrap().name, "Genesis");
        assert_eq!(books.first().unwrap().ordinal, 1);
        assert_eq!(books.last().unwrap().id, "REV");
        assert_eq!(books.last().unwrap().name, "Revelation");
        assert_eq!(books.last().unwrap().ordinal, 66);
    }

    #[test]
    fn ordinals_are_contiguous() {
        for (i, book) in CanonRegistry::BOOKS.iter().enumerate() {
            assert_eq!(book.ordinal as usize, i + 1);
        }
    }

    #[test]
    fn lookup_by_stable_id() {
        assert_eq!(CanonRegistry::by_id("GEN").unwrap().name, "Genesis");
        assert_eq!(CanonRegistry::by_id("JHN").unwrap().ordinal, 43);
        assert_eq!(CanonRegistry::by_id("1CO").unwrap().name, "1 Corinthians");
        assert_eq!(CanonRegistry::by_id("REV").unwrap().ordinal, 66);
    }

    #[test]
    fn lookup_by_alias() {
        assert_eq!(CanonRegistry::resolve("1 cor").unwrap().ordinal, 46);
        assert_eq!(CanonRegistry::resolve("John").unwrap().ordinal, 43);
        assert_eq!(CanonRegistry::resolve("jn").unwrap().ordinal, 43);
        assert_eq!(
            CanonRegistry::resolve("song of solomon").unwrap().ordinal,
            22
        );
    }

    #[test]
    fn unknown_id_fails_closed() {
        assert!(CanonRegistry::by_id("XYZ").is_none());
        assert!(CanonRegistry::by_id("").is_none());
    }

    #[test]
    fn unknown_name_fails_closed() {
        assert!(CanonRegistry::resolve("Apocrypha").is_none());
        assert!(CanonRegistry::resolve("gospel of thomas").is_none());
    }

    #[test]
    fn version_token_present_and_stable() {
        assert_eq!(CanonRegistry::version(), "protestant-66/v1");
        assert_eq!(VERSION, "protestant-66/v1");
        assert!(!VERSION.is_empty());
    }
}
