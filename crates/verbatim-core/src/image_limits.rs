use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Resource bounds for PDF image artifact extraction and persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageArtifactLimits {
    #[serde(default = "default_max_images_per_source")]
    pub max_images_per_source: usize,
    #[serde(default = "default_max_bytes_per_image")]
    pub max_bytes_per_image: usize,
    #[serde(default = "default_max_total_bytes_per_source")]
    pub max_total_bytes_per_source: usize,
    #[serde(default = "default_max_image_width")]
    pub max_image_width: u32,
    #[serde(default = "default_max_image_height")]
    pub max_image_height: u32,
    #[serde(default = "default_max_image_pixels")]
    pub max_image_pixels: u64,
}

impl Default for ImageArtifactLimits {
    fn default() -> Self {
        Self {
            max_images_per_source: default_max_images_per_source(),
            max_bytes_per_image: default_max_bytes_per_image(),
            max_total_bytes_per_source: default_max_total_bytes_per_source(),
            max_image_width: default_max_image_width(),
            max_image_height: default_max_image_height(),
            max_image_pixels: default_max_image_pixels(),
        }
    }
}

fn default_max_images_per_source() -> usize {
    512
}

fn default_max_bytes_per_image() -> usize {
    16 * 1024 * 1024
}

fn default_max_total_bytes_per_source() -> usize {
    256 * 1024 * 1024
}

fn default_max_image_width() -> u32 {
    10_000
}

fn default_max_image_height() -> u32 {
    10_000
}

fn default_max_image_pixels() -> u64 {
    100_000_000
}

/// Pipeline stage that detected an image artifact resource limit violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageArtifactLimitStage {
    Parser,
    Prepare,
    Write,
}

impl fmt::Display for ImageArtifactLimitStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parser => f.write_str("parser"),
            Self::Prepare => f.write_str("prepare"),
            Self::Write => f.write_str("write"),
        }
    }
}

/// Structured resource-limit error for PDF image artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageArtifactLimitError {
    UnsupportedImageExtraction {
        stage: ImageArtifactLimitStage,
        backend: &'static str,
        reason: &'static str,
        page: u32,
        image_index: u32,
    },
    TooManyImages {
        stage: ImageArtifactLimitStage,
        limit: usize,
        attempted: usize,
        page: u32,
        image_index: u32,
    },
    ImageBytesExceeded {
        stage: ImageArtifactLimitStage,
        limit: usize,
        actual: usize,
        page: u32,
        image_index: u32,
    },
    TotalBytesExceeded {
        stage: ImageArtifactLimitStage,
        limit: usize,
        attempted_total: usize,
        page: u32,
        image_index: u32,
    },
    ImageDimensionsExceeded {
        stage: ImageArtifactLimitStage,
        max_width: u32,
        max_height: u32,
        max_pixels: u64,
        width: u32,
        height: u32,
        pixels: u64,
        page: u32,
        image_index: u32,
    },
}

impl fmt::Display for ImageArtifactLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedImageExtraction {
                stage,
                backend,
                reason,
                page,
                image_index,
            } => write!(
                f,
                "PDF image artifact {stage} extraction unsupported by {backend}: page {page} image {image_index}: {reason}"
            ),
            Self::TooManyImages {
                stage,
                limit,
                attempted,
                page,
                image_index,
            } => write!(
                f,
                "PDF image artifact {stage} limit exceeded: image {attempted} at page {page} index {image_index} exceeds max images per source {limit}"
            ),
            Self::ImageBytesExceeded {
                stage,
                limit,
                actual,
                page,
                image_index,
            } => write!(
                f,
                "PDF image artifact {stage} limit exceeded: page {page} image {image_index} is {actual} bytes, max bytes per image is {limit}"
            ),
            Self::TotalBytesExceeded {
                stage,
                limit,
                attempted_total,
                page,
                image_index,
            } => write!(
                f,
                "PDF image artifact {stage} limit exceeded: accepting page {page} image {image_index} would make total artifact bytes {attempted_total}, max total bytes per source is {limit}"
            ),
            Self::ImageDimensionsExceeded {
                stage,
                max_width,
                max_height,
                max_pixels,
                width,
                height,
                pixels,
                page,
                image_index,
            } => write!(
                f,
                "PDF image artifact {stage} limit exceeded: page {page} image {image_index} is {width}x{height} ({pixels} pixels), max is {max_width}x{max_height} and {max_pixels} pixels"
            ),
        }
    }
}

impl Error for ImageArtifactLimitError {}

impl ImageArtifactLimitError {
    /// Returns true when extraction failed because this image cannot be safely
    /// materialized by the bounded artifact extractor.
    pub fn is_unsupported_extraction(&self) -> bool {
        matches!(self, Self::UnsupportedImageExtraction { .. })
    }
}

/// Stateful budget tracker for one source's image artifact work.
#[derive(Debug, Clone, Copy)]
pub struct ImageArtifactBudget {
    limits: ImageArtifactLimits,
    stage: ImageArtifactLimitStage,
    image_count: usize,
    total_bytes: usize,
}

impl ImageArtifactBudget {
    pub fn new(limits: ImageArtifactLimits, stage: ImageArtifactLimitStage) -> Self {
        Self {
            limits,
            stage,
            image_count: 0,
            total_bytes: 0,
        }
    }

    pub fn reserve_image_slot(
        &mut self,
        page: u32,
        image_index: u32,
    ) -> Result<(), ImageArtifactLimitError> {
        let attempted = self.image_count.saturating_add(1);
        if attempted > self.limits.max_images_per_source {
            return Err(ImageArtifactLimitError::TooManyImages {
                stage: self.stage,
                limit: self.limits.max_images_per_source,
                attempted,
                page,
                image_index,
            });
        }
        self.image_count = attempted;
        Ok(())
    }

    pub fn validate_dimensions(
        &self,
        page: u32,
        image_index: u32,
        width: u32,
        height: u32,
    ) -> Result<(), ImageArtifactLimitError> {
        let pixels = u64::from(width) * u64::from(height);
        if width > self.limits.max_image_width
            || height > self.limits.max_image_height
            || pixels > self.limits.max_image_pixels
        {
            return Err(ImageArtifactLimitError::ImageDimensionsExceeded {
                stage: self.stage,
                max_width: self.limits.max_image_width,
                max_height: self.limits.max_image_height,
                max_pixels: self.limits.max_image_pixels,
                width,
                height,
                pixels,
                page,
                image_index,
            });
        }
        Ok(())
    }

    pub fn accept_image_bytes(
        &mut self,
        page: u32,
        image_index: u32,
        bytes: usize,
    ) -> Result<(), ImageArtifactLimitError> {
        self.validate_image_bytes(page, image_index, bytes)?;
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        Ok(())
    }

    pub fn validate_image_bytes(
        &self,
        page: u32,
        image_index: u32,
        bytes: usize,
    ) -> Result<(), ImageArtifactLimitError> {
        if bytes > self.limits.max_bytes_per_image {
            return Err(ImageArtifactLimitError::ImageBytesExceeded {
                stage: self.stage,
                limit: self.limits.max_bytes_per_image,
                actual: bytes,
                page,
                image_index,
            });
        }
        let attempted_total = self.total_bytes.saturating_add(bytes);
        if attempted_total > self.limits.max_total_bytes_per_source {
            return Err(ImageArtifactLimitError::TotalBytesExceeded {
                stage: self.stage,
                limit: self.limits.max_total_bytes_per_source,
                attempted_total,
                page,
                image_index,
            });
        }
        Ok(())
    }
}
