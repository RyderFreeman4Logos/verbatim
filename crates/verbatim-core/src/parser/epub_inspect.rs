use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::epub_xml::{parse_xml, XmlDocument, XmlElement, XmlStats};

const CONTAINER_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
const PACKAGE_NS: &str = "http://www.idpf.org/2007/opf";
const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

#[derive(Debug, Serialize)]
pub struct EpubInspectionReport {
    pub path: String,
    pub valid: bool,
    pub entry_count: usize,
    pub entries: Vec<ZipEntryReport>,
    pub container: Option<ContainerReport>,
    pub opf: Option<OpfReport>,
    pub spine: Option<SpineReport>,
    pub navigation: Option<NavigationReport>,
    pub namespaces: BTreeMap<String, BTreeSet<String>>,
    pub semantic_attributes: BTreeMap<String, BTreeMap<String, usize>>,
    pub id_distribution: BTreeMap<String, usize>,
    pub class_distribution: BTreeMap<String, usize>,
    pub representative_spine_items: Vec<SpineItemReport>,
    pub diagnostics: Vec<EpubInspectDiagnostic>,
    pub report_hash: String,
}

#[derive(Debug, Serialize)]
pub struct EpubInspectDiagnostic {
    pub code: String,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ZipEntryReport {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct ContainerReport {
    pub path: String,
    pub opf_path: String,
}

#[derive(Debug, Serialize)]
pub struct OpfReport {
    pub path: String,
    pub manifest_item_count: usize,
    pub spine_item_count: usize,
    pub navigation_item_id: String,
}

#[derive(Debug, Serialize)]
pub struct SpineReport {
    pub item_count: usize,
}

#[derive(Debug, Serialize)]
pub struct NavigationReport {
    pub path: String,
    pub toc_count: usize,
}

#[derive(Debug, Serialize)]
pub struct SpineItemReport {
    pub idref: String,
    pub href: String,
    pub media_type: String,
    pub linear: String,
    pub properties: Vec<String>,
}

#[derive(Default)]
struct Inspection {
    entries: Vec<ZipEntryReport>,
    entry_counts: BTreeMap<String, usize>,
    container: Option<ContainerReport>,
    opf: Option<OpfReport>,
    spine: Option<SpineReport>,
    navigation: Option<NavigationReport>,
    namespaces: BTreeMap<String, BTreeSet<String>>,
    semantic_attributes: BTreeMap<String, BTreeMap<String, usize>>,
    id_distribution: BTreeMap<String, usize>,
    class_distribution: BTreeMap<String, usize>,
    representative_spine_items: Vec<SpineItemReport>,
    diagnostics: Vec<EpubInspectDiagnostic>,
}

struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    properties: Vec<String>,
}

pub fn inspect_epub(path: &Path) -> EpubInspectionReport {
    let mut inspection = Inspection::default();
    let path_text = path.display().to_string();
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            inspection.diagnostic(
                "EPUB_READ_FAILED",
                path_text.clone(),
                format!("cannot read EPUB: {error}"),
            );
            return inspection.finish(path_text);
        }
    };

    let mut archive = match ZipArchive::new(Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(error) => {
            inspection.diagnostic(
                "EPUB_ZIP_INVALID",
                path_text.clone(),
                format!("cannot read ZIP archive: {error}"),
            );
            return inspection.finish(path_text);
        }
    };
    for index in 0..archive.len() {
        let file = match archive.by_index(index) {
            Ok(file) => file,
            Err(error) => {
                inspection.diagnostic(
                    "EPUB_ZIP_INVALID",
                    format!("entry[{index}]"),
                    format!("cannot read ZIP entry: {error}"),
                );
                return inspection.finish(path_text);
            }
        };
        let name = file.name().to_owned();
        inspection
            .entry_counts
            .entry(name.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        inspection.entries.push(ZipEntryReport {
            name,
            directory: file.is_dir(),
            size: file.size(),
        });
    }
    inspection.entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.directory.cmp(&right.directory))
            .then(left.size.cmp(&right.size))
    });
    let entry_counts = inspection.entry_counts.clone();

    let container_bytes = match read_unique_entry(
        &mut archive,
        &entry_counts,
        "META-INF/container.xml",
        &mut inspection,
        "EPUB_CONTAINER_MISSING",
        "EPUB_CONTAINER_AMBIGUOUS",
    ) {
        Some(bytes) => bytes,
        None => return inspection.finish(path_text),
    };
    let container_xml = match parse_xml(&container_bytes) {
        Ok(document) => document,
        Err(error) => {
            inspection.diagnostic("EPUB_CONTAINER_INVALID", "META-INF/container.xml", error);
            return inspection.finish(path_text);
        }
    };
    inspection.merge_stats(&container_xml.stats);
    let opf_path = match container_opf_path(&container_xml) {
        Ok(path) => path,
        Err((code, message)) => {
            inspection.diagnostic(code, "META-INF/container.xml", message);
            return inspection.finish(path_text);
        }
    };
    inspection.container = Some(ContainerReport {
        path: "META-INF/container.xml".to_owned(),
        opf_path: opf_path.clone(),
    });

    let opf_bytes = match read_unique_entry(
        &mut archive,
        &entry_counts,
        &opf_path,
        &mut inspection,
        "EPUB_OPF_MISSING",
        "EPUB_OPF_AMBIGUOUS",
    ) {
        Some(bytes) => bytes,
        None => return inspection.finish(path_text),
    };
    let opf_xml = match parse_xml(&opf_bytes) {
        Ok(document) => document,
        Err(error) => {
            inspection.diagnostic("EPUB_OPF_INVALID", &opf_path, error);
            return inspection.finish(path_text);
        }
    };
    inspection.merge_stats(&opf_xml.stats);
    let (manifest, spine_items) = match package_chain(&opf_xml) {
        Ok(chain) => chain,
        Err((code, message)) => {
            inspection.diagnostic(code, opf_path.clone(), message);
            return inspection.finish(path_text);
        }
    };
    let nav_items: Vec<&ManifestItem> = manifest
        .iter()
        .filter(|item| item.properties.iter().any(|property| property == "nav"))
        .collect();
    let nav_item = match nav_items.as_slice() {
        [item] => *item,
        [] => {
            inspection.diagnostic(
                "EPUB_NAV_MISSING",
                opf_path.clone(),
                "manifest has no item with properties containing nav".to_owned(),
            );
            return inspection.finish(path_text);
        }
        _ => {
            inspection.diagnostic(
                "EPUB_NAV_AMBIGUOUS",
                opf_path.clone(),
                "manifest has more than one item with properties containing nav".to_owned(),
            );
            return inspection.finish(path_text);
        }
    };
    inspection.opf = Some(OpfReport {
        path: opf_path.clone(),
        manifest_item_count: manifest.len(),
        spine_item_count: spine_items.len(),
        navigation_item_id: nav_item.id.clone(),
    });
    inspection.spine = Some(SpineReport {
        item_count: spine_items.len(),
    });
    let nav_path = match join_href(&opf_path, &nav_item.href) {
        Ok(path) => path,
        Err(message) => {
            inspection.diagnostic("EPUB_NAV_INVALID", opf_path.clone(), message);
            return inspection.finish(path_text);
        }
    };
    let nav_bytes = match read_unique_entry(
        &mut archive,
        &entry_counts,
        &nav_path,
        &mut inspection,
        "EPUB_NAV_MISSING",
        "EPUB_NAV_AMBIGUOUS",
    ) {
        Some(bytes) => bytes,
        None => return inspection.finish(path_text),
    };
    let nav_xml = match parse_xml(&nav_bytes) {
        Ok(document) => document,
        Err(error) => {
            inspection.diagnostic("EPUB_NAV_INVALID", nav_path.clone(), error);
            return inspection.finish(path_text);
        }
    };
    inspection.merge_stats(&nav_xml.stats);
    let toc_count = nav_xml
        .elements
        .iter()
        .filter(|element| {
            element.name == "nav"
                && element.namespace == XHTML_NS
                && attr_tokens(element, "epub:type")
                    .iter()
                    .any(|token| token == "toc")
        })
        .count();
    if toc_count != 1 {
        inspection.diagnostic(
            if toc_count == 0 {
                "EPUB_NAV_TOC_MISSING"
            } else {
                "EPUB_NAV_TOC_AMBIGUOUS"
            },
            nav_path.clone(),
            format!("expected exactly one toc nav, found {toc_count}"),
        );
        return inspection.finish(path_text);
    }
    inspection.navigation = Some(NavigationReport {
        path: nav_path,
        toc_count,
    });

    let mut parsed_paths = BTreeSet::new();
    let mut representatives = Vec::new();
    for itemref in spine_items {
        let manifest_item = match manifest.iter().find(|item| item.id == itemref.idref) {
            Some(item) => item,
            None => {
                inspection.diagnostic(
                    "EPUB_SPINE_ITEM_MISSING",
                    format!("{opf_path}#{}", itemref.idref),
                    "spine idref is not present in the manifest".to_owned(),
                );
                continue;
            }
        };
        let content_path = match join_href(&opf_path, &manifest_item.href) {
            Ok(path) => path,
            Err(message) => {
                inspection.diagnostic(
                    "EPUB_SPINE_CONTENT_INVALID",
                    format!("{opf_path}#{}", itemref.idref),
                    message,
                );
                continue;
            }
        };
        if !parsed_paths.insert(content_path.clone()) {
            continue;
        }
        let content_bytes = match read_unique_entry(
            &mut archive,
            &entry_counts,
            &content_path,
            &mut inspection,
            "EPUB_SPINE_CONTENT_MISSING",
            "EPUB_SPINE_CONTENT_AMBIGUOUS",
        ) {
            Some(bytes) => bytes,
            None => continue,
        };
        let content_xml = match parse_xml(&content_bytes) {
            Ok(document) => document,
            Err(error) => {
                inspection.diagnostic("EPUB_SPINE_CONTENT_INVALID", content_path, error);
                continue;
            }
        };
        inspection.merge_stats(&content_xml.stats);
        if representatives.len() < 5 {
            representatives.push(SpineItemReport {
                idref: itemref.idref,
                href: manifest_item.href.clone(),
                media_type: manifest_item.media_type.clone(),
                linear: itemref.linear,
                properties: manifest_item.properties.clone(),
            });
        }
    }
    inspection.representative_spine_items = representatives;
    inspection.finish(path_text)
}

