use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use quick_xml::events::BytesStart;
use quick_xml::XmlVersion;

pub(super) fn attributes(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    bytes: &[u8],
    position: usize,
    path: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| malformed_xml(bytes, position, path, error))?;
        let key = xml_name(attribute.key.as_ref(), bytes, position, path)?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| malformed_xml(bytes, position, path, error))?
            .into_owned();
        reject_illegal_xml_10_chars(&value, bytes, position, path)?;
        result.insert(key, value);
    }
    Ok(result)
}

pub(super) fn ensure_attributes(
    attrs: &BTreeMap<String, String>,
    allowed: &[&str],
    line: u32,
    path: &Path,
) -> Result<()> {
    if let Some(key) = attrs
        .keys()
        .find(|key| !key.starts_with("xmlns") && !allowed.contains(&key.as_str()))
    {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: unsupported attribute `{key}` on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn require_attr<'a>(
    attrs: &'a BTreeMap<String, String>,
    name: &str,
    line: u32,
    path: &Path,
) -> Result<&'a str> {
    attrs
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "USX_MISSING_ATTRIBUTE: missing `{name}` on line {line} of {}",
                path.display()
            )
        })
}

pub(super) fn positive_number(value: &str, label: &str, line: u32, path: &Path) -> Result<u16> {
    let number = value.parse::<u16>().map_err(|_| {
        anyhow!(
            "USX_INVALID_COORDINATE: invalid {label} `{value}` on line {line} of {}",
            path.display()
        )
    })?;
    if number == 0 {
        bail!(
            "USX_INVALID_COORDINATE: {label} must be positive on line {line} of {}",
            path.display()
        );
    }
    Ok(number)
}

pub(super) fn require_name(name: &str, expected: &str, line: u32, path: &Path) -> Result<()> {
    if name != expected {
        bail!(
            "USX_UNEXPECTED_STRUCTURE: expected `{expected}`, found `{name}` on line {line} of {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn require_para_attributes(
    attrs: &BTreeMap<String, String>,
    line: u32,
    path: &Path,
) -> Result<()> {
    ensure_attributes(attrs, &["style", "vid"], line, path)?;
    require_attr(attrs, "style", line, path)?;
    Ok(())
}

pub(super) fn require_char_attributes(
    attrs: &BTreeMap<String, String>,
    line: u32,
    path: &Path,
) -> Result<()> {
    ensure_attributes(attrs, &["style"], line, path)?;
    require_attr(attrs, "style", line, path)?;
    Ok(())
}

pub(super) fn validate_declaration(
    declaration: &quick_xml::events::BytesDecl<'_>,
    bytes: &[u8],
    position: usize,
    line: u32,
    path: &Path,
) -> Result<()> {
    let version = declaration
        .xml_version()
        .map_err(|error| malformed_xml(bytes, position, path, error))?;
    if version != XmlVersion::Explicit1_0 {
        bail!(
            "USX_UNSUPPORTED_XML_DECLARATION: XML version must be 1.0 on line {line} of {}",
            path.display()
        );
    }
    let encoding = declaration
        .encoding()
        .transpose()
        .map_err(|error| malformed_xml(bytes, position, path, error))?;
    if let Some(encoding) = encoding {
        let encoding = std::str::from_utf8(encoding.as_ref())
            .map_err(|error| malformed_xml(bytes, position, path, error))?;
        if !encoding.eq_ignore_ascii_case("UTF-8") {
            bail!(
                "USX_UNSUPPORTED_XML_DECLARATION: XML encoding `{encoding}` is unsupported on line {line} of {}",
                path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn reject_declarations(bytes: &[u8], path: &Path) -> Result<()> {
    for needle in [b"<!doctype".as_slice(), b"<!entity".as_slice()] {
        if let Some(position) = bytes.windows(needle.len()).position(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        }) {
            let line = line_number(bytes, position);
            bail!(
                "USX_DOCTYPE_REJECTED: DTD and external entities are not permitted on line {line} of {}",
                path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn xml_name(name: &[u8], bytes: &[u8], position: usize, path: &Path) -> Result<String> {
    String::from_utf8(name.to_vec())
        .map_err(|_| malformed_xml(bytes, position, path, "invalid XML name"))
}

pub(super) fn illegal_xml_10_character(value: &str) -> Option<(usize, char)> {
    value.char_indices().find(|(_, character)| {
        !matches!(
            *character as u32,
            0x09 | 0x0a | 0x0d | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
        )
    })
}

pub(super) fn reject_illegal_xml_10_chars(
    value: &str,
    bytes: &[u8],
    position: usize,
    path: &Path,
) -> Result<()> {
    if let Some((_, character)) = illegal_xml_10_character(value) {
        return Err(malformed_xml(
            bytes,
            position,
            path,
            format!("illegal XML 1.0 character U+{:04X}", character as u32),
        ));
    }
    Ok(())
}

pub(super) fn malformed_xml<E: std::fmt::Display>(
    bytes: &[u8],
    position: usize,
    path: &Path,
    error: E,
) -> anyhow::Error {
    anyhow!(
        "USX_MALFORMED_XML: {error} on line {} of {}",
        line_number(bytes, position),
        path.display()
    )
}

pub(super) fn line_number(bytes: &[u8], position: usize) -> u32 {
    bytes[..position.min(bytes.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
        + 1
}
