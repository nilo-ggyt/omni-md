//! 同源GCB按实际排版前缀迁入唯一输出FD；显式可见ordinal域不冒充未解析隐区的原全源序号。
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
    pub domain: crate::MarkdownGraphemeOrdinalDomain,
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
    domain: crate::MarkdownGraphemeOrdinalDomain,
    total: Option<usize>,
    written: usize,
    read_ordinal: usize,
    source_total: Option<usize>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    skip_search: Option<(usize, usize)>,
    last: Option<(usize, MarkdownGraphemeBoundary)>,
    pending: Option<MarkdownGraphemeBoundary>,
    ancestors: Ancestors,
}

impl GraphemeCapture {
    /// 未来 GCB 可由一个真实 UBA seam 隔开；只有本 item 应先写入的边界仍阻止 glyph 捕获。
    pub(super) fn has_buffered_through(&self, through: usize) -> bool {
        self.pending
            .is_some_and(|boundary| boundary.utf16 <= through)
    }

    fn directory(&self) -> Result<GraphemeDirectory> {
        let (last, covered) = self.last.ok_or(Error::InvalidRequest)?;
        Ok(GraphemeDirectory {
            domain: self.domain,
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

/// 普通控制仍只请求真实 GCB；内部整形端点必须借原 ItemScan 签发的同 Source 票。
#[derive(Clone, Copy)]
enum CaptureEnd<'a> {
    Grapheme(usize),
    BidiSeam(&'a crate::markdown_line_composer::item::ResolvedBidiShapingSeam),
}

impl CaptureEnd<'_> {
    fn utf16(self, authority: &MarkdownLayoutTextAuthority) -> Result<usize> {
        match self {
            Self::Grapheme(end) => Ok(end),
            Self::BidiSeam(proof) => Ok(proof.boundary(authority)?.1),
        }
    }
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
        self.capture_graphemes_until(
            breaks,
            CaptureEnd::Grapheme(through_utf16),
            records,
            metadata_units,
        )
    }

    /// 字形 item 的内部 UBA seam 不进入 GCB 目录；保留真实后继边界供下一 item 完成后迁入。
    pub(in crate::markdown_line_composer) fn capture_item_graphemes(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        item: &MarkdownLineItem,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        item_shaping_seams(item, &self.authority)?;
        let target = match &item.shaping_seams()[1] {
            Some(proof) => CaptureEnd::BidiSeam(proof),
            None => CaptureEnd::Grapheme(item.facts().source_utf16.end),
        };
        self.capture_graphemes_until(breaks, target, records, metadata_units)
    }

    fn capture_graphemes_until(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        target: CaptureEnd<'_>,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        let result = (|| {
            self.guard()?;
            let through_utf16 = target.utf16(&self.authority)?;
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
            let domain = breaks.grapheme_ordinal_domain(&self.authority)?;
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
                domain,
                total: if domain == crate::MarkdownGraphemeOrdinalDomain::TableVisible {
                    None
                } else {
                    total
                },
                written: 0,
                read_ordinal: 0,
                source_total: total,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                skip_search: None,
                last: None,
                pending: None,
                ancestors: Ancestors::new(),
            });
            if capture.domain != domain
                || capture.source_total.is_some() && total != capture.source_total
            {
                return Err(Error::InconsistentCoverage);
            }
            capture.source_total = total;
            if domain != crate::MarkdownGraphemeOrdinalDomain::TableVisible {
                capture.total = total;
            }
            // 可见域的总数只在真实 EOF record 经过筛选并写入后封存，原 raw count 不等于可见 count。
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
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                if let Some((mut low, mut high)) = capture.skip_search {
                    if low < high {
                        if step.metadata_units == metadata_units {
                            break;
                        }
                        let middle = low + (high - low) / 2;
                        let mut value = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                        step.metadata_units += 1;
                        if !breaks.read_grapheme_prefix(
                            &self.authority,
                            middle..middle + 1,
                            &mut value,
                        )? {
                            break;
                        }
                        let span = self.table_skip.ok_or(Error::InconsistentCoverage)?;
                        if value[0].utf16 < span.end.utf16 {
                            low = middle + 1;
                        } else {
                            high = middle;
                        }
                        capture.skip_search = Some((low, high));
                        continue;
                    }
                    capture.read_ordinal = low;
                    capture.skip_search = None;
                }
                if capture.pending.is_none() {
                    if step.metadata_units == metadata_units {
                        break;
                    }
                    if total == Some(capture.read_ordinal) {
                        return Err(Error::InconsistentCoverage);
                    }
                    let mut value = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                    let end = capture.read_ordinal.checked_add(1).ok_or(Error::Overflow)?;
                    step.metadata_units += 1;
                    #[cfg(feature = "paged-shaping-frontier")]
                    let read = breaks.read_grapheme_prefix(
                        &self.authority,
                        capture.read_ordinal..end,
                        &mut value,
                    )?;
                    #[cfg(not(feature = "paged-shaping-frontier"))]
                    let read = breaks.read_ready_graphemes(
                        &self.authority,
                        capture.read_ordinal..end,
                        &mut value,
                    )?;
                    if !read {
                        break;
                    } // 原ordinal尚未提交，保留同capture并让下一量子推进原GCB。
                    capture.read_ordinal = end;
                    capture.pending = Some(value[0]); // 写预算用尽时只保一条实际已读记录，不重读计费。
                }
                let value = capture.pending.ok_or(Error::InconsistentCoverage)?;
                if value.utf16 > through_utf16 && matches!(target, CaptureEnd::BidiSeam(_)) {
                    // 原 BreakJob 已交付真实下一 GCB，但它在本 item 的 UBA 端点之后。
                    // 本轮不写一个伪边界、不丢已读记录；唯一后继 item 会继续消费同 pending。
                    step.ready = true;
                    break;
                }
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                if self.table_skip.is_some_and(|span| {
                    span.start.utf16 < value.utf16 && value.utf16 < span.end.utf16
                }) {
                    // 旧短行可能已被原 job 看过；只二分它的真实已知前缀，不迁入内部 raw GCB。
                    capture.pending = None;
                    capture.skip_search = Some((capture.read_ordinal, known));
                    continue;
                }
                if step.records == records {
                    break;
                }
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
                if total == Some(capture.read_ordinal)
                    && value
                        != (MarkdownGraphemeBoundary {
                            byte: self.source.byte_len(),
                            utf16: self.source.utf16_len(),
                        })
                {
                    return Err(Error::InconsistentCoverage);
                } // 只有原全局 count 与真实 byte/UTF16 尾都一致，才有完整 EOF。
                if total == Some(capture.read_ordinal) {
                    capture.total = Some(capture.written);
                }
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
