//! 真实LineEnd之后的只读交付；Line/Part只含原输出弱票，没有公开Range构造入口。
//!
//! 这里的26.6坐标相对于本行baseline。真实字体ascent/descent/leading与全局行y由平台
//! 使用同一个注册字体计算，本模块不制造固定行高、Native视口Ready或caret几何。

use super::*;
use crate::MarkdownLayoutTextSourceObserver;

mod grapheme_queries;
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
mod table_row;
pub use grapheme_queries::MarkdownComposedGraphemeOrdinalAnswer;
mod visual_caret;
pub use visual_caret::{
    MarkdownComposedCaretAssociation, MarkdownComposedCaretSide, MarkdownComposedVisualAnswer,
    MarkdownComposedVisualGraphemeAnswer, MarkdownComposedVisualGraphemeQuery,
    MarkdownComposedVisualGraphemeStep, MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};
#[cfg(feature = "paged-shaping-frontier")]
pub use visual_caret::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep,
};
mod lookup;
mod trailing_whitespace;
pub use lookup::{
    MarkdownComposedLineAffinity, MarkdownComposedLineLookup, MarkdownComposedLineLookupAnswer,
    MarkdownComposedLineLookupStep, MarkdownComposedLineLookupTarget,
};
pub use trailing_whitespace::{
    MarkdownComposedLineTrailingWhitespace, MarkdownComposedLineTrailingWhitespaceStep,
};

