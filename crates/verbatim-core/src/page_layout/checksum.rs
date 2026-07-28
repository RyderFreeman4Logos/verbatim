//! Checksummed pages and torn-page detection for co-located SSD layouts.
//!
//! Each page carries a checksum covering its payload; a mismatch between the
//! stored and recomputed checksum indicates a torn write or corruption. The
//! digest itself is stored by reference length only on errors, so a partial
//! page payload can never leak through [`PageChecksum`]'s `Debug`/`Display`.

use sha2::{Digest, Sha256};

use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// Number of bytes retained from a SHA-256 digest as the torn-page marker.
pub const CHECKSUM_LEN: usize = 8;

/// A checksum computed over a page payload for torn-page detection.
///
/// The digest is derived from SHA-256 truncated to [`CHECKSUM_LEN`] bytes so
/// two distinct payloads collide with negligible probability while the marker
/// stays compact. The full payload is never stored on this type or on errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageChecksum([u8; CHECKSUM_LEN]);

impl PageChecksum {
    /// Computes a checksum over a non-empty page payload.
    ///
    /// Returns [`PageLayoutDiagnosticCode::EmptyChecksumPayload`] for an empty
    /// payload, since a zero-length page cannot be torn-page-protected.
    pub fn from_payload(payload: &[u8]) -> PageLayoutResult<Self> {
        if payload.is_empty() {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::EmptyChecksumPayload,
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let digest = hasher.finalize();
        let mut truncated = [0u8; CHECKSUM_LEN];
        truncated.copy_from_slice(&digest[..CHECKSUM_LEN]);
        Ok(Self(truncated))
    }

    /// Constructs a checksum from a previously stored truncated digest.
    pub fn from_stored(bytes: [u8; CHECKSUM_LEN]) -> Self {
        Self(bytes)
    }

    /// Rejects a payload whose recomputed checksum disagrees with the stored
    /// marker, indicating a torn or corrupted page.
    pub fn verify(self, payload: &[u8]) -> PageLayoutResult<()> {
        let recomputed = Self::from_payload(payload)?;
        if recomputed.0 == self.0 {
            Ok(())
        } else {
            Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::ChecksumMismatch,
            ))
        }
    }

    /// Returns the truncated digest bytes by copy.
    pub const fn bytes(self) -> [u8; CHECKSUM_LEN] {
        self.0
    }
}

/// Whether and how pages are checksummed for torn-page detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChecksumPolicy {
    /// No checksum stored; torn-page detection is unavailable.
    Disabled,
    /// Every page carries a [`PageChecksum`] covering its payload.
    Enabled,
}

impl ChecksumPolicy {
    /// Returns `true` when pages carry a checksum.
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}