impl Inspection {
    fn diagnostic(&mut self, code: &str, location: impl Into<String>, message: impl Into<String>) {
        self.diagnostics.push(EpubInspectDiagnostic {
            code: code.to_owned(),
            location: location.into(),
            message: message.into(),
        });
    }

    fn merge_stats(&mut self, stats: &XmlStats) {
        for (prefix, namespaces) in &stats.namespaces {
            self.namespaces
                .entry(prefix.clone())
                .or_default()
                .extend(namespaces.iter().cloned());
        }
        for (name, values) in &stats.semantic_attributes {
            for (value, count) in values {
                *self
                    .semantic_attributes
                    .entry(name.clone())
                    .or_default()
                    .entry(value.clone())
                    .or_insert(0) += count;
            }
        }
        merge_counts(&mut self.id_distribution, &stats.id_distribution);
        merge_counts(&mut self.class_distribution, &stats.class_distribution);
    }

    fn finish(self, path: String) -> EpubInspectionReport {
        let mut report = EpubInspectionReport {
            path,
            valid: self.diagnostics.is_empty()
                && self.container.is_some()
                && self.opf.is_some()
                && self.spine.is_some()
                && self.navigation.is_some(),
            entry_count: self.entries.len(),
            entries: self.entries,
            container: self.container,
            opf: self.opf,
            spine: self.spine,
            navigation: self.navigation,
            namespaces: self.namespaces,
            semantic_attributes: self.semantic_attributes,
            id_distribution: self.id_distribution,
            class_distribution: self.class_distribution,
            representative_spine_items: self.representative_spine_items,
            diagnostics: self.diagnostics,
            report_hash: String::new(),
        };
        let serialized = serde_json::to_vec(&report).expect("inspection report is serializable");
        report.report_hash = hex_digest(&serialized);
        report
    }
}

