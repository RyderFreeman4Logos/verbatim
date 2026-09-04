use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceSpan {
    start: usize,
    end: usize,
}

enum SpannedNode {
    Scalar(SourceSpan),
    Array(SourceSpan, Vec<SpannedNode>),
    Object(SourceSpan, BTreeMap<String, SpannedNode>),
}

impl SpannedNode {
    fn span(&self) -> SourceSpan {
        match self {
            Self::Scalar(span) | Self::Array(span, _) | Self::Object(span, _) => *span,
        }
    }
}

pub(crate) struct SourceScan<'a> {
    source: &'a str,
    spans: HashMap<usize, SourceSpan>,
}

impl<'a> SourceScan<'a> {
    pub(crate) fn new(source: &'a str, document: &Value, path: &Path) -> Result<Self> {
        let root = SpanParser::new(source)
            .parse()
            .map_err(|offset| source_span_error(source, offset, path))?;
        let mut spans = HashMap::new();
        index_spans(document, &root, &mut spans)
            .map_err(|offset| source_span_error(source, offset, path))?;
        Ok(Self { source, spans })
    }

    pub(crate) fn span(&self, value: &Value, line: u32, path: &Path) -> Result<SourceSpan> {
        self.spans
            .get(&(value as *const Value as usize))
            .copied()
            .ok_or_else(|| source_span_error(self.source, line_offset(self.source, line), path))
    }

    pub(crate) fn node_line(&self, value: &Value, path: &Path) -> Result<u32> {
        Ok(line_at(self.source, self.span(value, 1, path)?.start))
    }

    pub(crate) fn extend(
        &self,
        span: &mut SourceSpan,
        value: &Value,
        line: u32,
        path: &Path,
    ) -> Result<()> {
        let child = self.span(value, line, path)?;
        span.end = span.end.max(child.end);
        Ok(())
    }

    pub(crate) fn line_range(&self, span: SourceSpan, path: &Path) -> Result<(u32, u32)> {
        if span.start >= span.end || span.end > self.source.len() {
            return Err(source_span_error(self.source, span.start, path));
        }
        let start = line_at(self.source, span.start);
        let end = line_at(self.source, span.end - 1);
        if start > end {
            return Err(source_span_error(self.source, span.start, path));
        }
        Ok((start, end))
    }
}

struct SpanParser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SpanParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            offset: 0,
        }
    }

    fn parse(mut self) -> std::result::Result<SpannedNode, usize> {
        self.skip_whitespace();
        let node = self.value()?;
        self.skip_whitespace();
        if self.offset == self.bytes.len() {
            Ok(node)
        } else {
            Err(self.offset)
        }
    }

    fn value(&mut self) -> std::result::Result<SpannedNode, usize> {
        self.skip_whitespace();
        let start = self.offset;
        match self.bytes.get(self.offset) {
            Some(b'{') => self.object(start),
            Some(b'[') => self.array(start),
            Some(b'"') => {
                self.string()?;
                Ok(SpannedNode::Scalar(SourceSpan {
                    start,
                    end: self.offset,
                }))
            }
            Some(b'-' | b'0'..=b'9') => self.number(start),
            Some(b't') => self.literal(start, "true"),
            Some(b'f') => self.literal(start, "false"),
            Some(b'n') => self.literal(start, "null"),
            _ => Err(self.offset),
        }
    }

    fn object(&mut self, start: usize) -> std::result::Result<SpannedNode, usize> {
        self.offset += 1;
        self.skip_whitespace();
        let mut fields = BTreeMap::new();
        if self.consume_byte(b'}') {
            return Ok(SpannedNode::Object(
                SourceSpan {
                    start,
                    end: self.offset,
                },
                fields,
            ));
        }
        loop {
            let (key_start, key_end) = self.string()?;
            let key =
                serde_json::from_str(&self.source[key_start..key_end]).map_err(|_| key_start)?;
            self.skip_whitespace();
            if !self.consume_byte(b':') {
                return Err(self.offset);
            }
            let value = self.value()?;
            fields.insert(key, value);
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Ok(SpannedNode::Object(
                    SourceSpan {
                        start,
                        end: self.offset,
                    },
                    fields,
                ));
            }
            if !self.consume_byte(b',') {
                return Err(self.offset);
            }
            self.skip_whitespace();
        }
    }

    fn array(&mut self, start: usize) -> std::result::Result<SpannedNode, usize> {
        self.offset += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume_byte(b']') {
            return Ok(SpannedNode::Array(
                SourceSpan {
                    start,
                    end: self.offset,
                },
                values,
            ));
        }
        loop {
            values.push(self.value()?);
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Ok(SpannedNode::Array(
                    SourceSpan {
                        start,
                        end: self.offset,
                    },
                    values,
                ));
            }
            if !self.consume_byte(b',') {
                return Err(self.offset);
            }
            self.skip_whitespace();
        }
    }

    fn string(&mut self) -> std::result::Result<(usize, usize), usize> {
        let start = self.offset;
        if !self.consume_byte(b'"') {
            return Err(self.offset);
        }
        while let Some(byte) = self.bytes.get(self.offset) {
            match byte {
                b'"' => {
                    self.offset += 1;
                    return Ok((start, self.offset));
                }
                b'\\' => {
                    self.offset += 2;
                    if self.offset > self.bytes.len() {
                        return Err(self.offset);
                    }
                }
                _ => self.offset += 1,
            }
        }
        Err(self.offset)
    }

    fn number(&mut self, start: usize) -> std::result::Result<SpannedNode, usize> {
        while let Some(byte) = self.bytes.get(self.offset) {
            if matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}') {
                break;
            }
            self.offset += 1;
        }
        Ok(SpannedNode::Scalar(SourceSpan {
            start,
            end: self.offset,
        }))
    }

    fn literal(&mut self, start: usize, literal: &str) -> std::result::Result<SpannedNode, usize> {
        if self.source[self.offset..].starts_with(literal) {
            self.offset += literal.len();
            Ok(SpannedNode::Scalar(SourceSpan {
                start,
                end: self.offset,
            }))
        } else {
            Err(self.offset)
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.offset += 1;
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.bytes.get(self.offset) == Some(&expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }
}

fn index_spans(
    value: &Value,
    node: &SpannedNode,
    spans: &mut HashMap<usize, SourceSpan>,
) -> std::result::Result<(), usize> {
    spans.insert(value as *const Value as usize, node.span());
    match (value, node) {
        (Value::Object(values), SpannedNode::Object(_, fields)) => {
            for (key, value) in values {
                let child = fields.get(key).ok_or(node.span().start)?;
                index_spans(value, child, spans)?;
            }
        }
        (Value::Array(values), SpannedNode::Array(_, children))
            if values.len() == children.len() =>
        {
            for (value, child) in values.iter().zip(children) {
                index_spans(value, child, spans)?;
            }
        }
        (Value::Object(_) | Value::Array(_), _) => return Err(node.span().start),
        (_, SpannedNode::Scalar(_)) => {}
        _ => return Err(node.span().start),
    }
    Ok(())
}

fn source_span_error(source: &str, offset: usize, path: &Path) -> anyhow::Error {
    super::diagnostic(
        "USJ_SOURCE_SPAN",
        "could not establish a valid JSON source span",
        line_at(source, offset),
        path,
    )
}

fn line_offset(source: &str, line: u32) -> usize {
    source
        .match_indices('\n')
        .nth(line.saturating_sub(1) as usize)
        .map_or(source.len(), |(offset, _)| offset)
}

pub(crate) fn line_at(source: &str, offset: usize) -> u32 {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
        + 1
}
