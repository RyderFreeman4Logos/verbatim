//! Canonical source profiles for reference-aware retrieval.
//!
//! Each profile understands a source family's native citation scheme
//! (e.g., Bible book/chapter/verse, RFC section, legal code paragraph).
//! The core data model is generic; profile-specific behavior lives in
//! submodules like [`bible`].

pub mod bible;

use crate::types::{CanonicalLocator, ReferenceComponent};

/// A parsed reference extracted from user input, before resolution.
#[derive(Debug, Clone)]
pub struct ParsedReference {
    pub profile_id: String,
    pub raw: String,
    pub start: Vec<ReferenceComponent>,
    pub end: Option<Vec<ReferenceComponent>>,
    pub display: String,
    pub confidence: ReferenceConfidence,
}

/// How confident the parser is that the input was a real reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceConfidence {
    /// Unambiguously a reference, e.g. "John 3:16".
    High,
    /// Looks like a reference but could be coincidental.
    Low,
}

/// Trait for source-family-specific reference logic.
pub trait SourceProfile: Send + Sync {
    fn id(&self) -> &str;
    /// Attempt to parse a user-provided string as a canonical reference.
    fn parse_reference(&self, input: &str) -> Option<ParsedReference>;
    /// Render a display citation from canonical locator components.
    fn render_citation(&self, locator: &CanonicalLocator) -> String;
    /// Build a normalized key from components (e.g., "john:3:16").
    fn normalize(&self, components: &[ReferenceComponent]) -> String;
}

/// Registry of known source profiles.
pub struct ProfileRegistry {
    profiles: Vec<Box<dyn SourceProfile>>,
}

impl ProfileRegistry {
    pub fn new() -> Self {
        Self {
            profiles: vec![Box::new(bible::BibleProfile::new())],
        }
    }

    /// Try to parse `input` as a canonical reference using all registered profiles.
    /// Returns the first high-confidence match.
    pub fn try_parse(&self, input: &str) -> Option<ParsedReference> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.len() > 200 {
            return None;
        }
        for profile in &self.profiles {
            if let Some(parsed) = profile.parse_reference(trimmed) {
                if parsed.confidence == ReferenceConfidence::High {
                    return Some(parsed);
                }
            }
        }
        None
    }

    /// Get a profile by ID.
    pub fn get(&self, id: &str) -> Option<&dyn SourceProfile> {
        self.profiles
            .iter()
            .find(|p| p.id() == id)
            .map(|p| p.as_ref())
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a normalized key from reference components.
/// Generic fallback: join lowercased values with ':'.
pub fn default_normalize(components: &[ReferenceComponent]) -> String {
    components
        .iter()
        .map(|c| c.value.to_lowercase().replace(' ', ""))
        .collect::<Vec<_>>()
        .join(":")
}

/// Default citation rendering: join component values with spaces,
/// using ':' between chapter and verse (common for hierarchical refs).
pub fn default_render(components: &[ReferenceComponent]) -> String {
    if components.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = components.iter().map(|c| c.value.as_str()).collect();
    // For 3+ components (book/chapter/verse), use "Book C:V" format.
    if parts.len() >= 3 {
        let book = &components[0].value;
        let mid = &components[1].value;
        let last = parts[2..].join(":");
        format!("{book} {mid}:{last}")
    } else {
        parts.join(" ")
    }
}
