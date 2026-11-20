//! Bounded JSON/JSONC span edits. Unedited bytes are never serialized again.
use super::{ConfigFormat, Error, Value};
use std::collections::BTreeMap;

struct Node {
    start: usize,
    end: usize,
    members: BTreeMap<String, Member>,
}
struct Member {
    key: usize,
    node: Node,
}
pub(super) struct Document {
    pub(super) value: Value,
    root: Node,
}
impl Node {
    // Validate edit spans even when the parser is used only to check a manifest.
    fn validate_spans(&self, limit: usize) -> Result<(), Error> {
        if self.start >= self.end || self.end > limit {
            return Err(Error::InvalidJson);
        }
        for member in self.members.values() {
            if member.key <= self.start
                || member.key >= member.node.start
                || member.node.end >= self.end
            {
                return Err(Error::InvalidJson);
            }
            member.node.validate_spans(limit)?;
        }
        Ok(())
    }
}
impl Document {
    fn validate_spans(&self, limit: usize) -> Result<(), Error> {
        if !self.value.is_object() {
            return Err(Error::UnsupportedShape);
        }
        self.root.validate_spans(limit)
    }
}
fn blank(s: &mut [u8], from: usize, to: usize) {
    for b in &mut s[from..to] {
        if *b != b'\r' && *b != b'\n' {
            *b = b' ';
        }
    }
}
fn comments(bytes: &[u8], allow: bool) -> Result<Vec<u8>, Error> {
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') && allow => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') && allow => {
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                if i + 1 >= bytes.len() {
                    return Err(Error::InvalidJson);
                }
                i += 2;
                blank(&mut out, start, i);
            }
            _ => i += 1,
        }
    }
    if allow {
        let snapshot = out.clone();
        let mut i = 0;
        while i < snapshot.len() {
            if snapshot[i] == b'"' {
                i += 1;
                while i < snapshot.len() {
                    match snapshot[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            } else {
                if snapshot[i] == b','
                    && snapshot[i + 1..]
                        .iter()
                        .find(|b| !b.is_ascii_whitespace())
                        .is_some_and(|b| *b == b'}' || *b == b']')
                {
                    out[i] = b' ';
                }
                i += 1;
            }
        }
    }
    Ok(out)
}
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }
    fn string(&mut self) -> Result<String, Error> {
        let start = self.i;
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return serde_json::from_slice(&self.b[start..self.i])
                        .map_err(|_| Error::InvalidJson);
                }
                _ => self.i += 1,
            }
        }
        Err(Error::InvalidJson)
    }
    fn node(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > 64 {
            return Err(Error::TooLarge);
        }
        self.ws();
        let start = self.i;
        let mut members = BTreeMap::new();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                self.ws();
                while self.b.get(self.i) != Some(&b'}') {
                    let key = self.i;
                    let name = self.string()?;
                    self.ws();
                    self.i += 1;
                    let node = self.node(depth + 1)?;
                    if members.insert(name, Member { key, node }).is_some() {
                        return Err(Error::UnsupportedShape);
                    }
                    self.ws();
                    if self.b.get(self.i) == Some(&b',') {
                        self.i += 1;
                        self.ws();
                    } else {
                        break;
                    }
                }
                self.i += 1;
            }
            Some(b'[') => {
                self.i += 1;
                self.ws();
                while self.b.get(self.i) != Some(&b']') {
                    self.node(depth + 1)?;
                    self.ws();
                    if self.b.get(self.i) == Some(&b',') {
                        self.i += 1;
                        self.ws();
                    } else {
                        break;
                    }
                }
                self.i += 1;
            }
            Some(b'"') => {
                self.string()?;
            }
            Some(_) => {
                while self
                    .b
                    .get(self.i)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.i += 1;
                }
            }
            None => return Err(Error::InvalidJson),
        }
        Ok(Node {
            start,
            end: self.i,
            members,
        })
    }
}
pub(super) fn parse(bytes: &[u8], format: ConfigFormat) -> Result<Document, Error> {
    std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
    let clean = comments(bytes, format == ConfigFormat::Jsonc)?;
    let value: Value = serde_json::from_slice(&clean).map_err(|_| Error::InvalidJson)?;
    if !value.is_object() {
        return Err(Error::UnsupportedShape);
    }
    let root = Parser { b: &clean, i: 0 }.node(0)?;
    let document = Document { value, root };
    document.validate_spans(bytes.len())?;
    Ok(document)
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn lookup<'a>(node: &'a Node, keys: &[String]) -> Option<&'a Node> {
    let mut n = node;
    for key in keys {
        n = &n.members.get(key)?.node;
    }
    Some(n)
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn encode(v: &Value) -> Result<String, Error> {
    serde_json::to_string(v).map_err(|_| Error::InvalidJson)
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub(super) fn set(
    bytes: &[u8],
    format: ConfigFormat,
    keys: &[String],
    value: Option<&Value>,
) -> Result<Vec<u8>, Error> {
    let doc = parse(bytes, format)?;
    let mut result = bytes.to_vec();
    if let Some(node) = lookup(&doc.root, keys) {
        if let Some(v) = value {
            result.splice(node.start..node.end, encode(v)?.bytes());
        } else {
            let parent =
                lookup(&doc.root, &keys[..keys.len() - 1]).ok_or(Error::UnsupportedShape)?;
            let member = parent
                .members
                .get(keys.last().ok_or(Error::UnsupportedShape)?)
                .ok_or(Error::UnsupportedShape)?;
            let clean = comments(bytes, format == ConfigFormat::Jsonc)?;
            let comma_after = (node.end..parent.end - 1)
                .find(|i| !clean[*i].is_ascii_whitespace())
                .filter(|i| clean[*i] == b',');
            let comma_before = (parent.start + 1..member.key)
                .rev()
                .find(|i| !clean[*i].is_ascii_whitespace())
                .filter(|i| clean[*i] == b',');
            if let Some(i) = comma_after.or(comma_before) {
                result[i] = b' ';
            }
            // Keep comments even inside a removed owned value.
            let fragment = &bytes[member.key..node.end];
            let masked = comments(fragment, true)?;
            for (i, b) in fragment.iter().enumerate() {
                if masked[i] == *b && *b != b'\n' && *b != b'\r' {
                    result[member.key + i] = b' ';
                }
            }
        }
    } else if let Some(v) = value {
        let mut depth = keys.len() - 1;
        while depth > 0 && lookup(&doc.root, &keys[..depth]).is_none() {
            depth -= 1;
        }
        let parent = lookup(&doc.root, &keys[..depth]).ok_or(Error::UnsupportedShape)?;
        if !doc
            .value
            .pointer(&super::pointer(&keys[..depth]))
            .is_some_and(Value::is_object)
        {
            return Err(Error::UnsupportedShape);
        }
        let mut nested = v.clone();
        for key in keys[depth + 1..].iter().rev() {
            nested = serde_json::json!({key:nested});
        }
        let k = serde_json::to_string(&keys[depth]).map_err(|_| Error::InvalidJson)?;
        let clean = comments(bytes, format == ConfigFormat::Jsonc)?;
        let last = (parent.start + 1..parent.end - 1)
            .rev()
            .find(|i| !clean[*i].is_ascii_whitespace());
        let comma = if last.is_some() { "," } else { "" };
        // Insert after the last value, before trailing comments/closing whitespace.
        let pos = last.map_or(parent.start + 1, |i| i + 1);
        let layout = if bytes[parent.start..parent.end].contains(&b'\n') {
            let newline = if bytes.windows(2).any(|p| p == b"\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let first = parent.members.values().map(|m| m.key).min();
            let existing_indent = first.and_then(|key| {
                let line = bytes[..key]
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i + 1);
                let prefix = &bytes[line..key];
                prefix
                    .iter()
                    .all(|b| *b == b' ' || *b == b'\t')
                    .then_some(prefix)
            });
            let indent = if let Some(indent) = existing_indent {
                String::from_utf8_lossy(indent).into_owned()
            } else {
                let line = bytes[..parent.start]
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i + 1);
                let prefix: Vec<u8> = bytes[line..parent.start]
                    .iter()
                    .copied()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .collect();
                let unit = if bytes
                    .split(|b| *b == b'\n')
                    .any(|l| l.first() == Some(&b'\t'))
                {
                    "\t"
                } else {
                    "  "
                };
                format!("{}{unit}", String::from_utf8_lossy(&prefix))
            };
            format!("{newline}{indent}")
        } else if last.is_some() {
            " ".to_owned()
        } else {
            String::new()
        };
        let text = format!("{comma}{layout}{k}: {}", encode(&nested)?);
        result.splice(pos..pos, text.bytes());
    }
    parse(&result, format)?;
    Ok(result)
}
