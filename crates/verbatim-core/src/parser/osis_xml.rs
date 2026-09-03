use std::path::Path;

use anyhow::Result;

pub(super) fn illegal_xml_10_character(value: &str) -> Option<(usize, char)> {
    value
        .char_indices()
        .find(|(_, character)| !is_xml_10_character(*character))
}

fn is_xml_10_character(character: char) -> bool {
    matches!(
        character as u32,
        0x09 | 0x0a | 0x0d | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
    )
}

pub(super) fn reject_illegal_xml_10_chars(
    value: &str,
    bytes: &[u8],
    position: usize,
    path: &Path,
) -> Result<()> {
    if let Some((_, character)) = illegal_xml_10_character(value) {
        return Err(super::malformed_xml(
            bytes,
            position,
            path,
            format!("illegal XML 1.0 character U+{:04X}", character as u32),
        ));
    }
    Ok(())
}
