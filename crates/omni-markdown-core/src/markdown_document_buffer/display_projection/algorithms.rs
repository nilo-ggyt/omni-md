//! 显示源的真实目录与原字节读取；游标只存数字、单条记录和原算法的有限续算状态。

use std::ops::Range;

use super::{DisplayProjection, DisplayProjectionRun};
use crate::MarkdownDocumentBufferError;
use crate::markdown_document_buffer::MarkdownUnicodeProperties;
use crate::markdown_document_buffer::frozen_text::{
    FrozenBidiParagraphCursor, FrozenBidiParagraphStep, FrozenPrintableAsciiSuffix,
    FrozenPrintableAsciiSuffixStep,
};
use crate::markdown_document_buffer::{MarkdownAsciiWordRun, MarkdownAsciiWordStep};
use unicode_bidi::BidiClass;

/// 每一步只读一条原索引；候选记录本身保留在游标内，不把二分搜索藏成一次 metadata。
struct RunSeek {
    lower: usize,
    upper: usize,
    position: usize,
    candidate: Option<(usize, DisplayProjectionRun)>,
}

impl RunSeek {
    fn new(projection: &DisplayProjection, position: usize) -> Self {
        Self {
            lower: 0,
            upper: projection.run_count(),
            position: position.min(projection.metrics().utf16_len.saturating_sub(1)),
            candidate: None,
        }
    }

    /// EOF 使用最后一个非空 run，空显示源由外层直接完成而不制造原索引记录。
    fn advance(
        &mut self,
        projection: &DisplayProjection,
        metadata: &mut usize,
        maximum_metadata: usize,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        while self.lower < self.upper && *metadata < maximum_metadata {
            let ordinal = self.lower + (self.upper - self.lower) / 2;
            let run = projection.run(ordinal)?;
            *metadata += 1;
            if run.display_utf16.end > self.position {
                self.upper = ordinal;
                self.candidate = Some((ordinal, run));
            } else {
                self.lower = ordinal + 1;
            }
        }
        Ok(self.lower == self.upper)
    }

    fn take(&mut self) -> (usize, DisplayProjectionRun) {
        self.candidate.take().expect("非空显示源二分命中原 run")
    }
}

/// 单个原 raw B 段游标负责标量边界；只有仍落在本 run 内的真实 B 才映回显示源。
pub(crate) struct ProjectionBidiParagraphCursor {
    position: usize,
    seek: Option<RunSeek>,
    raw: Option<FrozenBidiParagraphCursor>,
    run: Option<DisplayProjectionRun>,
    previous: Option<usize>,
    next: usize,
    start: usize,
    start_byte: usize,
    end: usize,
    end_byte: usize,
    content_end: usize,
    previous_ready: bool,
    next_ready: bool,
    complete: bool,
}

impl ProjectionBidiParagraphCursor {
    pub(crate) fn new(projection: &DisplayProjection, position: usize) -> Self {
        let metrics = projection.metrics();
        Self {
            position,
            seek: (metrics.utf16_len != 0).then(|| RunSeek::new(projection, position)),
            raw: None,
            run: None,
            previous: None,
            next: projection.run_count(),
            start: 0,
            start_byte: 0,
            end: metrics.utf16_len,
            end_byte: metrics.byte_len,
            content_end: metrics.utf16_len,
            previous_ready: false,
            next_ready: false,
            complete: metrics.utf16_len == 0,
        }
    }

