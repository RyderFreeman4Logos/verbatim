use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::OcrConfig;
use crate::types::{
    hex_sha256, BBox, EvidenceKind, EvidenceUnit, ImageArtifact, OcrLocatorMetadata, OcrProfile,
    OcrSourceStatus, PdfPageScanSummary, PdfScanSummary, SourceId, SourceIngestDiagnostics,
    SourceLocator, SourceOcrDiagnostics,
};

const MEANINGFUL_TEXT_MIN_CHARS: usize = 16;

pub trait OcrProvider: Send + Sync {
    fn profile(&self) -> OcrProfile;
    fn recognize_page(&self, request: &OcrPageRequest) -> Result<OcrPageOutput>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrPageRequest {
    pub source_id: SourceId,
    pub pdf_path: PathBuf,
    pub page: u32,
    pub page_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrPageOutput {
    #[serde(default)]
    pub lines: Vec<OcrLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_index: Option<u32>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub words: Vec<OcrWord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrWord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_index: Option<u32>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ExternalCommandOcrProvider {
    command: String,
    args: Vec<String>,
    profile: OcrProfile,
}

impl ExternalCommandOcrProvider {
    pub fn from_config(config: &OcrConfig) -> Result<Self> {
        if config.command.trim().is_empty() {
            bail!("OCR provider external_command requires [ocr].command");
        }
        Ok(Self {
            command: config.command.clone(),
            args: config.args.clone(),
            profile: config.profile(),
        })
    }
}

impl OcrProvider for ExternalCommandOcrProvider {
    fn profile(&self) -> OcrProfile {
        self.profile.clone()
    }

    fn recognize_page(&self, request: &OcrPageRequest) -> Result<OcrPageOutput> {
        let payload = ExternalOcrCommandRequest {
            pdf_path: request.pdf_path.to_string_lossy().into_owned(),
            page: request.page,
            page_label: request.page_label.clone(),
            profile: self.profile.clone(),
        };
        let input = serde_json::to_vec(&payload).context("serialize OCR command request")?;
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn OCR command: {}", self.command))?;
        if let Some(stdin) = &mut child.stdin {
            stdin
                .write_all(&input)
                .context("write OCR command request")?;
        }
        let output = child.wait_with_output().context("wait for OCR command")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "OCR command failed with status {}: {}",
                output.status,
                stderr.trim()
            );
        }
        serde_json::from_slice(&output.stdout).context("parse OCR command response")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExternalOcrCommandRequest {
    pdf_path: String,
    page: u32,
    page_label: Option<String>,
    profile: OcrProfile,
}

pub fn configured_ocr_provider(config: &OcrConfig) -> Result<Option<Box<dyn OcrProvider>>> {
    if !config.enabled {
        return Ok(None);
    }
    match config.provider.as_str() {
        "external_command" => Ok(Some(Box::new(ExternalCommandOcrProvider::from_config(
            config,
        )?))),
        other => bail!("unknown OCR provider: {other}. Available: external_command"),
    }
}

pub fn pdf_scan_summary(
    evidence: &[EvidenceUnit],
    image_artifacts: &[ImageArtifact],
) -> Option<PdfScanSummary> {
    let mut pages: BTreeMap<u32, PageAccumulator> = BTreeMap::new();

    for unit in evidence {
        if unit.kind != EvidenceKind::Text {
            continue;
        }
        let page = match &unit.locator {
            SourceLocator::Pdf { page, .. } => *page,
            _ => continue,
        };
        pages.entry(page).or_default().text_char_count += meaningful_char_count(&unit.text);
    }

    for artifact in image_artifacts {
        pages.entry(artifact.page).or_default().image_count += 1;
    }

    if pages.is_empty() {
        return None;
    }

    let mut text_char_count = 0usize;
    let mut image_only_page_count = 0usize;
    let page_summaries = pages
        .into_iter()
        .map(|(page, summary)| {
            text_char_count += summary.text_char_count;
            let has_meaningful_text = summary.text_char_count >= MEANINGFUL_TEXT_MIN_CHARS;
            let image_only = summary.image_count > 0 && !has_meaningful_text;
            if image_only {
                image_only_page_count += 1;
            }
            PdfPageScanSummary {
                page,
                page_label: Some(page.to_string()),
                text_char_count: summary.text_char_count,
                text_density: summary.text_char_count as f32,
                image_count: summary.image_count,
                has_meaningful_text,
                image_only,
            }
        })
        .collect::<Vec<_>>();

    let page_count = page_summaries.len();
    Some(PdfScanSummary {
        page_count,
        text_char_count,
        text_density: text_char_count as f32 / page_count.max(1) as f32,
        image_only_page_count,
        ocr_recommended: image_only_page_count > 0,
        pages: page_summaries,
    })
}

pub fn source_ingest_diagnostics(
    path: &Path,
    evidence: &[EvidenceUnit],
    image_artifacts: &[ImageArtifact],
    current_profile: Option<&OcrProfile>,
) -> SourceIngestDiagnostics {
    let is_pdf = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    let pdf = is_pdf
        .then(|| pdf_scan_summary(evidence, image_artifacts))
        .flatten();
    let current_profile_hash = current_profile.map(OcrProfile::profile_hash);
    let evidence_profile_hashes = ocr_profile_hashes(evidence);
    let evidence_count = evidence
        .iter()
        .filter(|unit| unit.kind == EvidenceKind::Ocr)
        .count();
    let status = ocr_status(
        pdf.as_ref(),
        evidence_count,
        &evidence_profile_hashes,
        current_profile_hash.as_deref(),
    );

    SourceIngestDiagnostics {
        pdf,
        ocr: SourceOcrDiagnostics {
            enabled: current_profile.is_some(),
            status,
            current_profile: current_profile.cloned(),
            current_profile_hash,
            evidence_count,
            evidence_profile_hashes,
        },
    }
}

pub fn ocr_required_pages(scan: &PdfScanSummary) -> Vec<PdfPageScanSummary> {
    scan.pages
        .iter()
        .filter(|page| page.image_only)
        .cloned()
        .collect()
}

pub fn ocr_evidence_from_output(
    source_id: &SourceId,
    page: &PdfPageScanSummary,
    output: OcrPageOutput,
    profile: &OcrProfile,
    start_position: u32,
) -> Vec<EvidenceUnit> {
    let profile_hash = profile.profile_hash();
    output
        .lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let text = line.text.trim();
            if text.is_empty() {
                return None;
            }
            let line_index = line.line_index.unwrap_or(idx as u32 + 1);
            let text_hash = hex_sha256(text.as_bytes());
            Some(EvidenceUnit {
                id: crate::types::EvidenceId(format!(
                    "{}-ocr-p{}-l{}",
                    source_id.0, page.page, line_index
                )),
                source_id: source_id.clone(),
                kind: EvidenceKind::Ocr,
                derived_from: None,
                locator: SourceLocator::PdfOcr {
                    page: page.page,
                    page_label: page.page_label.clone(),
                    line_index,
                    word_index: None,
                    bbox: line.bbox,
                    ocr: Box::new(OcrLocatorMetadata {
                        profile: profile.clone(),
                        profile_hash: profile_hash.clone(),
                        confidence: line.confidence,
                        text_hash: text_hash.clone(),
                    }),
                },
                text: text.to_string(),
                text_hash,
                heading_path: Vec::new(),
                position: start_position + idx as u32,
            })
        })
        .collect()
}

pub fn ocr_profile_stale(
    diagnostics: &SourceIngestDiagnostics,
    current_profile: Option<&OcrProfile>,
) -> bool {
    let Some(pdf) = &diagnostics.pdf else {
        return false;
    };
    if !pdf.ocr_recommended {
        return false;
    }
    let Some(profile) = current_profile else {
        return false;
    };
    let current_hash = profile.profile_hash();
    diagnostics.ocr.evidence_count == 0
        || diagnostics
            .ocr
            .evidence_profile_hashes
            .iter()
            .any(|hash| hash != &current_hash)
}

fn ocr_status(
    pdf: Option<&PdfScanSummary>,
    evidence_count: usize,
    evidence_profile_hashes: &[String],
    current_profile_hash: Option<&str>,
) -> OcrSourceStatus {
    let Some(pdf) = pdf else {
        return OcrSourceStatus::NotRequired;
    };
    if !pdf.ocr_recommended {
        return OcrSourceStatus::NotRequired;
    }
    let Some(current_profile_hash) = current_profile_hash else {
        return if evidence_count > 0 {
            OcrSourceStatus::Applied
        } else {
            OcrSourceStatus::Disabled
        };
    };
    if evidence_count == 0 {
        return OcrSourceStatus::Recommended;
    }
    if evidence_profile_hashes
        .iter()
        .all(|hash| hash == current_profile_hash)
    {
        OcrSourceStatus::Applied
    } else {
        OcrSourceStatus::Stale
    }
}

fn ocr_profile_hashes(evidence: &[EvidenceUnit]) -> Vec<String> {
    let mut hashes = BTreeSet::new();
    for unit in evidence {
        if let SourceLocator::PdfOcr { ocr, .. } = &unit.locator {
            hashes.insert(ocr.profile_hash.clone());
        }
    }
    hashes.into_iter().collect()
}

fn meaningful_char_count(text: &str) -> usize {
    text.chars().filter(|ch| !ch.is_whitespace()).count()
}

#[derive(Debug, Default)]
struct PageAccumulator {
    text_char_count: usize,
    image_count: usize,
}
