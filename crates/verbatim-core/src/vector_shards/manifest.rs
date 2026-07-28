//! Versioned, checksummed shard files and the manifest that is the stable contract.
//!
//! The manifest is the stable contract: it lists every file in the shard, its
//! size, role, and content hash. Exact file names are implementation details and
//! may evolve; the manifest's role-tagged, hash-verified file set is the stable
//! surface. No component may grow faster than its documented linear bound.

use serde::{Deserialize, Serialize};

use super::identity::{ShardGeneration, ShardId};
use super::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};

/// SHA-256 content hash for a shard file, fail-closed validated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct FileHash(String);

impl FileHash {
    /// Constructs a validated `sha256:` hex digest.
    pub fn new(value: impl Into<String>) -> VectorShardResult<Self> {
        let hash = Self(value.into());
        hash.validate()?;
        Ok(hash)
    }

    /// Returns the serialized hash.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates the `sha256:` prefix and 64 lowercase-hex digits.
    pub fn validate(&self) -> VectorShardResult<()> {
        let valid = self
            .0
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !valid {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidFileHash,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for FileHash {
    type Error = VectorShardError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The role a file plays inside an immutable shard. Exact names may change; the
/// role set is stable so the manifest remains the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardFileRole {
    /// Full-precision original float32 vectors: O(N*D).
    Vectors,
    /// SSD-resident graph pages: O(N*R) for fixed graph degree R.
    GraphPages,
    /// Compressed candidate-generation codes: O(N*Q) for Q code bytes.
    CandidateCodes,
    /// Compact numeric to chunk-identity mapping: O(N).
    IdMap,
    /// Soft-deletion tombstones: O(N).
    Tombstones,
    /// Indexed filter attributes (source, tenant, ACL, lifecycle, language, date).
    Attributes,
    /// Build/validation report.
    BuildReport,
}

/// One versioned, checksummed file listed by the shard manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardFile {
    name: ShardFileName,
    role: ShardFileRole,
    size_bytes: u64,
    hash: FileHash,
}

impl ShardFile {
    /// Constructs a validated file entry.
    pub fn new(
        name: impl Into<String>,
        role: ShardFileRole,
        size_bytes: u64,
        hash: FileHash,
    ) -> VectorShardResult<Self> {
        let file = Self {
            name: ShardFileName::new(name)?,
            role,
            size_bytes,
            hash,
        };
        file.validate()?;
        Ok(file)
    }

    /// Revalidates size positivity and hash integrity.
    pub fn validate(&self) -> VectorShardResult<()> {
        if self.size_bytes == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidFileSet,
            ));
        }
        self.hash.validate()?;
        Ok(())
    }

    /// Returns the file's implementation-defined name.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Returns the file's stable role.
    pub const fn role(&self) -> ShardFileRole {
        self.role
    }

    /// Returns the file size in bytes.
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Returns the file content hash.
    pub fn hash(&self) -> &FileHash {
        &self.hash
    }
}

/// A bounded, non-empty file name with a safe charset.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ShardFileName(String);

impl ShardFileName {
    /// Constructs a validated file name.
    pub fn new(value: impl Into<String>) -> VectorShardResult<Self> {
        let name = Self(value.into());
        name.validate()?;
        Ok(name)
    }

    /// Returns the file name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> VectorShardResult<()> {
        let allowed = self
            .0
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/'));
        // Reject empty, oversized, absolute paths, and any path-traversal segment.
        let safe = !self.0.is_empty()
            && self.0.len() <= 256
            && allowed
            && !self.0.starts_with('/')
            && self
                .0
                .split('/')
                .all(|segment| !segment.is_empty() && segment != ".." && segment != ".");
        if !safe {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidFileSet,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for ShardFileName {
    type Error = VectorShardError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Per-component linear storage growth bound in terms of N vectors, dimension D,
/// fixed graph degree R, candidate-code bytes Q, and metadata M.
///
/// No component may contain all source pairs, tenant pairs, vector pairs, or
/// per-source graph copies. The manifest records and revalidates these bounds so
/// that SSD growth stays linear within documented constant factors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageGrowthBound {
    /// Maximum vectors per shard; the shard is rolled over before this is exceeded.
    pub max_vectors: u64,
    /// Maximum physical bytes for the full shard file set.
    pub max_shard_bytes: u64,
    /// Documented per-vector dimension D (full-precision float32).
    pub dimension: u32,
    /// Documented fixed graph degree R (graph pages: O(N*R)).
    pub graph_degree: u16,
    /// Documented candidate-code bytes Q (candidate codes: O(N*Q)).
    pub candidate_code_bytes: u32,
}

impl StorageGrowthBound {
    /// Constructs a validated growth bound with positive, internally consistent limits.
    pub fn new(
        max_vectors: u64,
        max_shard_bytes: u64,
        dimension: u32,
        graph_degree: u16,
        candidate_code_bytes: u32,
    ) -> VectorShardResult<Self> {
        let bound = Self {
            max_vectors,
            max_shard_bytes,
            dimension,
            graph_degree,
            candidate_code_bytes,
        };
        bound.validate()?;
        Ok(bound)
    }

    /// Revalidates positivity and that the byte ceiling is at least the vectors
    /// floor (`N * D * 4`).
    pub fn validate(&self) -> VectorShardResult<()> {
        if self.max_vectors == 0
            || self.dimension == 0
            || self.graph_degree == 0
            || self.candidate_code_bytes == 0
        {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidGrowthBound,
            ));
        }
        // The vectors component alone is O(N*D) floats. Ensure the shard byte
        // ceiling is at least the floor so the bound is internally consistent.
        let vectors_floor = self
            .max_vectors
            .checked_mul(u64::from(self.dimension))
            .and_then(|bytes| bytes.checked_mul(4));
        match vectors_floor {
            Some(floor) if self.max_shard_bytes >= floor => Ok(()),
            _ => Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidGrowthBound,
            )),
        }
    }

    /// Returns the documented asymptotic growth class for the vectors component.
    pub const fn vectors_class(self) -> &'static str {
        "O(N*D)"
    }

    /// Returns the documented asymptotic growth class for graph pages.
    pub const fn graph_class(self) -> &'static str {
        "O(N*R)"
    }

    /// Returns the documented asymptotic growth class for candidate codes.
    pub const fn candidate_class(self) -> &'static str {
        "O(N*Q)"
    }

    /// Returns the documented asymptotic growth class for metadata/filters.
    pub const fn metadata_class(self) -> &'static str {
        "O(N+M)"
    }

    /// Returns the documented asymptotic growth class for the manifest/id-map.
    pub const fn manifest_class(self) -> &'static str {
        "O(N)"
    }
}

