//! Strict classic-xref PDF extent validation. No rendering, scripts or external I/O.
//! Xref/object streams and encrypted documents are explicitly unsupported here.
use super::container::Input;
use crate::{Error, Result, source::Image};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};
fn invalid(s: &str) -> Error {
    Error::Invalid(format!("PDF: {s}"))
}
fn white(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}
fn delimiter(c: u8) -> bool {
    white(c) || b"()<>[]{}/%".contains(&c)
}

#[derive(Clone, Debug)]
enum Value {
    Int(i64),
    Ref(u32, u16),
    Name(Vec<u8>),
    Array(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, Value>),
    Scalar,
}
impl Value {
    fn integer(&self) -> Result<usize> {
        if let Self::Int(n) = self {
            usize::try_from(*n).map_err(|_| invalid("negative or excessive integer"))
        } else {
            Err(invalid("expected integer"))
        }
    }
    fn reference(&self) -> Result<(u32, u16)> {
        if let Self::Ref(n, g) = self {
            Ok((*n, *g))
        } else {
            Err(invalid("expected indirect reference"))
        }
    }
    fn dict(&self) -> Result<&BTreeMap<Vec<u8>, Value>> {
        if let Self::Dict(d) = self {
            Ok(d)
        } else {
            Err(invalid("expected dictionary"))
        }
    }
    fn named(&self, s: &[u8]) -> bool {
        matches!(self,Self::Name(n) if n==s)
    }
}
struct Lexer<'a> {
    b: &'a [u8],
    p: usize,
    tokens: usize,
}
impl<'a> Lexer<'a> {
    fn new(b: &'a [u8], p: usize) -> Self {
        Self { b, p, tokens: 0 }
    }
    fn skip(&mut self) {
        loop {
            while self.p < self.b.len() && white(self.b[self.p]) {
                self.p += 1;
            }
            if self.b.get(self.p) != Some(&b'%') {
                break;
            }
            while self.p < self.b.len() && !b"\r\n".contains(&self.b[self.p]) {
                self.p += 1;
            }
        }
    }
    fn word(&mut self) -> Result<&'a [u8]> {
        self.skip();
        let start = self.p;
        while self.p < self.b.len() && !delimiter(self.b[self.p]) {
            self.p += 1;
        }
        if start == self.p {
            return Err(invalid("expected token"));
        }
        Ok(&self.b[start..self.p])
    }
    fn number(&mut self) -> Result<usize> {
        let s = self.word()?;
        if !s.iter().all(u8::is_ascii_digit) {
            return Err(invalid("expected unsigned integer"));
        }
        std::str::from_utf8(s)
            .unwrap()
            .parse()
            .map_err(|_| invalid("integer overflow"))
    }
    fn keyword(&mut self, s: &[u8]) -> Result<()> {
        if self.word()? == s {
            Ok(())
        } else {
            Err(invalid("unexpected keyword"))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        self.tokens += 1;
        if depth > 64 || self.tokens > 262144 {
            return Err(Error::Limit("PDF nesting or token limit".into()));
        }
        self.skip();
        let c = *self
            .b
            .get(self.p)
            .ok_or_else(|| invalid("truncated object"))?;
        match c {
            b'/' => {
                self.p += 1;
                let mut name = Vec::new();
                while self.p < self.b.len() && !delimiter(self.b[self.p]) {
                    let c = self.b[self.p];
                    self.p += 1;
                    if c == b'#' {
                        if self.p + 2 > self.b.len() {
                            return Err(invalid("name escape"));
                        }
                        name.push(
                            hex(self.b[self.p])?.checked_mul(16).unwrap()
                                + hex(self.b[self.p + 1])?,
                        );
                        self.p += 2;
                    } else {
                        name.push(c);
                    }
                    if name.len() > 4096 {
                        return Err(Error::Limit("PDF name length".into()));
                    }
                }
                Ok(Value::Name(name))
            }
            b'(' => {
                self.p += 1;
                let mut level = 1;
                while self.p < self.b.len() && level != 0 {
                    let c = self.b[self.p];
                    self.p += 1;
                    match c {
                        b'\\' => {
                            if self.p >= self.b.len() {
                                return Err(invalid("string escape"));
                            }
                            self.p += 1;
                        }
                        b'(' => {
                            level += 1;
                            if level > 64 {
                                return Err(Error::Limit("PDF string nesting".into()));
                            }
                        }
                        b')' => level -= 1,
                        _ => {}
                    }
                }
                if level != 0 {
                    return Err(invalid("unterminated string"));
                }
                Ok(Value::Scalar)
            }
            b'<' if self.b.get(self.p + 1) == Some(&b'<') => {
                self.p += 2;
                let mut dict = BTreeMap::new();
                loop {
                    self.skip();
                    if self.b.get(self.p..self.p + 2) == Some(b">>") {
                        self.p += 2;
                        break;
                    }
                    let Value::Name(name) = self.value(depth + 1)? else {
                        return Err(invalid("dictionary key"));
                    };
                    let value = self.value(depth + 1)?;
                    if dict.insert(name, value).is_some() {
                        return Err(invalid("duplicate dictionary key"));
                    }
                }
                Ok(Value::Dict(dict))
            }
            b'<' => {
                self.p += 1;
                while self.p < self.b.len() && self.b[self.p] != b'>' {
                    if !white(self.b[self.p]) {
                        hex(self.b[self.p])?;
                    }
                    self.p += 1;
                }
                if self.p == self.b.len() {
                    return Err(invalid("unterminated hex string"));
                }
                self.p += 1;
                Ok(Value::Scalar)
            }
            b'[' => {
                self.p += 1;
                let mut array = Vec::new();
                loop {
                    self.skip();
                    if self.b.get(self.p) == Some(&b']') {
                        self.p += 1;
                        break;
                    }
                    array.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(array))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let word = self.word()?;
                let text = std::str::from_utf8(word).map_err(|_| invalid("numeric token"))?;
                if text.contains('.') {
                    let digits = text.trim_start_matches(['+', '-']);
                    if digits.is_empty()
                        || digits == "."
                        || digits.bytes().filter(|c| *c == b'.').count() != 1
                        || !digits.bytes().all(|c| c == b'.' || c.is_ascii_digit())
                    {
                        return Err(invalid("real number"));
                    }
                    return Ok(Value::Scalar);
                }
                let n = text.parse::<i64>().map_err(|_| invalid("integer token"))?;
                let after = self.p;
                if (0..=u32::MAX as i64).contains(&n) {
                    let reference = (|| {
                        let generation = self.number()?;
                        if generation > 65535 {
                            return Err(invalid("generation"));
                        }
                        self.keyword(b"R")?;
                        Ok(Value::Ref(n as u32, generation as u16))
                    })();
                    if let Ok(value) = reference {
                        return Ok(value);
                    }
                    self.p = after;
                }
                Ok(Value::Int(n))
            }
            _ => {
                if [b"true".as_slice(), b"false", b"null"].contains(&self.word()?) {
                    Ok(Value::Scalar)
                } else {
                    Err(invalid("unknown object token"))
                }
            }
        }
    }
}
fn hex(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(invalid("hex digit")),
    }
}
type Key = (u32, u16);
type Objects = BTreeMap<Key, Value>;

