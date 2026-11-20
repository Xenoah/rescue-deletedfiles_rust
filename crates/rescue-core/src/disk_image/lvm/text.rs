//! Bounded parser for the declarative LVM2 ASCII metadata format.
use super::{Error, Result, invalid};
use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) enum Value {
    Text(String),
    Number(u64),
    List(Vec<Value>),
    Section(Section),
}
pub(super) type Section = BTreeMap<String, Value>;

pub(super) trait Fields {
    fn text(&self, key: &str) -> Result<&str>;
    fn num(&self, key: &str) -> Result<u64>;
    fn list(&self, key: &str) -> Result<&[Value]>;
    fn section(&self, key: &str) -> Result<&Section>;
}
impl Fields for Section {
    fn text(&self, key: &str) -> Result<&str> {
        match self.get(key) {
            Some(Value::Text(s)) => Ok(s),
            _ => Err(invalid("required text field missing")),
        }
    }
    fn num(&self, key: &str) -> Result<u64> {
        match self.get(key) {
            Some(Value::Number(n)) => Ok(*n),
            _ => Err(invalid("required number missing")),
        }
    }
    fn list(&self, key: &str) -> Result<&[Value]> {
        match self.get(key) {
            Some(Value::List(v)) => Ok(v),
            _ => Err(invalid("required list missing")),
        }
    }
    fn section(&self, key: &str) -> Result<&Section> {
        match self.get(key) {
            Some(Value::Section(v)) => Ok(v),
            _ => Err(invalid("required section missing")),
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    entries: usize,
}
impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        loop {
            match self.bytes.get(self.pos).copied() {
                Some(b'#') => {
                    let end = self.bytes[self.pos..].iter().position(|&b| b == b'\n');
                    self.pos += end.unwrap_or(self.bytes.len() - self.pos);
                }
                Some(b) if b.is_ascii_whitespace() => self.pos += 1,
                v => return v,
            }
        }
    }
    fn word(&mut self) -> Result<&str> {
        let start = self.pos;
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.'))
        {
            self.pos += 1;
        }
        if self.pos == start || self.pos - start > 256 {
            return Err(invalid("invalid metadata identifier"));
        }
        std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| invalid("invalid identifier"))
    }
    fn scalar(&mut self) -> Result<Value> {
        match self.peek() {
            Some(b'"') => {
                self.pos += 1;
                let mut out = Vec::new();
                while let Some(&b) = self.bytes.get(self.pos) {
                    self.pos += 1;
                    match b {
                        b'"' => {
                            return String::from_utf8(out)
                                .map(Value::Text)
                                .map_err(|_| invalid("invalid metadata string"));
                        }
                        b'\\' => {
                            let next = self
                                .bytes
                                .get(self.pos)
                                .copied()
                                .filter(|c| matches!(c, b'"' | b'\\'))
                                .ok_or_else(|| invalid("invalid metadata escape"))?;
                            out.push(next);
                            self.pos += 1;
                        }
                        0..=31 => return Err(invalid("control character in metadata string")),
                        _ => out.push(b),
                    }
                    if out.len() > 65536 {
                        return Err(Error::Limit("LVM metadata string exceeds 64 KiB".into()));
                    }
                }
                Err(invalid("unterminated metadata string"))
            }
            Some(b'0'..=b'9') => self
                .word()?
                .parse()
                .map(Value::Number)
                .map_err(|_| invalid("invalid metadata integer")),
            _ => Err(invalid("expected a quoted string or unsigned integer")),
        }
    }
    fn value(&mut self) -> Result<Value> {
        if self.peek() != Some(b'[') {
            return self.scalar();
        }
        self.pos += 1;
        let mut items = Vec::new();
        if self.peek() != Some(b']') {
            loop {
                if items.len() >= 65536 {
                    return Err(Error::Limit(
                        "LVM metadata list exceeds 65536 values".into(),
                    ));
                }
                items.push(self.scalar()?);
                if self.peek() != Some(b',') {
                    break;
                }
                self.pos += 1;
            }
        }
        if self.peek() != Some(b']') {
            return Err(invalid("unterminated metadata list"));
        }
        self.pos += 1;
        Ok(Value::List(items))
    }
    fn fields(&mut self, depth: usize) -> Result<Section> {
        if depth > 12 {
            return Err(Error::Limit("LVM metadata nesting exceeds 12".into()));
        }
        let mut fields = Section::new();
        while let Some(b) = self.peek() {
            if b == b'}' && depth != 0 {
                self.pos += 1;
                return Ok(fields);
            }
            self.entries += 1;
            if self.entries > 131072 {
                return Err(Error::Limit("LVM metadata exceeds 131072 fields".into()));
            }
            let key = self.word()?.to_owned();
            let v = match self.peek() {
                Some(b'=') => {
                    self.pos += 1;
                    self.value()?
                }
                Some(b'{') => {
                    self.pos += 1;
                    Value::Section(self.fields(depth + 1)?)
                }
                _ => return Err(invalid("missing metadata assignment")),
            };
            if fields.insert(key, v).is_some() {
                return Err(invalid("duplicate metadata field"));
            }
        }
        if depth != 0 {
            return Err(invalid("unterminated metadata section"));
        }
        Ok(fields)
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Section> {
    let bytes = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| invalid("missing metadata terminator"))?;
    if bytes.contains(&0) {
        return Err(invalid("embedded metadata terminator"));
    }
    Parser {
        bytes,
        pos: 0,
        entries: 0,
    }
    .fields(0)
}
