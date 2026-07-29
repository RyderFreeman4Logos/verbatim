//! Lexical retriever type classification and completeness-claim policy
//! (Refs #380).
//!
//! BM25 Top-K is an approximate relevance ranker: it cannot justify `all`,
//! `only`, `none`, or `every` claims over a scope. Exact phrase, identifier,
//! reference, metadata, and exhaustive enumeration retrievers are kept separate
//! so that completeness claims are routed to the correct retriever.

use serde::{Deserialize, Serialize};

use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};

/// Classification of lexical retriever types.
///
/// Each variant maps to a distinct retrieval path with different completeness
/// semantics. Only [`LexicalRetrieverType::ExhaustiveEnumeration`] may justify
/// a completeness claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRetrieverType {
    /// BM25 Top-K approximate relevance ranking. Never justifies completeness.
    Bm25TopK,
    /// Exact phrase / proximity match over a tokenized text field.
    ExactPhrase,
    /// Exact identifier match over a keyword/identifier field.
    Identifier,
    /// Exact reference match (citation, origin, cross-reference).
    Reference,
    /// Structured metadata / filter match (typed metadata, facets, ranges).
    Metadata,
    /// Exhaustive enumeration over a declared authorized scope. The only
    /// retriever that may justify `all`/`only`/`none`/`every` claims.
    ExhaustiveEnumeration,
}

impl LexicalRetrieverType {
    /// Returns `true` if this retriever may justify a completeness claim
    /// (`all`, `only`, `none`, `every`).
    pub const fn may_justify_completeness(self) -> bool {
        matches!(self, Self::ExhaustiveEnumeration)
    }

    /// Returns `true` if this retriever is an exact (non-approximate) path.
    pub const fn is_exact_path(self) -> bool {
        matches!(
            self,
            Self::ExactPhrase
                | Self::Identifier
                | Self::Reference
                | Self::Metadata
                | Self::ExhaustiveEnumeration
        )
    }

    /// Returns `true` if this retriever is the approximate BM25 Top-K path.
    pub const fn is_approximate(self) -> bool {
        matches!(self, Self::Bm25TopK)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bm25TopK => "bm25_top_k",
            Self::ExactPhrase => "exact_phrase",
            Self::Identifier => "identifier",
            Self::Reference => "reference",
            Self::Metadata => "metadata",
            Self::ExhaustiveEnumeration => "exhaustive_enumeration",
        }
    }

    /// Validates that a completeness claim is permitted for this retriever
    /// type. BM25 Top-K and other approximate/exact-but-incomplete retrievers
    /// may not claim `all`/`only`/`none`/`every`.
    pub fn validate_completeness_claim(self, claim: CompletenessClaim) -> LexicalEngineResult<()> {
        if claim.is_completeness_claim() && !self.may_justify_completeness() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::UnsupportedCompletenessClaim,
            ));
        }
        Ok(())
    }
}

/// The kind of completeness claim a retrieval result makes.
///
/// Only `All`, `Only`, `None`, and `Every` are completeness claims requiring
/// [`LexicalRetrieverType::ExhaustiveEnumeration`]. `TopK` and `Approximate`
/// make no completeness claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessClaim {
    /// Top-K results (no completeness claim).
    TopK,
    /// Approximate results (no completeness claim).
    Approximate,
    /// All matching documents in the scope (requires exhaustive enumeration).
    All,
    /// Only these documents match (requires exhaustive enumeration).
    Only,
    /// No documents match (requires exhaustive enumeration).
    None,
    /// Every document in the scope matches (requires exhaustive enumeration).
    Every,
}

impl CompletenessClaim {
    /// Returns `true` if this is a completeness claim requiring exhaustive
    /// enumeration.
    pub const fn is_completeness_claim(self) -> bool {
        matches!(self, Self::All | Self::Only | Self::None | Self::Every)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TopK => "top_k",
            Self::Approximate => "approximate",
            Self::All => "all",
            Self::Only => "only",
            Self::None => "none",
            Self::Every => "every",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_top_k_is_approximate() {
        assert!(LexicalRetrieverType::Bm25TopK.is_approximate());
        assert!(!LexicalRetrieverType::Bm25TopK.is_exact_path());
        assert!(!LexicalRetrieverType::Bm25TopK.may_justify_completeness());
    }

    #[test]
    fn exact_paths_are_exact() {
        assert!(LexicalRetrieverType::ExactPhrase.is_exact_path());
        assert!(LexicalRetrieverType::Identifier.is_exact_path());
        assert!(LexicalRetrieverType::Reference.is_exact_path());
        assert!(LexicalRetrieverType::Metadata.is_exact_path());
        assert!(LexicalRetrieverType::ExhaustiveEnumeration.is_exact_path());
        assert!(!LexicalRetrieverType::ExactPhrase.is_approximate());
    }

    #[test]
    fn only_exhaustive_justifies_completeness() {
        assert!(LexicalRetrieverType::ExhaustiveEnumeration.may_justify_completeness());
        assert!(!LexicalRetrieverType::Bm25TopK.may_justify_completeness());
        assert!(!LexicalRetrieverType::Identifier.may_justify_completeness());
    }

    #[test]
    fn bm25_rejects_completeness_claims() {
        for claim in [
            CompletenessClaim::All,
            CompletenessClaim::Only,
            CompletenessClaim::None,
            CompletenessClaim::Every,
        ] {
            let err = LexicalRetrieverType::Bm25TopK
                .validate_completeness_claim(claim)
                .unwrap_err();
            assert_eq!(
                err.diagnostic_code(),
                LexicalEngineDiagnosticCode::UnsupportedCompletenessClaim,
                "BM25 should reject {:?}",
                claim
            );
        }
    }

    #[test]
    fn bm25_accepts_non_completeness_claims() {
        assert!(LexicalRetrieverType::Bm25TopK
            .validate_completeness_claim(CompletenessClaim::TopK)
            .is_ok());
        assert!(LexicalRetrieverType::Bm25TopK
            .validate_completeness_claim(CompletenessClaim::Approximate)
            .is_ok());
    }

    #[test]
    fn exhaustive_accepts_all_claims() {
        for claim in [
            CompletenessClaim::All,
            CompletenessClaim::Only,
            CompletenessClaim::None,
            CompletenessClaim::Every,
            CompletenessClaim::TopK,
        ] {
            assert!(
                LexicalRetrieverType::ExhaustiveEnumeration
                    .validate_completeness_claim(claim)
                    .is_ok(),
                "ExhaustiveEnumeration should accept {:?}",
                claim
            );
        }
    }

    #[test]
    fn completeness_claim_classification() {
        assert!(CompletenessClaim::All.is_completeness_claim());
        assert!(CompletenessClaim::Only.is_completeness_claim());
        assert!(CompletenessClaim::None.is_completeness_claim());
        assert!(CompletenessClaim::Every.is_completeness_claim());
        assert!(!CompletenessClaim::TopK.is_completeness_claim());
        assert!(!CompletenessClaim::Approximate.is_completeness_claim());
    }

    #[test]
    fn retriever_type_as_str_stable() {
        assert_eq!(LexicalRetrieverType::Bm25TopK.as_str(), "bm25_top_k");
        assert_eq!(
            LexicalRetrieverType::ExhaustiveEnumeration.as_str(),
            "exhaustive_enumeration"
        );
    }
}