fn references(value: &Value, objects: &Objects, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(Error::Limit("PDF reference nesting".into()));
    }
    match value {
        Value::Ref(n, g) => {
            if !objects.contains_key(&(*n, *g)) {
                return Err(invalid("unresolved indirect reference"));
            }
        }
        Value::Array(a) => {
            for v in a {
                references(v, objects, depth + 1)?;
            }
        }
        Value::Dict(d) => {
            for v in d.values() {
                references(v, objects, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn pages(
    key: Key,
    parent: Option<Key>,
    objects: &Objects,
    seen: &mut BTreeSet<Key>,
    depth: usize,
) -> Result<usize> {
    if depth > 64 || !seen.insert(key) {
        return Err(invalid("cyclic or shared page tree"));
    }
    let d = objects
        .get(&key)
        .ok_or_else(|| invalid("page tree reference"))?
        .dict()?;
    if let Some(parent) = parent
        && d.get(b"Parent".as_slice())
            .ok_or_else(|| invalid("missing page parent"))?
            .reference()?
            != parent
    {
        return Err(invalid("page parent mismatch"));
    }
    match d.get(b"Type".as_slice()) {
        Some(v) if v.named(b"Page") => Ok(1),
        Some(v) if v.named(b"Pages") => {
            let Some(Value::Array(kids)) = d.get(b"Kids".as_slice()) else {
                return Err(invalid("page tree kids"));
            };
            let mut count = 0usize;
            for child in kids {
                count = count
                    .checked_add(pages(
                        child.reference()?,
                        Some(key),
                        objects,
                        seen,
                        depth + 1,
                    )?)
                    .ok_or_else(|| invalid("page count overflow"))?;
            }
            if d.get(b"Count".as_slice())
                .ok_or_else(|| invalid("page count"))?
                .integer()?
                != count
            {
                return Err(invalid("page count mismatch"));
            }
            Ok(count)
        }
        _ => Err(invalid("page tree type")),
    }
}

fn document(bytes: &[u8], eof: usize, cancel: &AtomicBool) -> Result<()> {
    if bytes.len() < 9
        || &bytes[..5] != b"%PDF-"
        || !matches!(
            &bytes[5..8],
            b"1.0" | b"1.1" | b"1.2" | b"1.3" | b"1.4" | b"1.5" | b"1.6" | b"1.7" | b"2.0"
        )
        || !white(bytes[8])
    {
        return Err(invalid("header/version"));
    }
    let tail = &bytes[eof.saturating_sub(96)..eof];
    let keyword = tail
        .windows(9)
        .rposition(|s| s == b"startxref")
        .ok_or_else(|| invalid("missing startxref"))?
        + eof.saturating_sub(96);
    let mut end_lexer = Lexer::new(bytes, keyword);
    end_lexer.keyword(b"startxref")?;
    let mut xref = end_lexer.number()?;
    if !bytes[end_lexer.p..eof].iter().all(|c| white(*c)) {
        return Err(invalid("startxref trailer"));
    }
    let mut visited = BTreeSet::new();
    let mut entries = BTreeMap::<u32, Option<(u16, usize)>>::new();
    let mut root = None;
    let mut size = None;
    let mut xref_spans = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if visited.len() >= 128 || !visited.insert(xref) || xref >= keyword {
            return Err(invalid("xref chain bounds or cycle"));
        }
        let mut l = Lexer::new(bytes, xref);
        if !bytes[xref..].starts_with(b"xref") {
            return Err(Error::Unsupported(
                "PDF xref/object streams; classic xref tables are currently supported".into(),
            ));
        }
        l.keyword(b"xref")?;
        let mut section_ids = BTreeSet::new();
        loop {
            l.skip();
            if bytes[l.p..].starts_with(b"trailer") {
                l.keyword(b"trailer")?;
                break;
            }
            let first = l.number()?;
            let count = l.number()?;
            if count == 0 || first.checked_add(count).is_none_or(|n| n > 65536) {
                return Err(Error::Limit("PDF xref object range".into()));
            }
            for id in first..first + count {
                let offset = l.number()?;
                let generation = l.number()?;
                let kind = l.word()?;
                if generation > 65535
                    || ![b"n".as_slice(), b"f"].contains(&kind)
                    || !section_ids.insert(id)
                {
                    return Err(invalid("xref entry"));
                }
                if kind == b"n" && (offset < 8 || offset >= xref || id == 0) {
                    return Err(invalid("xref object offset"));
                }
                entries.entry(id as u32).or_insert(if kind == b"n" {
                    Some((generation as u16, offset))
                } else {
                    None
                });
            }
        }
        let trailer = l.value(0)?;
        let d = trailer.dict()?;
        if d.contains_key(b"Encrypt".as_slice()) || d.contains_key(b"XRefStm".as_slice()) {
            return Err(Error::Unsupported("encrypted or hybrid-xref PDF".into()));
        }
        if root.is_none() {
            root = d
                .get(b"Root".as_slice())
                .map(Value::reference)
                .transpose()?;
        }
        let declared = d
            .get(b"Size".as_slice())
            .ok_or_else(|| invalid("trailer size"))?
            .integer()?;
        if declared > 65536
            || size.is_some_and(|s| declared > s)
            || section_ids.iter().any(|id| *id >= declared)
        {
            return Err(invalid("trailer size inconsistent"));
        }
        size.get_or_insert(declared);
        xref_spans.push((xref, l.p));
        if let Some(prev) = d.get(b"Prev".as_slice()) {
            let p = prev.integer()?;
            if p >= xref {
                return Err(invalid("forward xref link"));
            }
            xref = p;
        } else {
            break;
        }
    }
    let mut objects = Objects::new();
    let mut tails = Vec::new();
    let mut spans = Vec::new();
    let mut tokens = 0usize;
    for (id, entry) in &entries {
        let Some((generation, offset)) = entry else {
            continue;
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut l = Lexer::new(bytes, *offset);
        if l.number()? != *id as usize || l.number()? != usize::from(*generation) {
            return Err(invalid("xref/object identity mismatch"));
        }
        l.keyword(b"obj")?;
        let value = l.value(0)?;
        tokens += l.tokens;
        if tokens > 1048576 {
            return Err(Error::Limit("PDF total token limit".into()));
        }
        objects.insert((*id, *generation), value);
        tails.push(((*id, *generation), *offset, l.p));
    }
    for (key, start, tail) in tails {
        let mut l = Lexer::new(bytes, tail);
        l.skip();
        if bytes[l.p..].starts_with(b"stream") {
            l.keyword(b"stream")?;
            if bytes.get(l.p) == Some(&b'\r') {
                l.p += 1;
            }
            if bytes.get(l.p) != Some(&b'\n') {
                return Err(invalid("stream newline"));
            }
            l.p += 1;
            let length = objects[&key]
                .dict()?
                .get(b"Length".as_slice())
                .ok_or_else(|| invalid("stream length"))?;
            let length = if let Value::Ref(n, g) = length {
                objects
                    .get(&(*n, *g))
                    .ok_or_else(|| invalid("stream length reference"))?
                    .integer()?
            } else {
                length.integer()?
            };
            l.p =
                l.p.checked_add(length)
                    .filter(|n| *n < bytes.len())
                    .ok_or_else(|| invalid("stream extends beyond PDF"))?;
            if bytes.get(l.p) == Some(&b'\r') {
                l.p += 1;
            }
            if bytes.get(l.p) == Some(&b'\n') {
                l.p += 1;
            }
            // Do not skip arbitrary junk or comments between the stream and its terminator.
            if !bytes[l.p..].starts_with(b"endstream") {
                return Err(invalid("stream extent/endstream"));
            }
            l.keyword(b"endstream")?;
        }
        l.keyword(b"endobj")?;
        spans.push((start, l.p));
        references(&objects[&key], &objects, 0)?;
    }
    spans.extend(xref_spans);
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(invalid("overlapping object or xref extents"));
    }
    let root = root.ok_or_else(|| invalid("missing catalog"))?;
    let d = objects
        .get(&root)
        .ok_or_else(|| invalid("catalog reference"))?
        .dict()?;
    if !d
        .get(b"Type".as_slice())
        .is_some_and(|v| v.named(b"Catalog"))
    {
        return Err(invalid("catalog type"));
    }
    let page_root = d
        .get(b"Pages".as_slice())
        .ok_or_else(|| invalid("catalog pages"))?
        .reference()?;
    if pages(page_root, None, &objects, &mut BTreeSet::new(), 0)? == 0 {
        return Err(invalid("empty page tree"));
    }
    Ok(())
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    let mut cursor = 8;
    let mut attempts = 0;
    let mut last = None;
    while cursor + 5 <= input.limit() {
        input.ensure(cursor + 5)?;
        let stop = input.bytes.len().saturating_sub(4);
        while cursor < stop {
            if &input.bytes[cursor..cursor + 5] == b"%%EOF" {
                attempts += 1;
                if attempts > 1024 {
                    return Err(Error::Limit("PDF EOF search attempts".into()));
                }
                input.work(cursor)?;
                match document(&input.bytes[..cursor + 5], cursor, cancel) {
                    Ok(()) => {
                        input.ensure((cursor + 5 + 4096).min(input.limit()))?;
                        let mut end = cursor + 5;
                        if input.bytes.get(end) == Some(&b'\r') {
                            end += 1;
                        }
                        if input.bytes.get(end) == Some(&b'\n') {
                            end += 1;
                        }
                        let mut next = Lexer::new(&input.bytes, end);
                        next.skip();
                        let incremental = input.bytes[next.p..].starts_with(b"xref")
                            || (|| {
                                next.number()?;
                                next.number()?;
                                next.keyword(b"obj")
                            })()
                            .is_ok();
                        if !incremental {
                            return input.finish(end);
                        }
                        last = Some(invalid("incremental update is incomplete"));
                    }
                    Err(e @ (Error::Cancelled | Error::Limit(_) | Error::Unsupported(_))) => {
                        return Err(e);
                    }
                    Err(e) => last = Some(e),
                }
            }
            cursor += 1;
        }
    }
    Err(last.unwrap_or_else(|| invalid("complete xref/trailer/EOF not found")))
}