pub(super) const LINE: u8 = 8;
pub(super) const PART: u8 = 9;
pub(super) const PART_META: u8 = 10;
pub(super) const LINE_END: u8 = 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineKind {
    Allowed,
    Mandatory,
    Emergency,
    Overflow,
    /// 原 canonical 结构行；高度来自同一真实 Grid consumer 的最终排版。
    TableRow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownComposedLineFacts {
    pub source_utf16: Range<usize>,
    pub paragraph_utf16: Range<usize>,
    pub parts: usize,
    pub glyphs: usize,
    pub ordinal: usize,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    pub kind: MarkdownComposedLineKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownComposedLinePartFacts {
    /// 同一字体响应已经封存的左右缩进；不增加目录记录或重读活文档样式。
    pub paragraph_inset_26_6: i32,
    /// 来自本part原item封存记录，不能由平台按字符范围事后猜测。
    pub presentation: crate::MarkdownPresentationStyles,
    pub kind: super::super::MarkdownComposedLinePartKind,
    pub source_utf16: Range<usize>,
    pub direction: MarkdownShapingDirection,
    pub level: u8,
    pub script: [u8; 4],
    pub points: f32,
    pub origin_x_26_6: i64,
    pub origin_y_26_6: i64,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    pub glyphs: usize,
    pub visual_glyphs: Range<usize>,
}

/// 只能由完整行目录读出，保留弱输出身份；事实副本不能签发另一输出的part。
pub struct MarkdownComposedLine {
    output: Weak<()>,
    header: usize,
    end: usize,
    previous: Option<usize>,
    skip: Option<(usize, usize)>,
    base_level: u8,
    facts: MarkdownComposedLineFacts,
}
impl MarkdownComposedLine {
    pub fn facts(&self) -> &MarkdownComposedLineFacts {
        &self.facts
    }
    /// Read the base level already authenticated by LineLookup, without new directory or lifetime authority.
    pub fn base_level(&self) -> u8 {
        self.base_level
    }
}

/// 不保活Reader、Source或Font；平台必须仍持原Output及实际字体注册owner。
pub struct MarkdownComposedLinePart {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    registration: MarkdownLineFontRegistration,
    glyph_start: usize,
    cluster_start: usize,
    cluster_count: usize,
    facts: MarkdownComposedLinePartFacts,
}
impl MarkdownComposedLinePart {
    pub fn facts(&self) -> &MarkdownComposedLinePartFacts {
        &self.facts
    }
    pub fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    pub fn font_registration(&self) -> &MarkdownLineFontRegistration {
        &self.registration
    }
}

struct Payload {
    reader: LayoutIndexReader<64>,
    source: MarkdownLayoutTextSource,
    fonts: MarkdownLineFontCatalog,
    folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
}

/// 单一只读FD的真实owner；不持完整glyph、item、Line、Font或正文Vec。
pub struct MarkdownComposedLines {
    payload: Option<Payload>,
    identity: Arc<()>,
    // 只认证同一 writer 的追加前缀，不让旧 Line/Part 身份跨快照生效。
    #[cfg(feature = "paged-shaping-frontier")]
    lineage: Weak<()>,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownLineComposeCancellation,
    last: Option<usize>,
    count: usize,
    range: Range<usize>,
    graphemes: GraphemeDirectory,
    observer: MarkdownComposedLineResourceObserver,
}

/// 观察真实文件/映射与同Source字段的归还，不能凭Line/Part弱票失效冒充底层释放。
#[derive(Clone)]
pub struct MarkdownComposedLineResourceObserver {
    file: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
    backing: Option<crate::MarkdownOriginalBackingObserver>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownComposedLineResources {
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    /// 唯一metadata写buffer；Reader归还该buffer后只保原File。
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub reader_failed: bool,
    pub source_payload_released: bool,
    pub first_source_errno: Option<i32>,
}
impl MarkdownComposedLineResourceObserver {
    pub(super) fn for_source(file: LayoutIndexObserver, source: &MarkdownLayoutTextSource) -> Self {
        Self {
            file,
            source: source.resource_observer(),
            backing: source.original_backing_observer(),
        } // 只取实际owner的固定原子收据；不持source正文或复制任何File。
    }
    pub fn snapshot(&self) -> MarkdownComposedLineResources {
        let file = self.file.snapshot();
        MarkdownComposedLineResources {
            file_open: file.file_open,
            mapped_bytes: file.mapped_bytes,
            peak_mapped_bytes: file.peak_mapped_bytes,
            buffer_bytes: file.buffer_bytes,
            peak_buffer_bytes: file.peak_buffer_bytes,
            cleanup_failed: file.cleanup_failed,
            reader_failed: file.reader_failed,
            source_payload_released: self.source.payload_released(),
            first_source_errno: self.backing.as_ref().and_then(|value| value.first_errno()),
        }
    }
}

/// 只有producer的真实LineEnd流程可以组装这个移交包；它不对外暴露File或目录坐标。
pub(super) struct OutputSeal {
    pub source: MarkdownLayoutTextSource,
    pub authority: MarkdownLayoutTextAuthority,
    pub fonts: MarkdownLineFontCatalog,
    pub cancellation: MarkdownLineComposeCancellation,
    pub reader: LayoutIndexReader<64>,
    pub identity: Arc<()>,
    // 原 GlyphStore 在实际 LineEnd 移交时提供；Word 续算不为布局重置或外来 writer 续权。
    #[cfg(feature = "paged-shaping-frontier")]
    pub lineage: Weak<()>,
    pub last: Option<usize>,
    pub count: usize,
    pub range: Range<usize>,
    pub graphemes: GraphemeDirectory,
    pub folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
}

impl MarkdownComposedLines {
    pub(super) fn from_sealed(seal: OutputSeal) -> Result<Self> {
        seal.source.health()?;
        if !seal.source.permits(&seal.authority)
            || seal.range.start > seal.range.end
            || seal.range.end > seal.source.utf16_len()
            || (seal.count == 0) != seal.last.is_none()
        {
            return Err(Error::InconsistentCoverage);
        }
        if seal.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        seal.graphemes.validate(
            seal.source.byte_len(),
            seal.source.utf16_len(),
            seal.reader.len(),
        )?;
        if seal.graphemes.covered.utf16 < seal.range.end
            || seal.last.is_some_and(|last| seal.graphemes.last >= last)
        {
            return Err(Error::InconsistentCoverage);
        } // 只使用实际最近LineEnd签入的目录；writer随后预取的记录不在本快照权限内。
        let observer = MarkdownComposedLineResourceObserver::for_source(
            seal.reader.resource_observer(),
            &seal.source,
        );
        let value = Self {
            payload: Some(Payload {
                reader: seal.reader,
                source: seal.source,
                fonts: seal.fonts,
                folds: seal.folds,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: seal.tables,
            }),
            identity: seal.identity,
            #[cfg(feature = "paged-shaping-frontier")]
            lineage: seal.lineage,
            authority: seal.authority,
            cancellation: seal.cancellation,
            last: seal.last,
            count: seal.count,
            range: seal.range,
            graphemes: seal.graphemes,
            observer,
        };
        value.require(&value.authority)?;
        Ok(value)
    }
    pub fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.authority
    }
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }
    pub fn resource_observer(&self) -> MarkdownComposedLineResourceObserver {
        self.observer.clone()
    }

    /// 全为Core纯检查；Native调用方的current必须在外层RefCell借用前后分别执行。
    pub fn require(&self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if let Some(errno) = self
            .observer
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        let payload = self.payload.as_ref().ok_or(Error::Poisoned)?;
        payload.source.health()?;
        if authority != &self.authority || !payload.source.permits(authority) {
            return Err(Error::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let file = self.observer.file.snapshot();
        if !file.file_open
            || file.mapped_bytes != 0
            || file.buffer_bytes != 0
            || file.cleanup_failed
            || file.reader_failed
        {
            return Err(Error::Poisoned);
        }
        Ok(())
    }
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if matches!(&result, Err(Error::FoldedRange(_) | Error::TableRange)) {
            // 隐区缺少本记录域的内部 GCB 是可恢复显示状态；健康失败仍永久撤回实际 reader。
            if let Err(error) = self.require(&self.authority) {
                drop(self.payload.take());
                return Err(error);
            }
            return result;
        }
        let result = result.and_then(|value| {
            self.require(&self.authority)?;
            Ok(value)
        });
        if result.is_err() {
            drop(self.payload.take());
        } // 失败交还唯一reader与Source claim，旧Line不能重签。
        if let Some(errno) = self
            .observer
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        result
    }
    fn row(&mut self, record: usize) -> Result<[u8; 64]> {
        self.require(&self.authority)?;
        Ok(self
            .payload
            .as_mut()
            .ok_or(Error::Poisoned)?
            .reader
            .read(record)?)
    }
    fn line_header(&self, end: usize, tail: &[u8; 64]) -> Result<usize> {
        if tail[56] != LINE_END || tail[57..].iter().any(|byte| *byte != 0) {
            return Err(Error::InconsistentCoverage);
        }
        let header = usize_at(tail, 0)?;
        let parts = usize_at(tail, 8)?;
        if parts == 0
            || header.checked_add(1).and_then(|value| {
                parts
                    .checked_mul(2)
                    .and_then(|parts| value.checked_add(parts))
            }) != Some(end)
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(header)
    }
    fn line_at(&mut self, end: usize) -> Result<MarkdownComposedLine> {
        let tail = self.row(end)?;
        let header = self.line_header(end, &tail)?;
        let head = self.row(header)?;
        self.decode_line(end, tail, head)
    }
    /// 同步旧入口与quota1游标共用纯解码；这里不再读取额外记录，避免把两次pread藏成1。
    fn decode_line(
        &self,
        end: usize,
        tail: [u8; 64],
        head: [u8; 64],
    ) -> Result<MarkdownComposedLine> {
        let header = self.line_header(end, &tail)?;
        let parts = usize_at(&tail, 8)?;
        if head[56] != LINE || head[58] > 1 || head[59..].iter().any(|byte| *byte != 0) {
            return Err(Error::InconsistentCoverage);
        }
        let kind = match head[57] {
            0 => MarkdownComposedLineKind::Allowed,
            1 => MarkdownComposedLineKind::Mandatory,
            2 => MarkdownComposedLineKind::Emergency,
            3 => MarkdownComposedLineKind::Overflow,
            4 => MarkdownComposedLineKind::TableRow,
            _ => return Err(Error::InconsistentCoverage),
        };
        let facts = MarkdownComposedLineFacts {
            source_utf16: usize_at(&head, 0)?..usize_at(&head, 8)?,
            paragraph_utf16: usize_at(&head, 16)?..usize_at(&head, 24)?,
            ordinal: usize_at(&head, 32)?,
            parts,
            glyphs: usize_at(&tail, 32)?,
            x_advance_26_6: i64_at(&tail, 16),
            y_advance_26_6: i64_at(&tail, 24),
            kind,
        };
        let previous = match u64_at(&tail, 48) {
            NONE => None,
            _ => Some(usize_at(&tail, 48)?),
        };
        let skip = match (u64_at(&head, 40), u64_at(&head, 48)) {
            (NONE, NONE) if facts.ordinal == 0 => None,
            (record, ordinal) if record != NONE && ordinal != NONE && facts.ordinal != 0 => {
                let record = usize_at(&head, 40)?;
                let ordinal = usize_at(&head, 48)?;
                if ordinal != (facts.ordinal & (facts.ordinal - 1))
                    || record >= header
                    || previous.is_none_or(|previous| record > previous)
                    || (ordinal.checked_add(1) == Some(facts.ordinal) && Some(record) != previous)
                {
                    return Err(Error::InconsistentCoverage);
                }
                Some((record, ordinal))
            }
            _ => return Err(Error::InconsistentCoverage),
        };
        if facts.source_utf16.start > facts.source_utf16.end
            || facts.paragraph_utf16.start > facts.paragraph_utf16.end
            || (facts.ordinal == 0) != previous.is_none()
            || (facts.ordinal == 0 && facts.source_utf16.start != self.range.start)
            || (facts.source_utf16.is_empty()
                && (facts.source_utf16.start != self.range.end
                    || facts.source_utf16.end
                        != self
                            .payload
                            .as_ref()
                            .ok_or(Error::Poisoned)?
                            .source
                            .utf16_len()
                    || facts.kind != MarkdownComposedLineKind::Mandatory
                    || facts.parts != 1
                    || facts.glyphs != 0
                    || facts.x_advance_26_6 != 0
                    || facts.y_advance_26_6 != 0))
            || facts.source_utf16.start < self.range.start
            || facts.source_utf16.end > self.range.end
            || facts.source_utf16.start < facts.paragraph_utf16.start
            || facts.source_utf16.end > facts.paragraph_utf16.end
            || facts.ordinal >= self.count
            || usize_at(&tail, 40)? != facts.source_utf16.len()
            || previous.is_some_and(|record| record >= header)
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(MarkdownComposedLine {
            output: Arc::downgrade(&self.identity),
            header,
            end,
            previous,
            skip,
            base_level: head[58],
            facts,
        })
    }
    fn require_line(&mut self, line: &MarkdownComposedLine) -> Result<()> {
        if !Weak::ptr_eq(&line.output, &Arc::downgrade(&self.identity)) {
            return Err(Error::SourceMismatch);
        }
        let actual = self.line_at(line.end)?;
        if actual.header != line.header
            || actual.facts != line.facts
            || actual.previous != line.previous
            || actual.skip != line.skip
            || actual.base_level != line.base_level
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }
    pub fn last_line(&mut self) -> Result<Option<MarkdownComposedLine>> {
        let result = (|| {
            self.require(&self.authority)?;
            self.last.map(|record| self.line_at(record)).transpose()
        })();
        self.finish(result)
    }

    /// 同一真实Line头里的原段base，不从首part或最小resolved level反推段方向。
    /// 原记录仍为64B，复用两条既有Line认证读取，不持新Bidi或Reader。
    pub fn paragraph_base_level(&mut self, line: &MarkdownComposedLine) -> Result<u8> {
        let result = (|| {
            self.require(&self.authority)?;
            self.require_line(line)?;
            Ok(line.base_level)
        })();
        self.finish(result)
    }
    pub fn previous_line(
        &mut self,
        line: &MarkdownComposedLine,
    ) -> Result<Option<MarkdownComposedLine>> {
        let result = (|| {
            self.require_line(line)?;
            let previous = line
                .previous
                .map(|record| self.line_at(record))
                .transpose()?;
            if let Some(previous) = &previous {
                if previous.facts.ordinal.checked_add(1) != Some(line.facts.ordinal)
                    || previous.facts.source_utf16.end != line.facts.source_utf16.start
                {
                    return Err(Error::InconsistentCoverage);
                }
            } else if line.facts.ordinal != 0 {
                return Err(Error::InconsistentCoverage);
            }
            Ok(previous)
        })();
        self.finish(result)
    }
    pub fn part(
        &mut self,
        line: &MarkdownComposedLine,
        ordinal: usize,
    ) -> Result<Option<MarkdownComposedLinePart>> {
        let result = (|| {
            self.require_line(line)?;
            if ordinal > line.facts.parts {
                return Err(Error::InvalidRequest);
            }
            if ordinal == line.facts.parts {
                return Ok(None);
            }
            let record = ordinal
                .checked_mul(2)
                .and_then(|value| line.header.checked_add(1)?.checked_add(value))
                .ok_or(Error::Overflow)?;
            self.require(&self.authority)?;
            let mut part_records = [[0; 64]; 2];
            // 原Part头与meta本来都须完整读取后才能认证；合并同一128B范围，不预读其他part或增加缓存。
            self.payload
                .as_mut()
                .ok_or(Error::Poisoned)?
                .reader
                .read_batch(
                    record..record.checked_add(2).ok_or(Error::Overflow)?,
                    &mut part_records,
                )?;
            let [row, meta] = part_records;
            Self::part_header(line, &row, &meta)?;
            let item_record = usize_at(&meta, 40)?;
            self.require(&self.authority)?;
            let mut item_records = [[0; 64]; 3];
            // 只有已认证的Part才能给出原Directory位置；仍读精确三条192B，再共同解码、核覆盖与弱Font。
            self.payload
                .as_mut()
                .ok_or(Error::Poisoned)?
                .reader
                .read_batch(
                    item_record..item_record.checked_add(3).ok_or(Error::Overflow)?,
                    &mut item_records,
                )?;
            let item = decode_directory(item_records)?;
            self.decode_part(line, row, meta, item).map(Some)
        })();
        self.finish(result)
    }

    /// Authenticate both original part records before following the item pointer; both read paths share this format.
    fn part_header(line: &MarkdownComposedLine, row: &[u8; 64], meta: &[u8; 64]) -> Result<()> {
        if row[56] != PART
            || meta[56] != PART_META
            || row[57..].iter().any(|byte| *byte != 0)
            || meta[10..12].iter().any(|byte| *byte != 0)
            || u64_at(meta, 32) != number(line.facts.ordinal)?
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }

    /// Decode the five records already read; this conversion performs no additional IO.
    fn decode_part(
        &self,
        line: &MarkdownComposedLine,
        row: [u8; 64],
        meta: [u8; 64],
        item: Directory,
    ) -> Result<MarkdownComposedLinePart> {
        let font = FontKey {
            generation: u64_at(&meta, 0),
            slot: u16::from_le_bytes(meta[8..10].try_into().expect("固定2B字段")),
        };
        let direction = match meta[58] {
            0 => MarkdownShapingDirection::LeftToRight,
            1 => MarkdownShapingDirection::RightToLeft,
            _ => return Err(Error::InconsistentCoverage),
        };
        let facts = MarkdownComposedLinePartFacts {
            paragraph_inset_26_6: item.paragraph_inset_26_6,
            presentation: item.presentation,
            kind: super::super::MarkdownComposedLinePartKind::decode(meta[59])?,
            source_utf16: usize_at(&row, 0)?..usize_at(&row, 8)?,
            glyphs: usize_at(&row, 24)?,
            origin_x_26_6: i64_at(&row, 32),
            origin_y_26_6: i64_at(&row, 40),
            points: f32::from_le_bytes(meta[12..16].try_into().expect("固定4B字段")),
            x_advance_26_6: i64_at(&meta, 16),
            y_advance_26_6: i64_at(&meta, 24),
            level: meta[57],
            direction,
            script: meta[60..64].try_into().expect("固定4B字段"),
            visual_glyphs: usize_at(&meta, 48)?
                ..usize_at(&meta, 48)?
                    .checked_add(usize_at(&row, 24)?)
                    .ok_or(Error::Overflow)?,
        };
        let glyph_start = usize_at(&row, 16)?;
        let original_start = usize_at(&meta, 48)?;
        if facts.kind != item.kind
            || (facts.source_utf16.is_empty()
                && (facts.kind != super::super::MarkdownComposedLinePartKind::ParagraphBreak
                    || facts.source_utf16.end != self.range.end))
            || (facts.kind != super::super::MarkdownComposedLinePartKind::Glyphs
                && facts.glyphs != 0)
            || facts.source_utf16.start < line.facts.source_utf16.start
            || facts.source_utf16.end > line.facts.source_utf16.end
            || facts.source_utf16.start < item.facts.source_utf16.start
            || facts.source_utf16.end > item.facts.source_utf16.end
            || font != item.font
            || u64_at(&row, 48) != item.origin
            || facts.script != item.facts.script
            || facts.points != item.facts.points
            || !facts.points.is_finite()
            || (facts.points <= 0.0
                && facts.kind != super::super::MarkdownComposedLinePartKind::TableRow)
            || facts.level > 126
            || facts.level & 1 != u8::from(direction == MarkdownShapingDirection::RightToLeft)
            || original_start
                .checked_add(facts.glyphs)
                .is_none_or(|end| end > item.glyph_count)
            || item.glyph_start.checked_add(original_start) != Some(glyph_start)
        {
            return Err(Error::InconsistentCoverage);
        }
        let registration = self
            .payload
            .as_ref()
            .ok_or(Error::Poisoned)?
            .fonts
            .resolve(font)?
            .registration();
        Ok(MarkdownComposedLinePart {
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            registration,
            glyph_start,
            cluster_start: item.cluster_start,
            cluster_count: item.cluster_count,
            facts,
        })
    }
    /// 只读原glyph元数据；范围/字体/输出票错误均撤整候选，Err后的整个输出batch不可发布。
    pub fn read_batch(
        &mut self,
        part: &MarkdownComposedLinePart,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<()> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            let payload = self.payload.as_mut().ok_or(Error::Poisoned)?;
            payload.fonts.require_registration(&part.registration)?;
            if range.end.checked_sub(range.start) != Some(output.len())
                || output.len() > STEP
                || range.end > part.facts.glyphs
            {
                return Err(Error::InvalidRequest);
            }
            let start = part
                .glyph_start
                .checked_add(range.start)
                .ok_or(Error::Overflow)?;
            let end = part
                .glyph_start
                .checked_add(range.end)
                .ok_or(Error::Overflow)?;
            let mut rows = [[0; 64]; STEP];
            payload
                .reader
                .read_batch(start..end, &mut rows[..output.len()])?;
            for (row, slot) in rows.into_iter().zip(output.iter_mut()) {
                let glyph = decode_glyph(row)?;
                if !part
                    .facts
                    .source_utf16
                    .contains(&glyph.source_utf16_cluster)
                {
                    return Err(Error::InconsistentCoverage);
                }
                *slot = glyph;
            }
            payload.fonts.require_registration(&part.registration)?;
            Ok(())
        })();
        self.finish(result)
    }

    fn original_cluster(
        &mut self,
        part: &MarkdownComposedLinePart,
        offset: usize,
    ) -> Result<MarkdownGlyphCluster> {
        let (mut low, mut high) = (0, part.cluster_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let value = decode_cluster(
                self.row(
                    part.cluster_start
                        .checked_add(middle)
                        .ok_or(Error::Overflow)?,
                )?,
            )?;
            if offset < value.source_utf16.start {
                high = middle;
            } else if offset >= value.source_utf16.end {
                low = middle + 1;
            } else {
                if value.source_utf16.start < part.facts.source_utf16.start
                    || value.source_utf16.end > part.facts.source_utf16.end
                    || value.glyphs.start < part.facts.visual_glyphs.start
                    || value.glyphs.end > part.facts.visual_glyphs.end
                {
                    return Err(Error::InconsistentCoverage);
                }
                return Ok(value);
            }
        }
        Err(Error::InconsistentCoverage)
    }

    /// 原source/flags/advance不变，glyph与逻辑prefix均减去本part的真实起点，未扫描正文。
    pub fn cluster(
        &mut self,
        part: &MarkdownComposedLinePart,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            if offset < part.facts.source_utf16.start || offset > part.facts.source_utf16.end {
                return Err(Error::InvalidRequest);
            }
            if offset == part.facts.source_utf16.end {
                return Ok(MarkdownGlyphClusterAnswer::Known(None));
            }
            if part.facts.kind != super::super::MarkdownComposedLinePartKind::Glyphs {
                // 同LineEnd原控制范围只携源和真实进距，不假造可画glyph或ligature内部边界。
                return Ok(MarkdownGlyphClusterAnswer::Known(Some(
                    MarkdownGlyphCluster {
                        source_utf16: part.facts.source_utf16.clone(),
                        glyphs: 0..0,
                        missing_glyphs: 0,
                        flags: 0,
                        prefix_x_26_6: 0,
                        prefix_y_26_6: 0,
                        x_advance_26_6: part.facts.x_advance_26_6,
                        y_advance_26_6: 0,
                    },
                )));
            }
            let mut value = self.original_cluster(part, offset)?;
            let start = self.original_cluster(part, part.facts.source_utf16.start)?;
            // 前缀原本从item逻辑起点累计；直接暴露会把后续软行的caret错误平移到前面行。
            value.prefix_x_26_6 = value
                .prefix_x_26_6
                .checked_sub(start.prefix_x_26_6)
                .ok_or(Error::Overflow)?;
            value.prefix_y_26_6 = value
                .prefix_y_26_6
                .checked_sub(start.prefix_y_26_6)
                .ok_or(Error::Overflow)?;
            value.glyphs = value
                .glyphs
                .start
                .checked_sub(part.facts.visual_glyphs.start)
                .ok_or(Error::Overflow)?
                ..value
                    .glyphs
                    .end
                    .checked_sub(part.facts.visual_glyphs.start)
                    .ok_or(Error::Overflow)?;
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            Ok(MarkdownGlyphClusterAnswer::Known(Some(value)))
        })();
        self.finish(result)
    }

    pub fn close(mut self) -> Result<()> {
        let Some(payload) = self.payload.take() else {
            return Err(Error::Poisoned);
        };
        let result = payload.reader.close().map_err(Error::from);
        payload.source.health()?;
        result
    }
}