fn read_unique_entry(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    entry_counts: &BTreeMap<String, usize>,
    name: &str,
    inspection: &mut Inspection,
    missing_code: &str,
    ambiguous_code: &str,
) -> Option<Vec<u8>> {
    match entry_counts.get(name).copied() {
        None => {
            inspection.diagnostic(missing_code, name, "required EPUB entry is missing");
            None
        }
        Some(count) if count != 1 => {
            inspection.diagnostic(
                ambiguous_code,
                name,
                format!("required EPUB entry occurs {count} times"),
            );
            None
        }
        Some(_) => {
            let mut file = match archive.by_name(name) {
                Ok(file) => file,
                Err(error) => {
                    inspection.diagnostic(
                        "EPUB_ZIP_INVALID",
                        name,
                        format!("cannot read required entry: {error}"),
                    );
                    return None;
                }
            };
            let mut bytes = Vec::new();
            if let Err(error) = file.read_to_end(&mut bytes) {
                inspection.diagnostic(
                    "EPUB_ZIP_INVALID",
                    name,
                    format!("cannot read required entry: {error}"),
                );
                return None;
            }
            Some(bytes)
        }
    }
}

fn container_opf_path(document: &XmlDocument) -> Result<String, (&'static str, String)> {
    let roots: Vec<&XmlElement> = document
        .elements
        .iter()
        .filter(|element| element.name == "container")
        .collect();
    let root = match roots.as_slice() {
        [root] if root.parent.is_none() && root.namespace == CONTAINER_NS => *root,
        [] => {
            return Err((
                "EPUB_CONTAINER_INVALID",
                "container root is missing".to_owned(),
            ))
        }
        [_] => {
            return Err((
                "EPUB_CONTAINER_INVALID",
                "container root namespace is invalid".to_owned(),
            ))
        }
        _ => {
            return Err((
                "EPUB_CONTAINER_AMBIGUOUS",
                "container has multiple roots".to_owned(),
            ))
        }
    };
    let rootfiles: Vec<&XmlElement> = document
        .elements
        .iter()
        .filter(|element| {
            element.name == "rootfiles" && element.parent.as_deref() == Some("container")
        })
        .collect();
    if rootfiles.len() != 1 {
        return Err((
            "EPUB_CONTAINER_INVALID",
            format!("expected one rootfiles element, found {}", rootfiles.len()),
        ));
    }
    let rootfile_count = document
        .elements
        .iter()
        .filter(|element| {
            element.name == "rootfile" && element.parent.as_deref() == Some("rootfiles")
        })
        .count();
    if rootfile_count != 1 {
        return Err((
            "EPUB_OPF_AMBIGUOUS",
            format!("expected one rootfile element, found {rootfile_count}"),
        ));
    }
    let rootfile = document
        .elements
        .iter()
        .find(|element| {
            element.name == "rootfile" && element.parent.as_deref() == Some("rootfiles")
        })
        .expect("rootfile count checked");
    if rootfile.namespace != root.namespace {
        return Err((
            "EPUB_CONTAINER_INVALID",
            "rootfile namespace is invalid".to_owned(),
        ));
    }
    match rootfile.attributes.get("full-path") {
        Some(path) if !path.is_empty() && !path.starts_with('/') && !path.contains("..") => {
            Ok(path.to_owned())
        }
        Some(_) => Err((
            "EPUB_CONTAINER_INVALID",
            "rootfile full-path is not a safe relative path".to_owned(),
        )),
        None => Err((
            "EPUB_CONTAINER_INVALID",
            "rootfile full-path is missing".to_owned(),
        )),
    }
}

