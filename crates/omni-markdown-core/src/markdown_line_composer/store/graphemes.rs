//! 同源GCB按实际排版前缀迁入唯一输出FD；原BreakJob仍是唯一字素算法与全局ordinal权威。
use super::*;
use crate::{MarkdownGraphemeBoundary, MarkdownLayoutBreakJob};

mod links;
mod search;
use links::Ancestors;
pub(super) use search::BoundarySearch;
pub(super) use search::{lower_bound, ordinal_boundary};

pub(super) const GRAPHEME: u8 = 12;

/// 不连续record的固定目录；count是原全局ordinal前缀，covered不是glyph布局范围。
#[derive(Clone, Copy)]
pub(super) struct GraphemeDirectory {
    pub last: usize,
    pub count: usize,
    pub total: Option<usize>,
    pub covered: MarkdownGraphemeBoundary,
    pub complete: bool,
}
impl GraphemeDirectory {
    pub(super) fn validate(&self, bytes: usize, utf16: usize, records: usize) -> Result<()> {
        let eof = self.covered == (MarkdownGraphemeBoundary { byte: bytes, utf16 });
        if self.count == 0
            || self.total.is_some_and(|total| self.count > total)
            || self.last >= records
            || self.covered.byte > bytes
            || self.covered.utf16 > utf16
            || self.complete != (self.total == Some(self.count))
            || (self.complete && !eof)
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }
}

pub(super) struct GraphemeCapture {
    total: Option<usize>,
    written: usize,
    last: Option<(usize, MarkdownGraphemeBoundary)>,
    pending: Option<MarkdownGraphemeBoundary>,
    ancestors: Ancestors,
}

impl GraphemeCapture {
    /// 一个已读取但尚未写入的记录仍属于本次迁移；不能在它之前开始另一producer。
    pub(super) fn has_buffered_record(&self) -> bool {
        self.pending.is_some()
    }

    fn directory(&self) -> Result<GraphemeDirectory> {
        let (last, covered) = self.last.ok_or(Error::InvalidRequest)?;
        Ok(GraphemeDirectory {
            last,
            covered,
            count: self.written,
            total: self.total,
            complete: self.total == Some(self.written),
        })
    }
}

/// records只计真实64B写入，metadata_units计原GCB读取；两份配额不互相借用。
pub(in crate::markdown_line_composer) struct GraphemeCaptureStep {
    pub records: usize,
    pub metadata_units: usize,
    pub ready: bool,
}