    /// 目录、原树与真实 lease 共用调用方的三个额度；隐藏字符不能贡献显示段端点。
    pub(crate) fn advance(
        &mut self,
        projection: &DisplayProjection,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<FrozenBidiParagraphStep, MarkdownDocumentBufferError> {
        projection.health()?;
        let mut step = FrozenBidiParagraphStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            range: None,
            byte_range: None,
            content_end: self.content_end,
            maximum_borrow: 0,
        };
        if let Some(seek) = &mut self.seek {
            if !seek.advance(projection, &mut step.metadata, maximum_metadata)? {
                return Ok(step);
            }
            let (ordinal, run) = seek.take();
            let raw_position = run.raw_utf16.start + self.position - run.display_utf16.start;
            self.raw = Some(projection.raw().begin_bidi_paragraph(raw_position)?);
            self.previous = ordinal.checked_sub(1);
            self.next = ordinal + 1;
            self.run = Some(run);
            self.seek = None;
        }
        if let Some(raw) = &mut self.raw {
            let observed = projection.raw().advance_bidi_paragraph(
                raw,
                maximum_bytes - step.bytes,
                maximum_scalars - step.scalars,
                maximum_metadata - step.metadata,
            )?;
            step.bytes += observed.bytes;
            step.scalars += observed.scalars;
            step.metadata += observed.metadata;
            step.maximum_borrow = step.maximum_borrow.max(observed.maximum_borrow);
            if let Some(range) = observed.range {
                let bytes = observed.byte_range.expect("原 B 段同时交付真实字节端点");
                let run = self.run.as_ref().expect("已定位显示 run");
                if range.start > run.raw_utf16.start && range.start <= run.raw_utf16.end {
                    self.start = run.display_utf16.start + range.start - run.raw_utf16.start;
                    self.start_byte = run.display_bytes.start + bytes.start - run.raw_bytes.start;
                    self.previous_ready = true;
                }
                if observed.content_end >= run.raw_utf16.start
                    && observed.content_end < run.raw_utf16.end
                {
                    self.end = run.display_utf16.start + range.end - run.raw_utf16.start;
                    self.end_byte = run.display_bytes.start + bytes.end - run.raw_bytes.start;
                    self.content_end =
                        run.display_utf16.start + observed.content_end - run.raw_utf16.start;
                    self.next_ready = true;
                }
                // 仅真实显示全源的 B 集合为空，才可跳过相邻 run；raw 的集合不能替它签字。
                if projection.metrics().unicode_properties.bidi_classes()
                    & MarkdownUnicodeProperties::class_mask(BidiClass::B)
                    == 0
                {
                    self.previous_ready = true;
                    self.next_ready = true;
                }
                self.raw = None;
                self.run = None;
            } else {
                projection.health()?;
                return Ok(step);
            }
        }
        while !self.complete {
            if !self.previous_ready {
                let Some(ordinal) = self.previous else {
                    self.previous_ready = true;
                    continue;
                };
                if step.metadata == maximum_metadata {
                    break;
                }
                let run = projection.run(ordinal)?;
                step.metadata += 1;
                self.previous = ordinal.checked_sub(1);
                if let Some(terminator) = run.last_bidi {
                    self.start = terminator.utf16 + 1;
                    self.start_byte = terminator.byte_end;
                    self.previous_ready = true;
                }
                continue;
            }
            if !self.next_ready {
                if self.next == projection.run_count() {
                    self.next_ready = true;
                    continue;
                }
                if step.metadata == maximum_metadata {
                    break;
                }
                let run = projection.run(self.next)?;
                step.metadata += 1;
                self.next += 1;
                if let Some(terminator) = run.first_bidi {
                    self.end = terminator.utf16 + 1;
                    self.end_byte = terminator.byte_end;
                    self.content_end = terminator.utf16;
                    self.next_ready = true;
                }
                continue;
            }
            self.complete = true;
        }
        // 源与索引必须在所有实际借用结束后仍健康，才可发布 BOF/EOF 或真实 B 端点。
        projection.health()?;
        if self.complete {
            step.range = Some(self.start..self.end);
            step.byte_range = Some(self.start_byte..self.end_byte);
            step.content_end = self.content_end;
        }
        Ok(step)
    }
}

/// 两端各一条原 run；中间非 ASCII 计数来自输出标量的累计值，不顺扫巨型显示范围。
pub(crate) struct ProjectionPrintableAsciiSuffix {
    range: Range<usize>,
    start_seek: Option<RunSeek>,
    end_seek: Option<RunSeek>,
    start: Option<(usize, DisplayProjectionRun)>,
    end: Option<(usize, DisplayProjectionRun)>,
    raw: Option<FrozenPrintableAsciiSuffix>,
    trailing: Option<Range<usize>>,
    rejected: bool,
    answer: Option<bool>,
}

impl ProjectionPrintableAsciiSuffix {
    pub(crate) fn new(projection: &DisplayProjection, range: Range<usize>) -> Self {
        let nonempty = projection.metrics().utf16_len != 0;
        Self {
            start_seek: nonempty.then(|| RunSeek::new(projection, range.start)),
            end_seek: nonempty.then(|| {
                RunSeek::new(
                    projection,
                    range.end.saturating_sub(usize::from(!range.is_empty())),
                )
            }),
            range,
            start: None,
            end: None,
            raw: None,
            trailing: None,
            rejected: false,
            answer: (!nonempty).then_some(true),
        }
    }

