#[cfg(feature = "parser-pdf-oxide")]
pub mod oxide;
#[cfg(feature = "parser-pdfplumber")]
pub mod plumber;

use anyhow::{bail, Result};

use crate::traits::Parser;

pub fn select_parser(name: &str) -> Result<Box<dyn Parser>> {
    match name {
        #[cfg(feature = "parser-pdf-oxide")]
        "pdf_oxide" => Ok(Box::new(oxide::PdfOxideParser)),
        #[cfg(feature = "parser-pdfplumber")]
        "pdfplumber" => Ok(Box::new(plumber::PdfPlumberParser)),
        _ => bail!("unknown parser: {name}. Available: pdf_oxide, pdfplumber"),
    }
}
