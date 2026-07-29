//! Retriever classification shared across the fusion contract.
//!
//! Mirrors the retriever-family taxonomy used by the overfetch budget
//! (`RetrieverKind`) and the lexical completeness policy
//! (`LexicalRetrieverType`), but kept self-contained so the hybrid-fusion
//! contract does not depend on either module's internals. Only the
//! `ExhaustiveEnumeration` variant may justify a completeness claim.

use serde::{Deserialize, Serialize};

/// The family of retrievers that may contribute to a fused pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverKind {
    /// Dense vector ANN (DiskANN3). Approximate; never justifies completeness.
    DenseAnn,
    /// Lexical BM25 Top-K (Tantivy). Approximate; never justifies completeness.
    LexicalBm25,
    /// Exact phrase / identifier / reference / metadata match. Exact path,
    /// but does not by itself justify an exhaustive `all`/`only`/`none` claim.
    ExactReference,
    /// Graph expansion retriever. Approximate.
    Graph,
    /// Structured metadata / filter retriever. Exact path.
    Metadata,
    /// Exhaustive enumeration over a declared authorized scope. The only
    /// retriever family that may justify `all`/`only`/`none`/`every` claims.
    ExhaustiveEnumeration,
}

impl RetrieverKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DenseAnn => "dense_ann",
            Self::LexicalBm25 => "lexical_bm25",
            Self::ExactReference => "exact_reference",
            Self::Graph => "graph",
            Self::Metadata => "metadata",
            Self::ExhaustiveEnumeration => "exhaustive_enumeration",
        }
    }

    /// Returns `true` when this retriever is approximate (ANN or BM25 Top-K).
    pub const fn is_approximate(self) -> bool {
        matches!(self, Self::DenseAnn | Self::LexicalBm25 | Self::Graph)
    }

    /// Returns `true` when this retriever follows an exact (non-approximate) path.
    pub const fn is_exact_path(self) -> bool {
        matches!(
            self,
            Self::ExactReference | Self::Metadata | Self::ExhaustiveEnumeration
        )
    }

    /// Returns `true` when this retriever may justify a completeness claim.
    pub const fn may_justify_completeness(self) -> bool {
        matches!(self, Self::ExhaustiveEnumeration)
    }
}
