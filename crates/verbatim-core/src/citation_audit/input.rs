//! Audited prose, untrusted existing citations, and source-offset claim records.

use serde::{Deserialize, Serialize};

use super::util::{content_hash_of, require_non_empty};
use super::{CitationAuditError, CitationAuditResult};

/// Stable opaque identifier assigned by a segmentation adapter.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimId(String);

impl ClaimId {
    pub fn new(value: impl Into<String>) -> CitationAuditResult<Self> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> CitationAuditResult<()> {
        require_non_empty("claim_id", &self.0)
    }
}

/// Citation-like markup copied from an external document. It is display/input
/// data only and never identifies validated evidence.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrustedExistingCitation {
    pub source_start: u64,
    pub source_end: u64,
    pub raw_citation: String,
}

impl UntrustedExistingCitation {
    fn validate_for_document(&self, document: &AuditDocument) -> CitationAuditResult<()> {
        require_non_empty("existing_citation.raw_citation", &self.raw_citation)?;
        let start = usize::try_from(self.source_start).map_err(|_| {
            CitationAuditError::validation("existing citation start is out of range")
        })?;
        let end = usize::try_from(self.source_end)
            .map_err(|_| CitationAuditError::validation("existing citation end is out of range"))?;
        if start > end || end > document.prose.len() {
            return Err(CitationAuditError::validation(
                "existing citation offsets are outside audited prose",
            ));
        }
        if !document.prose.is_char_boundary(start) || !document.prose.is_char_boundary(end) {
            return Err(CitationAuditError::validation(
                "existing citation offsets must be UTF-8 boundaries",
            ));
        }
        Ok(())
    }
}

/// The externally supplied document being audited. `prose` may be untrusted
/// and cannot alter workflow control or evidence resolution.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditDocument {
    pub document_id: String,
    pub prose: String,
    #[serde(default)]
    pub existing_citations: Vec<UntrustedExistingCitation>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuditDocumentFields {
    pub document_id: String,
    pub prose: String,
    pub existing_citations: Vec<UntrustedExistingCitation>,
}

impl AuditDocument {
    pub fn new(fields: AuditDocumentFields) -> CitationAuditResult<Self> {
        let document = Self {
            document_id: fields.document_id,
            prose: fields.prose,
            existing_citations: fields.existing_citations,
        };
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> CitationAuditResult<()> {
        require_non_empty("audit_document.document_id", &self.document_id)?;
        require_non_empty("audit_document.prose", &self.prose)?;
        for citation in &self.existing_citations {
            citation.validate_for_document(self)?;
        }
        Ok(())
    }

    pub fn content_hash(&self) -> CitationAuditResult<String> {
        self.validate()?;
        content_hash_of(self)
    }
}

/// A source-bounded factual claim selected from the audited prose.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub claim_id: ClaimId,
    pub text: String,
    pub source_start: u64,
    pub source_end: u64,
    /// Preserved for display/forensics; never accepted as evidence.
    #[serde(default)]
    pub existing_citations: Vec<UntrustedExistingCitation>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClaimRecordFields {
    pub claim_id: String,
    pub text: String,
    pub source_start: u64,
    pub source_end: u64,
    pub existing_citations: Vec<UntrustedExistingCitation>,
}

impl ClaimRecord {
    pub fn new(fields: ClaimRecordFields) -> CitationAuditResult<Self> {
        let record = Self {
            claim_id: ClaimId::new(fields.claim_id)?,
            text: fields.text,
            source_start: fields.source_start,
            source_end: fields.source_end,
            existing_citations: fields.existing_citations,
        };
        record.validate_shape()?;
        Ok(record)
    }

    pub fn validate_shape(&self) -> CitationAuditResult<()> {
        self.claim_id.validate()?;
        require_non_empty("claim_record.text", &self.text)?;
        if self.source_start >= self.source_end {
            return Err(CitationAuditError::validation(
                "claim source offsets must be a non-empty ordered range",
            ));
        }
        Ok(())
    }

    pub fn validate_for_document(&self, document: &AuditDocument) -> CitationAuditResult<()> {
        document.validate()?;
        self.validate_shape()?;
        let start = usize::try_from(self.source_start)
            .map_err(|_| CitationAuditError::validation("claim start is out of range"))?;
        let end = usize::try_from(self.source_end)
            .map_err(|_| CitationAuditError::validation("claim end is out of range"))?;
        if end > document.prose.len()
            || !document.prose.is_char_boundary(start)
            || !document.prose.is_char_boundary(end)
        {
            return Err(CitationAuditError::validation(
                "claim offsets must be UTF-8 boundaries within audited prose",
            ));
        }
        let Some(source_text) = document.prose.get(start..end) else {
            return Err(CitationAuditError::validation(
                "claim offsets cannot slice audited prose",
            ));
        };
        if source_text != self.text {
            return Err(CitationAuditError::validation(
                "claim text must exactly match its audited source offsets",
            ));
        }
        for citation in &self.existing_citations {
            citation.validate_for_document(document)?;
        }
        Ok(())
    }
}

/// Persistable segmentation artifact bound to one exact audited document.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSegmentation {
    pub document_hash: String,
    pub claims: Vec<ClaimRecord>,
}

impl ClaimSegmentation {
    pub fn new(document: &AuditDocument, claims: Vec<ClaimRecord>) -> CitationAuditResult<Self> {
        let segmentation = Self {
            document_hash: document.content_hash()?,
            claims,
        };
        segmentation.validate_for_document(document)?;
        Ok(segmentation)
    }

    pub fn validate_for_document(&self, document: &AuditDocument) -> CitationAuditResult<()> {
        if self.document_hash != document.content_hash()? {
            return Err(CitationAuditError::validation(
                "claim segmentation must be bound to the exact audited document hash",
            ));
        }
        if self.claims.is_empty() {
            return Err(CitationAuditError::validation(
                "claim segmentation requires at least one claim",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for claim in &self.claims {
            claim.validate_for_document(document)?;
            if !ids.insert(claim.claim_id.clone()) {
                return Err(CitationAuditError::validation(
                    "claim segmentation must not duplicate claim IDs",
                ));
            }
        }
        Ok(())
    }
}
