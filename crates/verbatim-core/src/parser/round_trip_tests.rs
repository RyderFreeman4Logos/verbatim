use anyhow::{bail, Result};
use std::io::Write;
use tempfile::NamedTempFile;

use super::select_parser;
use crate::types::{EvidenceKind, EvidenceUnit, SourceLocator};

const USFM: &str = r"\id JHN
\c 3
\v 16 For God so loved the world \f + \ft footnote text\f* \fe + \ft endnote text\fe* \x + \xt cross reference text\x*
";

const USX: &str = r#"<usx version="3.0">
  <book code="JHN" style="id">John</book>
  <chapter number="3" style="c" sid="JHN 3"/>
  <para style="p">
    <verse number="16" style="v" sid="JHN 3:16"/>For God so loved the world <note caller="+" style="f"><char style="ft">footnote text</char></note><note caller="+" style="fe"><char style="ft">endnote text</char></note><note caller="+" style="x"><char style="xt">cross reference text</char></note><verse eid="JHN 3:16"/>
  </para>
  <chapter eid="JHN 3"/>
</usx>
"#;

const USJ: &str = r#"{
  "type": "USJ",
  "version": "3.1",
  "content": [
    {"type": "book", "marker": "id", "code": "JHN", "content": ["John"]},
    {"type": "chapter", "marker": "c", "number": "3", "sid": "JHN 3"},
    {"type": "para", "marker": "p", "content": [
      {"type": "verse", "marker": "v", "number": "16", "sid": "JHN 3:16", "content": [
        "For God so loved the world ",
        {"type": "note", "marker": "f", "caller": "+", "content": [{"type": "char", "marker": "ft", "content": ["footnote text"]}]},
        {"type": "note", "marker": "fe", "caller": "+", "content": [{"type": "char", "marker": "ft", "content": ["endnote text"]}]},
        {"type": "note", "marker": "x", "caller": "+", "content": [{"type": "char", "marker": "xt", "content": ["cross reference text"]}]}
      ]}
    ]}
  ]
}
"#;

#[test]
fn three_formats_round_trip_to_same_verse_and_note_relations() {
    let usfm = parse_fixture("usfm", USFM);
    let usx = parse_fixture("usx", USX);
    let usj = parse_fixture("usj", USJ);

    compare_round_trip(&[("usfm", &usfm), ("usx", &usx), ("usj", &usj)])
        .expect("USFM, USX, and USJ should be equivalent");
}

#[test]
fn round_trip_mismatch_fails_closed_with_diagnostic() {
    let usfm = parse_fixture("usfm", USFM);
    let usx = parse_fixture("usx", USX);
    let mut usj = parse_fixture("usj", USJ);
    let note = usj
        .iter_mut()
        .find(|unit| unit.kind == EvidenceKind::Footnote)
        .expect("fixture footnote");
    note.annotations
        .insert("note_type".to_string(), "not_footnote".to_string());

    let error = compare_round_trip(&[("usfm", &usfm), ("usx", &usx), ("usj", &usj)])
        .expect_err("a note_type mismatch must fail closed")
        .to_string();
    assert!(error.contains("round-trip mismatch for usj"), "{error}");
    assert!(error.contains("note_type"), "{error}");
}

fn parse_fixture(format: &str, contents: &str) -> Vec<EvidenceUnit> {
    let suffix = format!(".{format}");
    let mut file = NamedTempFile::with_suffix(&suffix).expect("fixture file");
    file.write_all(contents.as_bytes())
        .expect("fixture contents");
    file.flush().expect("fixture flush");
    select_parser(format)
        .expect("registered parser")
        .parse(file.path())
        .unwrap_or_else(|error| panic!("{format} fixture failed: {error}"))
}

fn compare_round_trip(formats: &[(&str, &[EvidenceUnit])]) -> Result<()> {
    if formats.len() != 3 {
        bail!(
            "round-trip comparison requires exactly three formats, got {}",
            formats.len()
        );
    }
    let (expected_format, expected_units) = formats[0];
    let expected = round_trip_signature(expected_format, expected_units)?;
    for (format, units) in formats.iter().skip(1) {
        let actual = round_trip_signature(format, units)?;
        if actual != expected {
            bail!(
                "round-trip mismatch for {format}: expected canonical verse and (note_type, text, derived_from) {expected:?}, got {actual:?}"
            );
        }
    }
    Ok(())
}

fn round_trip_signature(
    format: &str,
    units: &[EvidenceUnit],
) -> Result<((String, String, String), Vec<(String, String, String)>)> {
    let verses = units
        .iter()
        .filter(|unit| unit.kind == EvidenceKind::Verse)
        .collect::<Vec<_>>();
    let verse = match verses.as_slice() {
        [verse] => verse,
        _ => bail!(
            "{format} round-trip requires exactly one canonical verse, got {}",
            verses.len()
        ),
    };
    let verse_identity = canonical_identity(verse)?;
    let mut notes = Vec::new();
    for note in units
        .iter()
        .filter(|unit| unit.kind == EvidenceKind::Footnote)
    {
        let note_type = note.annotations.get("note_type").ok_or_else(|| {
            anyhow::anyhow!("{format} footnote `{}` is missing note_type", note.id.0)
        })?;
        let derived_from = note.derived_from.as_ref().ok_or_else(|| {
            anyhow::anyhow!("{format} footnote `{}` is missing derived_from", note.id.0)
        })?;
        let parent = units
            .iter()
            .find(|unit| &unit.id == derived_from)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{format} footnote `{}` has unknown derived_from `{}`",
                    note.id.0,
                    derived_from.0
                )
            })?;
        if parent.kind != EvidenceKind::Verse {
            bail!(
                "{format} footnote `{}` derives from non-verse `{}`",
                note.id.0,
                derived_from.0
            );
        }
        let parent_identity = canonical_identity(parent)?;
        if parent_identity != verse_identity {
            bail!(
                "{format} footnote `{}` derives from a different canonical verse",
                note.id.0
            );
        }
        notes.push((note_type.clone(), note.text.clone(), parent_identity.1));
    }
    notes.sort();
    Ok((verse_identity, notes))
}

fn canonical_identity(unit: &EvidenceUnit) -> Result<(String, String, String)> {
    let SourceLocator::Canonical { locator } = &unit.locator else {
        bail!("round-trip unit `{}` is not canonical", unit.id.0);
    };
    Ok((
        locator.profile_id.clone(),
        locator.normalized.clone(),
        locator.display.clone(),
    ))
}
