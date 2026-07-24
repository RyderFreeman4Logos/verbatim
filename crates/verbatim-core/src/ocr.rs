use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::OcrConfig;
use crate::types::{
    hex_sha256, BBox, EvidenceKind, EvidenceUnit, ImageArtifact, OcrLocatorMetadata, OcrProfile,
    OcrSourceStatus, PdfPageScanSummary, PdfScanSummary, SourceId, SourceIngestDiagnostics,
    SourceLocator, SourceOcrDiagnostics,
};

const MEANINGFUL_TEXT_MIN_CHARS: usize = 16;
const CHILD_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(100);

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
    timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

impl ExternalCommandOcrProvider {
    pub fn from_config(config: &OcrConfig) -> Result<Self> {
        if config.command.trim().is_empty() {
            bail!("OCR provider external_command requires [ocr].command");
        }
        if config.timeout_seconds == 0 {
            bail!("OCR provider external_command requires [ocr].timeout_seconds > 0");
        }
        if config.max_stdout_bytes == 0 {
            bail!("OCR provider external_command requires [ocr].max_stdout_bytes > 0");
        }
        if config.max_stderr_bytes == 0 {
            bail!("OCR provider external_command requires [ocr].max_stderr_bytes > 0");
        }
        Ok(Self {
            command: config.command.clone(),
            args: config.args.clone(),
            profile: config.profile(),
            timeout: Duration::from_secs(config.timeout_seconds),
            max_stdout_bytes: config.max_stdout_bytes,
            max_stderr_bytes: config.max_stderr_bytes,
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
        let output = run_external_ocr_command(
            &self.command,
            &self.args,
            &input,
            self.timeout,
            self.max_stdout_bytes,
            self.max_stderr_bytes,
        )?;
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

#[derive(Debug)]
struct ExternalCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_external_ocr_command(
    command_name: &str,
    args: &[String],
    input: &[u8],
    timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<ExternalCommandOutput> {
    let mut command = Command::new(command_name);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_ocr_command(&mut command);

    let child = command
        .spawn()
        .with_context(|| format!("spawn OCR command: {command_name}"))?;
    let mut child = GuardedChild::new(child);

    let stdout = child
        .child
        .stdout
        .take()
        .context("open OCR command stdout")?;
    let stderr = child
        .child
        .stderr
        .take()
        .context("open OCR command stderr")?;
    let stdout_reader = spawn_limited_reader(stdout, max_stdout_bytes, "stdout");
    let stderr_reader = spawn_limited_reader(stderr, max_stderr_bytes, "stderr");

    let write_result = write_child_stdin(&mut child.child, input);
    let status_result = match write_result {
        Ok(()) => child.wait(timeout, command_name),
        Err(error) => Err(error),
    };
    if status_result.is_err() {
        child.kill_and_reap();
    }

    let stdout = join_limited_reader(stdout_reader, "stdout");
    let stderr = join_limited_reader(stderr_reader, "stderr");
    let status = status_result?;

    Ok(ExternalCommandOutput {
        status,
        stdout: stdout?,
        stderr: stderr?,
    })
}

#[cfg(unix)]
fn configure_ocr_command(command: &mut Command) {
    // Place the OCR helper in its own process group so timeout teardown can
    // signal the whole tree. Network is denied by the fail-closed ingest
    // security policy (INGEST-SEC-001 / #337); this is not a full OS sandbox.
    command.process_group(0);
    crate::ingest_security::IngestSecurityPolicy::default().apply_to_external_command(command);
}

#[cfg(not(unix))]
fn configure_ocr_command(command: &mut Command) {
    crate::ingest_security::IngestSecurityPolicy::default().apply_to_external_command(command);
}

fn write_child_stdin(child: &mut Child, input: &[u8]) -> Result<()> {
    let mut stdin = child.stdin.take().context("open OCR command stdin")?;
    stdin
        .write_all(input)
        .context("write OCR command request")?;
    Ok(())
}

fn spawn_limited_reader<R>(
    reader: R,
    max_bytes: usize,
    stream_name: &'static str,
) -> JoinHandle<Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || read_limited(reader, max_bytes, stream_name))
}

fn read_limited<R>(mut reader: R, max_bytes: usize, stream_name: &str) -> Result<Vec<u8>>
where
    R: Read,
{
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("read OCR command {stream_name}"))?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > max_bytes {
            let remaining = max_bytes.saturating_sub(output.len());
            output.extend_from_slice(&buffer[..remaining]);
            bail!("OCR command {stream_name} exceeded {max_bytes} bytes");
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

fn join_limited_reader(handle: JoinHandle<Result<Vec<u8>>>, stream_name: &str) -> Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| anyhow!("OCR command {stream_name} reader panicked"))?
}

struct GuardedChild {
    child: Child,
    process_group_id: u32,
    armed: bool,
}

impl GuardedChild {
    fn new(child: Child) -> Self {
        let process_group_id = child.id();
        Self {
            child,
            process_group_id,
            armed: true,
        }
    }

    fn wait(&mut self, timeout: Duration, command_name: &str) -> Result<ExitStatus> {
        let started_at = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().context("poll OCR command status")? {
                self.armed = false;
                return Ok(status);
            }
            if started_at.elapsed() >= timeout {
                self.kill_and_reap();
                bail!(
                    "OCR command timed out after {}s: {command_name}",
                    timeout.as_secs()
                );
            }
            thread::sleep(CHILD_WAIT_POLL_INTERVAL);
        }
    }

    fn kill_and_reap(&mut self) {
        if !self.armed {
            return;
        }
        terminate_child_process(&mut self.child, self.process_group_id);
        let _ = self.child.wait();
        self.armed = false;
    }
}

impl Drop for GuardedChild {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

#[cfg(unix)]
fn terminate_child_process(child: &mut Child, process_group_id: u32) {
    signal_process_group(process_group_id, libc::SIGTERM);
    thread::sleep(CHILD_TERMINATION_GRACE);
    signal_process_group(process_group_id, libc::SIGKILL);
    let _ = child.kill();
}

#[cfg(unix)]
fn signal_process_group(process_group_id: u32, signal: libc::c_int) {
    let Ok(group_id) = i32::try_from(process_group_id) else {
        return;
    };
    if group_id <= 0 {
        return;
    }
    // SAFETY: group_id comes from the just-spawned child pid after placing that
    // child in its own process group. Passing a negative pid_t to kill(2) sends
    // the signal to that process group; ESRCH/EPERM are non-fatal cleanup cases.
    let _ = unsafe { libc::kill(-(group_id as libc::pid_t), signal) };
}

#[cfg(not(unix))]
fn terminate_child_process(child: &mut Child, _process_group_id: u32) {
    let _ = child.kill();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn shell_config(script: &Path) -> OcrConfig {
        OcrConfig {
            enabled: true,
            command: "sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            timeout_seconds: 2,
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
            ..OcrConfig::default()
        }
    }

    #[cfg(unix)]
    fn request(pdf_path: PathBuf) -> OcrPageRequest {
        OcrPageRequest {
            source_id: SourceId("src-1".into()),
            pdf_path,
            page: 1,
            page_label: Some("1".into()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_command_ocr_provider_parses_bounded_json_response() {
        let tempdir = tempdir().unwrap();
        let script = tempdir.path().join("fixture-ocr.sh");
        fs::write(
            &script,
            "cat >/dev/null\nprintf '%s\\n' '{\"lines\":[{\"line_index\":1,\"text\":\"hello ocr\",\"confidence\":0.9}]}'\n",
        )
        .unwrap();
        let provider = ExternalCommandOcrProvider::from_config(&shell_config(&script)).unwrap();

        let output = provider
            .recognize_page(&request(tempdir.path().join("source.pdf")))
            .unwrap();

        assert_eq!(output.lines.len(), 1);
        assert_eq!(output.lines[0].text, "hello ocr");
        assert_eq!(output.lines[0].confidence, Some(0.9));
    }

    #[cfg(unix)]
    #[test]
    fn external_command_ocr_provider_rejects_oversized_stdout() {
        let tempdir = tempdir().unwrap();
        let script = tempdir.path().join("noisy-ocr.sh");
        fs::write(&script, "cat >/dev/null\nprintf '%s' '0123456789abcdef'\n").unwrap();
        let mut config = shell_config(&script);
        config.max_stdout_bytes = 8;
        let provider = ExternalCommandOcrProvider::from_config(&config).unwrap();

        let error = provider
            .recognize_page(&request(tempdir.path().join("source.pdf")))
            .unwrap_err()
            .to_string();

        assert!(error.contains("stdout exceeded 8 bytes"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn external_command_ocr_provider_times_out_and_reaps_child() {
        let tempdir = tempdir().unwrap();
        let script = tempdir.path().join("hung-ocr.sh");
        fs::write(&script, "cat >/dev/null\nsleep 5\n").unwrap();
        let mut config = shell_config(&script);
        config.timeout_seconds = 1;
        let provider = ExternalCommandOcrProvider::from_config(&config).unwrap();
        let started_at = Instant::now();

        let error = provider
            .recognize_page(&request(tempdir.path().join("source.pdf")))
            .unwrap_err()
            .to_string();

        assert!(error.contains("timed out after 1s"), "{error}");
        assert!(started_at.elapsed() < Duration::from_secs(3));
    }
}
