//! Bounded shard router: small metadata, hard fan-out maximum, shared deadline.
//!
//! The router selects compatible generations and shards before vector search
//! without loading corpus-scale metadata into RAM. Large routing/filter state
//! belongs on SSD or in compressed immutable structures; only a small bounded
//! summary is held online. Fan-out has a configured hard maximum and all selected
//! shards share one deadline.

use serde::{Deserialize, Serialize};

use super::identity::{ShardGeneration, ShardId};
use super::manifest::ShardManifest;
use super::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};

/// Compact generation descriptor held in bounded router metadata.
///
/// The router keeps at most one small descriptor per published generation rather
/// than per-shard or per-source state, so online memory stays O(generations),
/// not O(shards) or O(sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenerationDescriptor {
    generation: ShardGeneration,
    shard_count: u32,
}

impl GenerationDescriptor {
    /// Constructs a descriptor with a positive shard count.
    pub fn new(generation: ShardGeneration, shard_count: u32) -> VectorShardResult<Self> {
        if shard_count == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidRouter,
            ));
        }
        Ok(Self {
            generation,
            shard_count,
        })
    }

    /// Returns the generation this descriptor summarizes.
    pub const fn generation(&self) -> ShardGeneration {
        self.generation
    }

    /// Returns the number of shards in this generation.
    pub const fn shard_count(&self) -> u32 {
        self.shard_count
    }
}

/// Configuration for the bounded shard router.
///
/// `max_fan_out` is a hard maximum on the number of shards a single query may
/// touch. `deadline_micros` is a shared wall-time deadline across all fanned-out
/// shards. Router metadata is bounded to `max_generations` descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardRouterConfig {
    max_fan_out: u32,
    deadline_micros: u64,
    max_generations: u32,
}

impl ShardRouterConfig {
    /// Constructs a validated router configuration with positive bounds.
    pub fn new(
        max_fan_out: u32,
        deadline_micros: u64,
        max_generations: u32,
    ) -> VectorShardResult<Self> {
        let config = Self {
            max_fan_out,
            deadline_micros,
            max_generations,
        };
        config.validate()?;
        Ok(config)
    }

    /// Revalidates that every bound is positive.
    pub fn validate(&self) -> VectorShardResult<()> {
        if self.max_fan_out == 0 || self.deadline_micros == 0 || self.max_generations == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidRouter,
            ));
        }
        Ok(())
    }

    /// Returns the hard maximum number of shards a query may touch.
    pub const fn max_fan_out(&self) -> u32 {
        self.max_fan_out
    }

    /// Returns the shared wall-time deadline in microseconds.
    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }

    /// Returns the maximum number of generation descriptors held online.
    pub const fn max_generations(&self) -> u32 {
        self.max_generations
    }
}

/// Bounded shard router that selects compatible shards before vector search.
///
/// The router holds a small, bounded set of generation descriptors — never
/// corpus-scale metadata. It selects shards whose manifest generation is
/// compatible with the query and whose count does not exceed the fan-out cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardRouter {
    config: ShardRouterConfig,
    generations: Vec<GenerationDescriptor>,
}

impl ShardRouter {
    /// Constructs a router after validating the descriptor count stays bounded.
    pub fn new(
        config: ShardRouterConfig,
        generations: Vec<GenerationDescriptor>,
    ) -> VectorShardResult<Self> {
        let router = Self {
            config,
            generations,
        };
        router.validate()?;
        Ok(router)
    }

    /// Revalidates the router's bounded-metadata invariant.
    pub fn validate(&self) -> VectorShardResult<()> {
        self.config.validate()?;
        if self.generations.len() as u32 > self.config.max_generations {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidRouter,
            ));
        }
        Ok(())
    }

    /// Returns the router configuration.
    pub const fn config(&self) -> ShardRouterConfig {
        self.config
    }

    /// Returns the bounded generation descriptors.
    pub fn generations(&self) -> &[GenerationDescriptor] {
        &self.generations
    }

    /// Selects the shards compatible with the requested generation, capped at
    /// `max_fan_out`. Returns an error if the selected set is empty or exceeds
    /// the hard fan-out maximum.
    pub fn select(
        &self,
        requested_generation: ShardGeneration,
        manifests: &[&ShardManifest],
    ) -> VectorShardResult<Vec<ShardId>> {
        // Verify the requested generation is one the router knows about.
        if !self
            .generations
            .iter()
            .any(|descriptor| descriptor.generation == requested_generation)
        {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidRouterSelection,
            ));
        }

        let mut selected: Vec<ShardId> = manifests
            .iter()
            .filter(|manifest| manifest.generation() == requested_generation)
            .map(|manifest| manifest.shard().clone())
            .collect();

        if selected.is_empty() {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidRouterSelection,
            ));
        }
        if selected.len() as u32 > self.config.max_fan_out {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::FanOutExceeded,
            ));
        }

        // Exact small-scope scans should use contiguous/sorted ID runs: sort by
        // ordinal so callers receive a stable, sorted selection.
        selected.sort_by_key(|shard| shard.ordinal().value());
        Ok(selected)
    }
}