type PackageChainResult = Result<(Vec<ManifestItem>, Vec<SpineItemRef>), (&'static str, String)>;

fn package_chain(document: &XmlDocument) -> PackageChainResult {
    let packages: Vec<&XmlElement> = document
        .elements
        .iter()
        .filter(|element| element.name == "package")
        .collect();
    if packages.len() != 1 {
        return Err((
            "EPUB_OPF_INVALID",
            format!("expected one package root, found {}", packages.len()),
        ));
    }
    if packages[0].parent.is_some() || packages[0].namespace != PACKAGE_NS {
        return Err((
            "EPUB_OPF_INVALID",
            "package root namespace is invalid".to_owned(),
        ));
    }
    let manifest_count = document
        .elements
        .iter()
        .filter(|element| {
            element.name == "manifest" && element.parent.as_deref() == Some("package")
        })
        .count();
    let spine_count = document
        .elements
        .iter()
        .filter(|element| element.name == "spine" && element.parent.as_deref() == Some("package"))
        .count();
    if manifest_count != 1 || spine_count != 1 {
        return Err((
            "EPUB_OPF_INVALID",
            format!(
                "expected one manifest and one spine, found {manifest_count} and {spine_count}"
            ),
        ));
    }
    let mut manifest = Vec::new();
    for element in document
        .elements
        .iter()
        .filter(|element| element.name == "item" && element.parent.as_deref() == Some("manifest"))
    {
        let id = required_attr(element, "id", "manifest item id")?;
        let href = required_attr(element, "href", "manifest item href")?;
        let media_type = required_attr(element, "media-type", "manifest item media-type")?;
        let properties = attr_tokens(element, "properties");
        if manifest.iter().any(|item: &ManifestItem| item.id == id) {
            return Err((
                "EPUB_OPF_AMBIGUOUS",
                format!("manifest item id occurs more than once: {id}"),
            ));
        }
        manifest.push(ManifestItem {
            id,
            href,
            media_type,
            properties,
        });
    }
    if manifest.is_empty() {
        return Err(("EPUB_OPF_INVALID", "manifest has no items".to_owned()));
    }
    let spine_items: Vec<SpineItemRef> = document
        .elements
        .iter()
        .filter(|element| element.name == "itemref" && element.parent.as_deref() == Some("spine"))
        .map(|element| {
            Ok(SpineItemRef {
                idref: required_attr(element, "idref", "spine itemref idref")?,
                linear: element
                    .attributes
                    .get("linear")
                    .cloned()
                    .unwrap_or_else(|| "yes".to_owned()),
            })
        })
        .collect::<Result<_, (&'static str, String)>>()?;
    if spine_items.is_empty() {
        return Err((
            "EPUB_SPINE_EMPTY",
            "spine has no itemref elements".to_owned(),
        ));
    }
    Ok((manifest, spine_items))
}

struct SpineItemRef {
    idref: String,
    linear: String,
}

fn required_attr(
    element: &XmlElement,
    name: &'static str,
    label: &str,
) -> Result<String, (&'static str, String)> {
    match element.attributes.get(name) {
        Some(value) if !value.is_empty() => Ok(value.clone()),
        _ => Err((
            "EPUB_OPF_INVALID",
            format!("{label} attribute is missing or empty"),
        )),
    }
}

fn attr_tokens(element: &XmlElement, name: &str) -> Vec<String> {
    element
        .attributes
        .get(name)
        .map(|value| value.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn join_href(base: &str, href: &str) -> Result<String, String> {
    let href = href.split('#').next().unwrap_or("");
    if href.is_empty() || href.starts_with('/') || href.contains('\0') {
        return Err("manifest href is not a safe relative path".to_owned());
    }
    let base_dir = base
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let mut parts = Vec::new();
    for part in base_dir.split('/').chain(href.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("manifest href escapes the EPUB root".to_owned());
                }
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("manifest href is empty".to_owned());
    }
    Ok(parts.join("/"))
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (key, count) in source {
        *target.entry(key.clone()).or_insert(0) += count;
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_parser_rejects_invalid_utf8() {
        assert!(parse_xml(b"<root>\xff</root>").is_err());
    }

    #[test]
    fn href_joining_rejects_escape() {
        assert!(join_href("OEBPS/content.opf", "../../outside.xhtml").is_err());
    }
}
