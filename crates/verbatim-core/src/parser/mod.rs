#[cfg(any(feature = "parser-pdf-oxide", feature = "parser-pdfplumber"))]
mod bounded_pdf_images;
pub mod markdown;
#[cfg(feature = "parser-pdf-oxide")]
pub mod oxide;
pub mod plaintext;
#[cfg(feature = "parser-pdfplumber")]
pub mod plumber;

use std::path::Path;

use anyhow::{bail, Result};

use crate::traits::Parser;

pub fn select_parser(name: &str) -> Result<Box<dyn Parser>> {
    match name {
        #[cfg(feature = "parser-pdf-oxide")]
        "pdf_oxide" => Ok(Box::new(oxide::PdfOxideParser)),
        #[cfg(feature = "parser-pdfplumber")]
        "pdfplumber" => Ok(Box::new(plumber::PdfPlumberParser)),
        "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "plaintext" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!("unknown parser: {name}. Available: pdf_oxide, pdfplumber, markdown, plaintext"),
    }
}

pub fn parser_for_extension(path: &Path) -> Result<Box<dyn Parser>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "pdf" => {
            #[cfg(feature = "parser-pdf-oxide")]
            return Ok(Box::new(oxide::PdfOxideParser));
            #[cfg(not(feature = "parser-pdf-oxide"))]
            bail!("no PDF parser available (enable parser-pdf-oxide feature)")
        }
        "md" | "markdown" => Ok(Box::new(markdown::MarkdownParser)),
        "txt" | "text" => Ok(Box::new(plaintext::PlaintextParser)),
        _ => bail!("unsupported file extension: .{ext}"),
    }
}