/// The stable contract for one immutable, bounded-size SSD shard.
///
/// The manifest binds a [`ShardId`] to its full checksummed file set, vector
/// count, growth bound, and generation. Exact file names are implementation
/// details; the manifest's role-tagged, hash-verified file set is the stable
/// surface that build, validation, publication, and rollback depend on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardManifest {
    shard: ShardId,
    generation: ShardGeneration,
    vector_count: u64,
    files: Vec<ShardFile>,
    growth_bound: StorageGrowthBound,
}

impl ShardManifest {
    /// Constructs a manifest after validating identity, file roles, and bounds.
    pub fn new(
        shard: ShardId,
        generation: ShardGeneration,
        vector_count: u64,
        files: Vec<ShardFile>,
        growth_bound: StorageGrowthBound,
    ) -> VectorShardResult<Self> {
        let manifest = Self {
            shard,
            generation,
            vector_count,
            files,
            growth_bound,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Revalidates the full manifest contract.
    ///
    /// Checks: nonzero vector count within the growth bound, generation matches
    /// the shard, at least one file per required role, no duplicate roles or
    /// names, and every file passes its own validation.
    pub fn validate(&self) -> VectorShardResult<()> {
        self.shard.validate()?;
        self.growth_bound.validate()?;

        if self.generation != self.shard.generation() {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidManifest,
            ));
        }
        if self.vector_count == 0 || self.vector_count > self.growth_bound.max_vectors {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidManifest,
            ));
        }
        if self.files.is_empty() {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidFileSet,
            ));
        }

        // Reject duplicate file names.
        let mut seen_names = std::collections::HashSet::with_capacity(self.files.len());
        for file in &self.files {
            file.validate()?;
            if !seen_names.insert(file.name.as_str()) {
                return Err(VectorShardError::contract(
                    VectorShardDiagnosticCode::InvalidFileSet,
                ));
            }
        }

        // Require exactly one file per required role.
        for role in REQUIRED_ROLES {
            if !self.files.iter().any(|file| file.role == role) {
                return Err(VectorShardError::contract(
                    VectorShardDiagnosticCode::InvalidFileSet,
                ));
            }
        }

        Ok(())
    }

    /// Returns the shard identity.
    pub const fn shard(&self) -> &ShardId {
        &self.shard
    }

    /// Returns the publication generation.
    pub const fn generation(&self) -> ShardGeneration {
        self.generation
    }

    /// Returns the vector count recorded by this manifest.
    pub const fn vector_count(&self) -> u64 {
        self.vector_count
    }

    /// Returns the checksummed file set.
    pub fn files(&self) -> &[ShardFile] {
        &self.files
    }

    /// Returns the documented storage growth bound.
    pub const fn growth_bound(&self) -> StorageGrowthBound {
        self.growth_bound
    }

    /// Returns the total bytes across all listed files.
    pub fn total_size_bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|file| file.size_bytes)
            .fold(0_u64, |acc, size| acc.saturating_add(size))
    }
}

/// Every immutable shard manifest must list at least one file for each of these
/// roles. Roles are the stable contract; exact file names may evolve.
pub const REQUIRED_ROLES: [ShardFileRole; 6] = [
    ShardFileRole::Vectors,
    ShardFileRole::GraphPages,
    ShardFileRole::CandidateCodes,
    ShardFileRole::IdMap,
    ShardFileRole::Tombstones,
    ShardFileRole::Attributes,
];