    /// 原范围两端即使已有非 ASCII 反例也完整验证，不能用 false 掩盖低代理项端点。
    pub(crate) fn advance(
        &mut self,
        projection: &DisplayProjection,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<FrozenPrintableAsciiSuffixStep, MarkdownDocumentBufferError> {
        projection.health()?;
        let properties = projection.metrics().unicode_properties;
        let mut step = FrozenPrintableAsciiSuffixStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            maximum_borrow: 0,
            answer: self.answer,
            minimum: properties.printable_ascii_min,
            maximum: properties.printable_ascii_max,
        };
        if let Some(seek) = &mut self.start_seek {
            if !seek.advance(projection, &mut step.metadata, maximum_metadata)? {
                return Ok(step);
            }
            self.start = Some(seek.take());
            self.start_seek = None;
        }
        if let Some(seek) = &mut self.end_seek {
            if !seek.advance(projection, &mut step.metadata, maximum_metadata)? {
                return Ok(step);
            }
            self.end = Some(seek.take());
            self.end_seek = None;
            let (start_ordinal, start) = self.start.as_ref().expect("已定位真实起端 run");
            let (end_ordinal, end) = self.end.as_ref().expect("已定位真实末端 run");
            let raw_start = start.raw_utf16.start + self.range.start - start.display_utf16.start;
            let raw_end = end.raw_utf16.start + self.range.end - end.display_utf16.start;
            let first = if start_ordinal == end_ordinal {
                raw_start..raw_end
            } else {
                self.rejected = end.non_printable_before != start.non_printable_after;
                self.trailing = Some(end.raw_utf16.start..raw_end);
                raw_start..start.raw_utf16.end
            };
            self.raw = Some(projection.raw().begin_printable_ascii_suffix(first)?);
            self.start = None;
            self.end = None;
        }
        while let Some(raw) = &mut self.raw {
            let observed = projection.raw().advance_printable_ascii_suffix(
                raw,
                maximum_bytes - step.bytes,
                maximum_scalars - step.scalars,
                maximum_metadata - step.metadata,
            )?;
            step.bytes += observed.bytes;
            step.scalars += observed.scalars;
            step.metadata += observed.metadata;
            step.maximum_borrow = step.maximum_borrow.max(observed.maximum_borrow);
            let Some(answer) = observed.answer else {
                break;
            };
            self.rejected |= !answer;
            if let Some(trailing) = self.trailing.take() {
                self.raw = Some(projection.raw().begin_printable_ascii_suffix(trailing)?);
            } else {
                self.raw = None;
                self.answer = Some(!self.rejected);
            }
        }
        projection.health()?;
        step.answer = self.answer;
        Ok(step)
    }
}

/// 原 ASCII Word 规则跨删符 run 续算；每次只保留当前 raw 原树游标，不缓存显示正文。
pub(crate) struct ProjectionAsciiWordRun {
    position: usize,
    forward: bool,
    initialized: bool,
    seek: Option<RunSeek>,
    run: Option<(usize, DisplayProjectionRun)>,
    raw: Option<MarkdownAsciiWordRun>,
    complete: bool,
}

impl ProjectionAsciiWordRun {
    pub(crate) fn new(position: usize, forward: bool) -> Self {
        Self {
            position,
            forward,
            initialized: false,
            seek: None,
            run: None,
            raw: None,
            complete: false,
        }
    }

