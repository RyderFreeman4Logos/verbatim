#[cfg(feature = "parser-anydoc-pdf")]
pub mod anydoc;
#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
mod bounded_pdf_images;
pub mod canonical_jsonl;
pub mod canonical_package;
mod canonical_package_conversion;
pub mod epub_inspect;
mod epub_xml;
pub mod json;
pub mod markdown;
#[cfg(feature = "parser-pdf-oxide")]
pub mod oxide;
pub mod plaintext;
#[cfg(feature = "parser-pdfplumber")]
pub mod plumber;
pub(crate) mod text_segments;
pub mod usfm;

use std::path::Path;

use anyhow::{bail, Result};

use crate::traits::Parser;

pub fn select_parser(name: &str) -> Result<Box<dyn Parser>> {
    match name {
        "canonical_jsonl" => Ok(Box::new(canonical_jsonl::CanonicalJsonlParser)),
        "usfm" => Ok(Box::new(usfm::UsfmParser)),
        #[cfg(feature = "parser-pdf-oxide")]
        "pdf_oxide" => Ok(Box::new(oxide::PdfOxideParser)),
        #[cfg(feature = "parser-pdfplumber")]
        "pdfplumber" => Ok(Box::new(plumber::PdfPlumberParser)),
        #[cfg(feature = "parser-anydoc-pdf")]
        "anydoc_pdf" => Ok(Box::new(anydoc::AnyDocPdfParser)),
        "json" => Ok(Box::new(json::JsonParser)),
        "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "plaintext" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!(
            "unknown parser: {name}. Available: canonical_jsonl, usfm, pdf_oxide, pdfplumber, anydoc_pdf, json, markdown, plaintext"
        ),
    }
}

pub fn parser_for_extension(path: &Path) -> Result<Box<dyn Parser>> {
    if path.is_dir() {
        return Ok(Box::new(canonical_package::CanonicalPackageParser));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => {
            #[cfg(feature = "parser-anydoc-pdf")]
            return Ok(Box::new(anydoc::AnyDocPdfParser));
            #[cfg(all(feature = "parser-pdf-oxide", not(feature = "parser-anydoc-pdf")))]
            return Ok(Box::new(oxide::PdfOxideParser));
            #[cfg(not(any(feature = "parser-pdf-oxide", feature = "parser-anydoc-pdf")))]
            bail!("no PDF parser available (enable parser-pdf-oxide or parser-anydoc-pdf feature)")
        }
        "jsonl" => Ok(Box::new(canonical_jsonl::CanonicalJsonlParser)),
        "usfm" => Ok(Box::new(usfm::UsfmParser)),
        "json" => Ok(Box::new(json::JsonParser)),
        "md" | "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "txt" | "text" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!("unsupported file extension: .{ext}"),
    }
}
