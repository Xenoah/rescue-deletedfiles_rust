//! Explicit user-defined boundaries. A signature match does not authenticate content.
use super::{charge, container::MAX_ENCODED};
use crate::{Error, Result, source::Image};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SignatureRule {
    pub(super) extension: String,
    pub(super) header: Vec<u8>,
    end: End,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum End {
    Size(usize),
    Footer(Vec<u8>),
}

fn hex(input: &str, minimum: usize) -> std::result::Result<Vec<u8>, String> {
    if !(minimum * 2..=32).contains(&input.len())
        || !input.len().is_multiple_of(2)
        || !input.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(format!(
            "expected {minimum}..16 bytes encoded in hexadecimal"
        ));
    }
    (0..input.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&input[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

impl FromStr for SignatureRule {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let mut parts = s.split(':');
        let ext = parts.next().unwrap_or_default();
        if ext.is_empty() || ext.len() > 16 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("expected an alphanumeric extension of 1..16 characters".into());
        }
        let header = hex(parts.next().unwrap_or_default(), 4)?;
        let tail = parts.next().unwrap_or_default();
        let end = if let Some(size) = tail.strip_prefix("size=") {
            let n = size.parse().map_err(|_| "invalid decimal file length")?;
            if !(header.len()..=MAX_ENCODED).contains(&n) {
                return Err("file length must include the header and fit in 64 MiB".into());
            }
            End::Size(n)
        } else if let Some(footer) = tail.strip_prefix("end=") {
            End::Footer(hex(footer, 2)?)
        } else {
            return Err("expected EXT:HEADERHEX:size=BYTES or EXT:HEADERHEX:end=FOOTERHEX".into());
        };
        if parts.next().is_some() {
            return Err("extra signature rule field".into());
        }
        Ok(Self {
            extension: ext.to_ascii_lowercase(),
            header,
            end,
        })
    }
}

impl TryFrom<String> for SignatureRule {
    type Error = String;
    fn try_from(s: String) -> std::result::Result<Self, String> {
        s.parse()
    }
}
impl From<SignatureRule> for String {
    fn from(rule: SignatureRule) -> Self {
        rule.to_string()
    }
}
impl fmt::Display for SignatureRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.extension)?;
        for b in &self.header {
            write!(f, "{b:02x}")?;
        }
        match &self.end {
            End::Size(n) => write!(f, ":size={n}"),
            End::Footer(bytes) => {
                write!(f, ":end=")?;
                for b in bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
    }
}

impl SignatureRule {
    pub(super) fn extract(
        &self,
        image: &mut Image,
        at: u64,
        cancel: &AtomicBool,
        budget: &mut u64,
        boundary: u64,
    ) -> Result<Vec<u8>> {
        let available = boundary
            .checked_sub(at)
            .ok_or(Error::SourceChanged)?
            .min(MAX_ENCODED as u64) as usize;
        let limit = match self.end {
            End::Size(n) => n,
            End::Footer(_) => available,
        };
        if limit < self.header.len() || limit > available {
            return Err(Error::Invalid("rule extent exceeds scan range".into()));
        }
        charge(budget, self.header.len())?;
        let mut bytes = image.read(at, self.header.len())?;
        if bytes != self.header {
            return Err(Error::SourceChanged);
        }
        while bytes.len() < limit {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let start = bytes.len();
            let n = (limit - start).min(64 * 1024);
            charge(budget, n)?;
            let (part, gaps) = image.read_available(at + start as u64, n)?;
            bytes.extend_from_slice(&part);
            if let End::Footer(footer) = &self.end {
                let search = start
                    .saturating_sub(footer.len() - 1)
                    .max(self.header.len());
                if let Some(pos) = bytes[search..]
                    .windows(footer.len())
                    .position(|b| b == footer)
                {
                    bytes.truncate(search + pos + footer.len());
                    // A read-ahead gap may follow the footer. Require coverage of the exact extent.
                    if !gaps.is_empty() {
                        charge(budget, bytes.len() - start)?;
                        image.read(at + start as u64, bytes.len() - start)?;
                    }
                    return Ok(bytes);
                }
            }
            if !gaps.is_empty() {
                return Err(Error::MissingRanges(gaps));
            }
        }
        match self.end {
            End::Size(_) => Ok(bytes),
            End::Footer(_) => Err(Error::Invalid(
                "rule footer missing within scan range or 64 MiB".into(),
            )),
        }
    }
}
