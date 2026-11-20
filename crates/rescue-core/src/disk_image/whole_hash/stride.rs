//! Hash ordered runs separated by a fixed physical distance, in either direction.
use super::{HashExtent, HashSearch, Search};
use crate::{Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub enum StrideSpacing {
    /// Signed distance between the starts of two successive runs, in blocks.
    Fixed(i32),
    /// Try each non-overlapping distance up to this bound, in both directions.
    UpTo(u32),
}

#[derive(Clone, Copy)]
pub struct StrideSearch {
    pub run_blocks: u32,
    pub spacing: StrideSpacing,
}

#[derive(Serialize)]
pub struct MatchedStride {
    pub block_size: u32,
    pub run_blocks: u32,
    pub stride_blocks: i32,
}

impl StrideSearch {
    fn check(self, req: &HashSearch<'_>) -> Result<()> {
        if self.run_blocks == 0 || self.run_blocks > 65536 {
            return Err(Error::Request(
                "run size must be between 1 and 65536 blocks".into(),
            ));
        }
        match self.spacing {
            StrideSpacing::Fixed(n) if !(self.run_blocks..=65536).contains(&n.unsigned_abs()) => {
                Err(Error::Request(
                    "stride magnitude must be at least the run size and at most 65536 blocks"
                        .into(),
                ))
            }
            StrideSpacing::UpTo(_) if req.first_offset.is_none() => Err(Error::Request(
                "searching multiple strides requires a known first offset".into(),
            )),
            StrideSpacing::UpTo(n) if !(self.run_blocks..=256).contains(&n) => Err(Error::Request(
                "maximum stride must be at least the run size and at most 256 blocks".into(),
            )),
            _ => Ok(()),
        }
    }
}

struct Pattern {
    size: u64,
    run: u64,
    runs: u64,
    distance: u64,
    reverse: bool,
}

impl Pattern {
    fn spans(&self, first: u64) -> impl Iterator<Item = HashExtent> + '_ {
        (0..self.runs).map(move |i| {
            let displacement = i * self.distance;
            HashExtent {
                offset: if self.reverse {
                    first - displacement
                } else {
                    first + displacement
                },
                length: self.run.min(self.size - i * self.run),
            }
        })
    }

    // Every extent must lie in [start, end). The final run can be short.
    fn starts(&self, start: u64, end: u64) -> Option<(u64, u64)> {
        let displacement = (self.runs - 1) * self.distance;
        let last_len = self.size - (self.runs - 1) * self.run;
        if self.reverse {
            Some((
                start.checked_add(displacement)?,
                end.checked_sub(self.run.min(self.size))?,
            ))
        } else {
            Some((start, end.checked_sub(displacement + last_len)?))
        }
    }
}

impl Search<'_> {
    fn try_pattern(
        &mut self,
        pattern: &Pattern,
        req: &HashSearch<'_>,
        end: u64,
    ) -> Result<Option<Vec<HashExtent>>> {
        let Some((low, high)) = pattern.starts(req.start, end) else {
            return Ok(None);
        };
        if low > high {
            return Ok(None);
        }
        let grid = u64::from(req.block_size);
        let (first, last) = if let Some(at) = req.first_offset {
            if !(low..=high).contains(&at) {
                return Ok(None);
            }
            (at, at)
        } else {
            let pad = (grid - (low - req.start) % grid) % grid;
            let Some(at) = low.checked_add(pad) else {
                return Ok(None);
            };
            (at, high)
        };
        for at in (first..=last).step_by(grid as usize) {
            self.start_candidate()?;
            let mut digest = Sha256::new();
            let mut readable = true;
            for span in pattern.spans(at) {
                if !self.append(&mut digest, span)? {
                    readable = false;
                    break;
                }
            }
            if readable && <[u8; 32]>::from(digest.finalize()) == self.target {
                return Ok(Some(pattern.spans(at).collect()));
            }
        }
        Ok(None)
    }

    pub(super) fn find_strided(
        &mut self,
        req: &HashSearch<'_>,
        end: u64,
        stride: StrideSearch,
    ) -> Result<(Vec<HashExtent>, MatchedStride)> {
        stride.check(req)?;
        let unit = u64::from(req.block_size);
        let run = unit * u64::from(stride.run_blocks);
        let runs = req.file_bytes.div_ceil(run);
        if runs > 65536 {
            return Err(Error::Limit(
                "regular layout exceeds 65536 source runs".into(),
            ));
        }
        let distances: Vec<i32> = match stride.spacing {
            StrideSpacing::Fixed(d) => vec![d],
            StrideSpacing::UpTo(max) => (stride.run_blocks..=max)
                .flat_map(|n| [n as i32, -(n as i32)])
                .collect(),
        };
        for d in distances {
            let pattern = Pattern {
                size: req.file_bytes,
                run,
                runs,
                distance: u64::from(d.unsigned_abs()) * unit,
                reverse: d < 0,
            };
            if let Some(found) = self.try_pattern(&pattern, req, end)? {
                return Ok((
                    found,
                    MatchedStride {
                        block_size: req.block_size,
                        run_blocks: stride.run_blocks,
                        stride_blocks: d,
                    },
                ));
            }
            // A one-run file has the same positions for every spacing.
            if runs == 1 {
                break;
            }
        }
        Err(self.no_match())
    }
}