impl GlyphStore {
    /// 只迁入截至原实际item/control端点的边界；不读正文、不把配额边界当GCB或EOF。
    pub(in crate::markdown_line_composer) fn capture_graphemes_through(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        through_utf16: usize,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        let result = (|| {
            self.guard()?;
            if records == 0
                || metadata_units == 0
                || records > STEP
                || metadata_units > STEP
                || through_utf16 > self.source.utf16_len()
                || self.capture.is_some()
                || self.control_capture.is_some()
                || !matches!(self.index, Some(Rows::Writing(_)))
            {
                return Err(Error::InvalidRequest);
            }
            let mut step = GraphemeCaptureStep {
                records: 0,
                metadata_units: 0,
                ready: false,
            };
            #[cfg(feature = "paged-shaping-frontier")]
            let (known, total) = {
                let prefix = breaks.grapheme_prefix_facts(&self.authority)?;
                (prefix.known_boundary_count, prefix.total_count)
            };
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let (known, total) = {
                let Some(total) = breaks.ready_grapheme_count(&self.authority)? else {
                    return Ok(step);
                };
                (total, Some(total))
            };
            if known == 0 {
                // 原GCB尚无首条记录；通过有限读留下实际需求，不能把空前缀解释为EOF。
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    step.metadata_units = 1;
                    breaks.read_grapheme_prefix(
                        &self.authority,
                        0..1,
                        &mut [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }],
                    )?;
                }
                return Ok(step);
            }
            let capture = self.graphemes.get_or_insert_with(|| GraphemeCapture {
                total,
                written: 0,
                last: None,
                pending: None,
                ancestors: Ancestors::new(),
            });
            if capture.total.is_some() && total != capture.total {
                return Err(Error::InconsistentCoverage);
            }
            capture.total = total; // None只表示总数未封存；不能由已知前缀长度推断末ordinal。
            let file = self.index.as_mut().ok_or(Error::Poisoned)?;
            loop {
                if let Some((_, covered)) = capture.last
                    && covered.utf16 >= through_utf16
                {
                    if covered.utf16 != through_utf16 && step.records != 0 {
                        return Err(Error::InconsistentCoverage);
                    } // 新目标必须是真实item GCB；已捕获更长前缀则可供较短行继续消费。
                    step.ready = true;
                    break;
                }
                if capture.pending.is_none() {
                    if step.metadata_units == metadata_units {
                        break;
                    }
                    if total == Some(capture.written) {
                        return Err(Error::InconsistentCoverage);
                    }
                    let mut value = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                    let end = capture.written.checked_add(1).ok_or(Error::Overflow)?;
                    step.metadata_units += 1;
                    #[cfg(feature = "paged-shaping-frontier")]
                    let read = breaks.read_grapheme_prefix(
                        &self.authority,
                        capture.written..end,
                        &mut value,
                    )?;
                    #[cfg(not(feature = "paged-shaping-frontier"))]
                    let read = breaks.read_ready_graphemes(
                        &self.authority,
                        capture.written..end,
                        &mut value,
                    )?;
                    if !read {
                        break;
                    } // 原ordinal尚未提交，保留同capture并让下一量子推进原GCB。
                    capture.pending = Some(value[0]); // 写预算用尽时只保一条实际已读记录，不重读计费。
                }
                if step.records == records {
                    break;
                }
                let value = capture.pending.ok_or(Error::InconsistentCoverage)?;
                if value.byte > self.source.byte_len()
                    || value.utf16 > self.source.utf16_len()
                    || capture.last.is_some_and(|(_, previous)| {
                        previous.byte >= value.byte || previous.utf16 >= value.utf16
                    })
                    || (capture.written == 0
                        && value != (MarkdownGraphemeBoundary { byte: 0, utf16: 0 }))
                {
                    return Err(Error::InconsistentCoverage);
                }
                let record = file.len();
                let row = capture.ancestors.encode(
                    value,
                    capture.written,
                    capture.last.map(|value| value.0),
                )?;
                file.append(row)?;
                capture.ancestors.publish(capture.written, record)?;
                capture.written = capture.written.checked_add(1).ok_or(Error::Overflow)?;
                capture.last = Some((record, value));
                capture.pending = None;
                step.records += 1;
                if total == Some(capture.written)
                    && value
                        != (MarkdownGraphemeBoundary {
                            byte: self.source.byte_len(),
                            utf16: self.source.utf16_len(),
                        })
                {
                    return Err(Error::InconsistentCoverage);
                } // 只有原全局count与真实byte/UTF16尾都一致，才有完整EOF。
            }
            breaks.require_source(&self.authority)?;
            Ok(step)
        })();
        self.finish(result)
    }

    /// 旧有限测试仍可迁完整表作为oracle；生产入口只传实际item/control端点。
    #[cfg(test)]
    pub(in crate::markdown_line_composer) fn capture_graphemes(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        records: usize,
    ) -> Result<bool> {
        Ok(self
            .capture_graphemes_through(
                breaks,
                self.source.utf16_len(),
                records.min(STEP),
                records.min(STEP),
            )?
            .ready)
    }

    pub(super) fn grapheme_directory(&self) -> Result<GraphemeDirectory> {
        self.guard()?;
        let directory = self
            .graphemes
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .directory()?;
        directory.validate(
            self.source.byte_len(),
            self.source.utf16_len(),
            self.index.as_ref().ok_or(Error::Poisoned)?.len(),
        )?;
        Ok(directory)
    }

    pub(super) fn published_grapheme_directory(&self) -> Result<GraphemeDirectory> {
        self.guard()?;
        self.published_graphemes.ok_or(Error::InvalidRequest)
    }

    pub(super) fn is_grapheme(&mut self, offset: usize) -> Result<bool> {
        let directory = self.grapheme_directory()?;
        if offset > self.source.utf16_len() {
            return Err(Error::InvalidRequest);
        }
        if offset > directory.covered.utf16 {
            return Err(Error::GraphemePrefixPending);
        }
        let bytes = self.source.byte_len();
        let units = self.source.utf16_len();
        let (_, value) = lower_bound(directory, offset, bytes, units, |record| {
            self.guard()?;
            Ok(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)
        })?;
        self.guard()?;
        Ok(value.is_some_and(|value| value.utf16 == offset))
    }
}