    /// run 之间的隐藏字符不属于显示输入，原 alnum 终止规则只解释已夹取的真实 raw 区间。
    pub(crate) fn advance(
        &mut self,
        projection: &DisplayProjection,
        authority: &crate::MarkdownLayoutTextAuthority,
        maximum_bytes: usize,
        maximum_scalars: usize,
        maximum_metadata: usize,
    ) -> Result<MarkdownAsciiWordStep, MarkdownDocumentBufferError> {
        projection.health()?;
        let mut step = MarkdownAsciiWordStep {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            position: self.position,
            complete: self.complete,
        };
        if !self.initialized {
            self.initialized = true;
            self.complete = if self.forward {
                self.position == projection.metrics().utf16_len
            } else {
                self.position == 0
            };
            if !self.complete {
                let position = if self.forward {
                    self.position
                } else {
                    self.position - 1
                };
                self.seek = Some(RunSeek::new(projection, position));
            }
        }
        while !self.complete {
            if let Some(seek) = &mut self.seek {
                if !seek.advance(projection, &mut step.metadata, maximum_metadata)? {
                    break;
                }
                self.run = Some(seek.take());
                self.seek = None;
            }
            if self.raw.is_none() {
                let (_, run) = self.run.as_ref().expect("实际显示 Word 已定位 run");
                let raw_position = run.raw_utf16.start + self.position - run.display_utf16.start;
                self.raw = Some(projection.raw().begin_scoped_ascii_word_run(
                    raw_position,
                    self.forward,
                    authority.clone(),
                    run.raw_utf16.clone(),
                )?);
            }
            let observed = projection.raw().advance_ascii_word_run(
                self.raw.as_mut().expect("已有原 raw Word 续算"),
                maximum_bytes - step.bytes,
                maximum_scalars - step.scalars,
                maximum_metadata - step.metadata,
            )?;
            step.bytes += observed.bytes;
            step.scalars += observed.scalars;
            step.metadata += observed.metadata;
            let (ordinal, run) = self.run.as_ref().expect("原 raw Word 属于当前 run");
            self.position = run.display_utf16.start + observed.position - run.raw_utf16.start;
            if !observed.complete {
                break;
            }
            let reached_edge = if self.forward {
                observed.position == run.raw_utf16.end
            } else {
                observed.position == run.raw_utf16.start
            };
            if !reached_edge {
                self.complete = true;
                break; // 当前实际显示字符已终止 alnum，不让后继 run 绕过它。
            }
            let next = if self.forward {
                (*ordinal + 1 < projection.run_count()).then_some(*ordinal + 1)
            } else {
                ordinal.checked_sub(1)
            };
            let Some(next) = next else {
                self.complete = true;
                break;
            };
            self.raw = None;
            if step.metadata == maximum_metadata {
                // 保留单个原目录待取位置；下步不重做二分，也不提前读取下一条。
                self.run = None;
                self.seek = Some(RunSeek {
                    lower: next,
                    upper: next + 1,
                    position: if self.forward {
                        self.position
                    } else {
                        self.position - 1
                    },
                    candidate: None,
                });
                break;
            }
            let run = projection.run(next)?;
            step.metadata += 1;
            self.run = Some((next, run));
        }
        projection.health()?;
        step.position = self.position;
        step.complete = self.complete;
        Ok(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_document_buffer::display_projection::DisplayProjectionBuilder;
    use crate::{
        MarkdownDocumentBuffer, MarkdownLayoutTextSource, MarkdownPresentationStyles,
        MarkdownTableCellDisplayScalar, MarkdownTextSnapshot,
    };
    use std::sync::Arc;

    /// 只用有限合成样本装配真实原 mapping 与真实匿名索引，省略标量不进入显示聚合。
    fn projection(
        text: &str,
        keep: impl Fn(char) -> bool,
    ) -> (Arc<DisplayProjection>, crate::MarkdownLayoutTextAuthority) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实显示字节.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let mut builder = DisplayProjectionBuilder::new(document.text_snapshot()).unwrap();
        let mut raw_utf16 = 0;
        let mut display_bytes = 0;
        let mut display_utf16 = 0;
        for (raw_byte, scalar) in text.char_indices() {
            if keep(scalar) {
                // 交错样式强制产生多个真实 run，验证删符断点不是 raw 的连续字符范围。
                let styles = if display_utf16 % 2 == 0 {
                    MarkdownPresentationStyles::STRONG
                } else {
                    MarkdownPresentationStyles::EMPHASIS
                };
                builder
                    .append(MarkdownTableCellDisplayScalar {
                        scalar,
                        display_bytes: display_bytes..display_bytes + scalar.len_utf8(),
                        display_utf16: display_utf16..display_utf16 + scalar.len_utf16(),
                        raw_bytes: raw_byte..raw_byte + scalar.len_utf8(),
                        raw_utf16: raw_utf16..raw_utf16 + scalar.len_utf16(),
                        canonical_utf16: raw_utf16..raw_utf16 + scalar.len_utf16(),
                        styles,
                    })
                    .unwrap();
                display_bytes += scalar.len_utf8();
                display_utf16 += scalar.len_utf16();
            }
            raw_utf16 += scalar.len_utf16();
        }
        let projection = builder.finish().unwrap();
        drop(document);
        drop(source);
        drop(directory);
        (projection, authority)
    }

    /// 每个量子只有一字节、一标量、一条目录；真实源错误和代理项不能被部分答案盖掉。
    fn bidi(
        projection: &Arc<DisplayProjection>,
        position: usize,
    ) -> Result<(Range<usize>, Range<usize>, usize), MarkdownDocumentBufferError> {
        let snapshot = MarkdownTextSnapshot::from_display_projection(Arc::clone(projection), 0);
        let mut cursor = snapshot.begin_bidi_paragraph(position)?;
        for _ in 0..10000 {
            let step = snapshot.advance_bidi_paragraph(&mut cursor, 1, 1, 1)?;
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            assert!(step.maximum_borrow <= 1);
            if let Some(range) = step.range {
                return Ok((range, step.byte_range.unwrap(), step.content_end));
            }
        }
        panic!("有限真实显示样本应完成 B 段查询")
    }

    #[test]
    fn visible_bidi_boundaries_ignore_hidden_separators_and_keep_crlf_and_surrogate_contract() {
        let (projection, _) = projection("隐\u{2029}a**b**\r\nc😀z", |scalar| {
            !matches!(scalar, '隐' | '\u{2029}' | '*')
        });
        assert_eq!(bidi(&projection, 0), Ok((0..3, 0..3, 2)));
        assert_eq!(bidi(&projection, 2), Ok((0..3, 0..3, 2)));
        assert_eq!(bidi(&projection, 3), Ok((3..4, 3..4, 3)));
        assert_eq!(bidi(&projection, 4), Ok((4..8, 4..10, 8)));
        assert_eq!(bidi(&projection, 8), Ok((4..8, 4..10, 8)));
        assert_eq!(
            bidi(&projection, 6),
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        );
        let observer = projection.resource_observer();
        assert!(observer.snapshot().file_open);
        drop(projection);
        let resources = observer.snapshot();
        assert!(!resources.file_open && resources.buffer_bytes == 0 && !resources.cleanup_failed);
    }

    #[test]
    fn ascii_word_crosses_removed_markers_but_not_actual_space_after_mixed_raw_prefix() {
        let (projection, authority) = projection("😀אבa**b**c z\u{2029}", |scalar| {
            scalar.is_ascii() && scalar != '*'
        });
        let snapshot = MarkdownTextSnapshot::from_display_projection(Arc::clone(&projection), 0);
        for (position, forward, expected) in
            [(0, true, 3), (3, false, 0), (4, true, 5), (5, false, 4)]
        {
            let mut cursor = snapshot
                .begin_ascii_word_run(position, forward, authority.clone())
                .unwrap();
            let mut result = None;
            for _ in 0..10000 {
                let step = snapshot
                    .advance_ascii_word_run(&mut cursor, 1, 1, 1)
                    .unwrap();
                assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
                if step.complete {
                    result = Some(step.position);
                    break;
                }
            }
            assert_eq!(result, Some(expected));
        }
    }

    #[test]
    fn display_ascii_suffix_validates_both_unicode_endpoints_after_real_negative_fact() {
        let (projection, _) = projection("a**😀**b中c", |scalar| scalar != '*');
        let snapshot = MarkdownTextSnapshot::from_display_projection(Arc::clone(&projection), 0);
        for (range, expected) in [
            (0..1, Ok(true)),
            (1..3, Ok(false)),
            (3..4, Ok(true)),
            (4..6, Ok(false)),
            (0..2, Err(MarkdownDocumentBufferError::NotUnicodeBoundary)),
            (2..2, Err(MarkdownDocumentBufferError::NotUnicodeBoundary)),
            (1..2, Err(MarkdownDocumentBufferError::NotUnicodeBoundary)),
        ] {
            let mut cursor = snapshot.begin_printable_ascii_suffix(range).unwrap();
            let mut result = None;
            for _ in 0..10000 {
                match snapshot.advance_printable_ascii_suffix(&mut cursor, 1, 1, 1) {
                    Ok(step) => {
                        assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
                        assert!(step.maximum_borrow <= 1);
                        if let Some(answer) = step.answer {
                            result = Some(Ok(answer));
                            break;
                        }
                    }
                    Err(error) => {
                        result = Some(Err(error));
                        break;
                    }
                }
            }
            assert_eq!(result, Some(expected));
        }
    }
}
