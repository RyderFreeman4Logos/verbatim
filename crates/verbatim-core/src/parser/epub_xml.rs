use std::collections::{BTreeMap, BTreeSet};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub(crate) struct XmlElement {
    pub(crate) name: String,
    pub(crate) namespace: String,
    pub(crate) parent: Option<String>,
    pub(crate) attributes: BTreeMap<String, String>,
}

pub(crate) struct XmlDocument {
    pub(crate) elements: Vec<XmlElement>,
    pub(crate) stats: XmlStats,
}

#[derive(Default)]
pub(crate) struct XmlStats {
    pub(crate) namespaces: BTreeMap<String, BTreeSet<String>>,
    pub(crate) semantic_attributes: BTreeMap<String, BTreeMap<String, usize>>,
    pub(crate) id_distribution: BTreeMap<String, usize>,
    pub(crate) class_distribution: BTreeMap<String, usize>,
}

pub(crate) fn parse_xml(bytes: &[u8]) -> Result<XmlDocument, String> {
    std::str::from_utf8(bytes).map_err(|_| "XML contains invalid UTF-8".to_owned())?;
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut scopes = vec![BTreeMap::from([(
        String::from("xml"),
        String::from("http://www.w3.org/XML/1998/namespace"),
    )])];
    let mut stack = Vec::new();
    let mut elements = Vec::new();
    let mut stats = XmlStats::default();
    let mut root_count = 0;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                let parent_scope = scopes.last().expect("root scope");
                let parent = stack.last().cloned();
                let (mut element, scope) = make_element(&start, parent_scope, reader.decoder())?;
                element.parent = parent;
                if stack.is_empty() {
                    root_count += 1;
                }
                stats.record(&element);
                stack.push(element.name.clone());
                scopes.push(scope);
                elements.push(element);
            }
            Ok(Event::Empty(empty)) => {
                let parent_scope = scopes.last().expect("root scope");
                let parent = stack.last().cloned();
                let (mut element, _) = make_element(&empty, parent_scope, reader.decoder())?;
                element.parent = parent;
                if stack.is_empty() {
                    root_count += 1;
                }
                stats.record(&element);
                elements.push(element);
            }
            Ok(Event::End(end)) => {
                let expected = stack
                    .pop()
                    .ok_or_else(|| "unexpected XML end element".to_owned())?;
                let actual = xml_name(end.name().as_ref())?;
                if expected != actual {
                    return Err(format!(
                        "XML end element {actual} does not match {expected}"
                    ));
                }
                scopes.pop();
            }
            Ok(Event::Text(text)) => {
                let text_bytes: &[u8] = text.as_ref();
                if stack.is_empty() && !text_bytes.iter().all(u8::is_ascii_whitespace) {
                    return Err("XML has non-whitespace text outside its root".to_owned());
                }
            }
            Ok(Event::CData(text)) => {
                let text_bytes: &[u8] = text.as_ref();
                if stack.is_empty() && !text_bytes.iter().all(u8::is_ascii_whitespace) {
                    return Err("XML has CDATA outside its root".to_owned());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("malformed XML: {error}")),
        }
        buffer.clear();
    }
    if root_count != 1 || !stack.is_empty() {
        return Err(format!(
            "XML must contain exactly one complete root, found {root_count}"
        ));
    }
    Ok(XmlDocument { elements, stats })
}

fn make_element(
    start: &BytesStart<'_>,
    parent_scope: &BTreeMap<String, String>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<(XmlElement, BTreeMap<String, String>), String> {
    let name = xml_name(start.name().as_ref())?;
    let mut scope = parent_scope.clone();
    let mut attributes = BTreeMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| format!("malformed XML attribute: {error}"))?;
        let key = xml_name(attribute.key.as_ref())?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| format!("malformed XML attribute value: {error}"))?
            .into_owned();
        if key == "xmlns" {
            scope.insert(String::new(), value.clone());
        } else if let Some(prefix) = key.strip_prefix("xmlns:") {
            scope.insert(prefix.to_owned(), value.clone());
        }
        attributes.insert(key, value);
    }
    let prefix = name.split_once(':').map(|(prefix, _)| prefix).unwrap_or("");
    let namespace = scope.get(prefix).cloned().unwrap_or_default();
    Ok((
        XmlElement {
            name,
            namespace,
            parent: None,
            attributes,
        },
        scope,
    ))
}

impl XmlStats {
    fn record(&mut self, element: &XmlElement) {
        for (key, value) in &element.attributes {
            if key == "xmlns" {
                self.namespaces
                    .entry(String::new())
                    .or_default()
                    .insert(value.clone());
            } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                self.namespaces
                    .entry(prefix.to_owned())
                    .or_default()
                    .insert(value.clone());
            } else if key == "id" || key == "xml:id" {
                *self.id_distribution.entry(value.clone()).or_insert(0) += 1;
            } else if key == "class" {
                for token in value.split_whitespace().filter(|token| !token.is_empty()) {
                    *self.class_distribution.entry(token.to_owned()).or_insert(0) += 1;
                }
            }
            if matches!(key.as_str(), "epub:type" | "role" | "epub:role" | "rel") {
                for token in value.split_whitespace().filter(|token| !token.is_empty()) {
                    *self
                        .semantic_attributes
                        .entry(key.clone())
                        .or_default()
                        .entry(token.to_owned())
                        .or_insert(0) += 1;
                }
            }
        }
    }
}

fn xml_name(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| "XML contains a non-UTF-8 name".to_owned())
}
