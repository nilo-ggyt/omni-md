//! 显示坐标的只读来源：原 piece 字节继续共享，省略与样式只进入同一匿名分页目录。

use std::sync::atomic::{AtomicI32, Ordering};

use super::*;
use crate::markdown_layout_index::{
    LayoutIndexError, LayoutIndexObserver, LayoutIndexReader, PagedLayoutIndex,
};
use crate::unicode_bidi_data::UnicodeBidiData;
use crate::{MarkdownPresentationStyles, MarkdownTableCellDisplayScalar};
use unicode_bidi::{BidiClass, BidiDataSource};

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) mod algorithms;

const RECORD_BYTES: usize = 256;
const UNKNOWN_FAILURE: i32 = i32::MIN;

#[derive(Clone, Copy)]
/// 实际 BMP 终止标量的显示起点和真实 UTF8 尾；省略字符不进入这些摘要。
pub(crate) struct DisplayProjectionTerminator {
    pub(crate) utf16: usize,
    pub(crate) byte_end: usize,
}

#[derive(Clone)]
/// 一条目录只描述同样式、两种编码各连续的真实原源区间，不保存正文。
pub(crate) struct DisplayProjectionRun {
    pub(crate) display_bytes: Range<usize>,
    pub(crate) display_utf16: Range<usize>,
    pub(crate) raw_bytes: Range<usize>,
    pub(crate) raw_utf16: Range<usize>,
    pub(crate) styles: MarkdownPresentationStyles,
    pub(crate) non_printable_before: usize,
    pub(crate) non_printable_after: usize,
    pub(crate) first_bidi: Option<DisplayProjectionTerminator>,
    pub(crate) last_bidi: Option<DisplayProjectionTerminator>,
    pub(crate) first_text_terminator: Option<DisplayProjectionTerminator>,
    pub(crate) last_text_terminator: Option<DisplayProjectionTerminator>,
}

impl DisplayProjectionRun {
    /// 编码固定 record；没有指针、Native 值、正文或可变外部结构进入匿名目录。
    fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut record = [0; RECORD_BYTES];
        let mut put = |slot: usize, value: usize| {
            record[slot * 8..slot * 8 + 8].copy_from_slice(&(value as u64).to_le_bytes());
        };
        for (slot, value) in [
            self.display_bytes.start,
            self.display_bytes.end,
            self.display_utf16.start,
            self.display_utf16.end,
            self.raw_bytes.start,
            self.raw_bytes.end,
            self.raw_utf16.start,
            self.raw_utf16.end,
            self.styles.bits() as usize,
            self.non_printable_before,
            self.non_printable_after,
        ]
        .into_iter()
        .enumerate()
        {
            put(slot, value);
        }
        for (index, value) in [
            self.first_bidi,
            self.last_bidi,
            self.first_text_terminator,
            self.last_text_terminator,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(value) = value {
                put(11 + index * 2, value.utf16 + 1); // 零只表示不存在，不与真实显示起点混淆。
                put(12 + index * 2, value.byte_end);
            }
        }
        record
    }

    /// 文件实际短读由原 Reader 拒绝；坐标和样式恢复错误不能静默变成默认显示内容。
    fn decode(record: [u8; RECORD_BYTES]) -> Result<Self, MarkdownDocumentBufferError> {
        let get = |slot: usize| -> Result<usize, MarkdownDocumentBufferError> {
            usize::try_from(u64::from_le_bytes(
                record[slot * 8..slot * 8 + 8]
                    .try_into()
                    .expect("固定 record 字段始终为八字节"),
            ))
            .map_err(|_| MarkdownDocumentBufferError::DisplayStorageUnavailable(None))
        };
        let terminator = |index: usize| -> Result<
            Option<DisplayProjectionTerminator>,
            MarkdownDocumentBufferError,
        > {
            let value = get(11 + index * 2)?;
            Ok(if value == 0 {
                None
            } else {
                Some(DisplayProjectionTerminator {
                    utf16: value - 1,
                    byte_end: get(12 + index * 2)?,
                })
            })
        };
        Ok(Self {
            display_bytes: get(0)?..get(1)?,
            display_utf16: get(2)?..get(3)?,
            raw_bytes: get(4)?..get(5)?,
            raw_utf16: get(6)?..get(7)?,
            styles: u32::try_from(get(8)?)
                .ok()
                .and_then(MarkdownPresentationStyles::from_bits)
                .ok_or(MarkdownDocumentBufferError::DisplayStorageUnavailable(None))?,
            non_printable_before: get(9)?,
            non_printable_after: get(10)?,
            first_bidi: terminator(0)?,
            last_bidi: terminator(1)?,
            first_text_terminator: terminator(2)?,
            last_text_terminator: terminator(3)?,
        })
    }
}

/// 只保一个待合并 run 和原 16 KiB 追加页；目录长度不会变为常驻 Vec 或 AVL 节点。
pub(crate) struct DisplayProjectionBuilder {
    raw: MarkdownTextSnapshot,
    index: PagedLayoutIndex<RECORD_BYTES>,
    pending: Option<DisplayProjectionRun>,
    metrics: MarkdownDocumentMetrics,
    non_printable: usize,
}

impl DisplayProjectionBuilder {
    /// 观察这个尚未封存的真实writer；观察者不保活raw快照、目录FD或追加页。
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        self.index.resource_observer()
    }

    /// 只记录真正追加的固定 metadata 量，供原 caller 汇总本步逻辑工作，不是内存峰值。
    pub(crate) fn record_bytes(&self) -> usize {
        self.index.len() * RECORD_BYTES
    }
    /// raw 已由原格工厂封闭；构建器不接活动 Doc、裸范围或第二份文本。
    pub(crate) fn new(raw: MarkdownTextSnapshot) -> Result<Self, MarkdownDocumentBufferError> {
        raw.original_backing_health()?;
        Ok(Self {
            raw,
            index: PagedLayoutIndex::new().map_err(index_error)?,
            pending: None,
            metrics: MarkdownDocumentMetrics::default(),
            non_printable: 0,
        })
    }

    /// 每个已转换标量只更新原显示摘要和一个来源 run；没有正文追加文件或整格输出字符串。
    pub(crate) fn append(
        &mut self,
        scalar: MarkdownTableCellDisplayScalar,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        let before = self.non_printable;
        self.non_printable += usize::from(!(' '..='~').contains(&scalar.scalar));
        let terminal = DisplayProjectionTerminator {
            utf16: scalar.display_utf16.start,
            byte_end: scalar.display_bytes.end,
        };
        let bidi = (UnicodeBidiData.bidi_class(scalar.scalar) == BidiClass::B).then_some(terminal);
        let checking = matches!(scalar.scalar, '\r' | '\n' | '\u{2029}').then_some(terminal);
        if let Some(run) = self.pending.as_mut()
            && run.display_bytes.end == scalar.display_bytes.start
            && run.display_utf16.end == scalar.display_utf16.start
            && run.raw_bytes.end == scalar.raw_bytes.start
            && run.raw_utf16.end == scalar.raw_utf16.start
            && run.styles == scalar.styles
        {
            run.display_bytes.end = scalar.display_bytes.end;
            run.display_utf16.end = scalar.display_utf16.end;
            run.raw_bytes.end = scalar.raw_bytes.end;
            run.raw_utf16.end = scalar.raw_utf16.end;
            run.non_printable_after = self.non_printable;
            run.first_bidi = run.first_bidi.or(bidi);
            run.last_bidi = bidi.or(run.last_bidi);
            run.first_text_terminator = run.first_text_terminator.or(checking);
            run.last_text_terminator = checking.or(run.last_text_terminator);
        } else {
            self.flush()?;
            self.pending = Some(DisplayProjectionRun {
                display_bytes: scalar.display_bytes,
                display_utf16: scalar.display_utf16,
                raw_bytes: scalar.raw_bytes,
                raw_utf16: scalar.raw_utf16,
                styles: scalar.styles,
                non_printable_before: before,
                non_printable_after: self.non_printable,
                first_bidi: bidi,
                last_bidi: bidi,
                first_text_terminator: checking,
                last_text_terminator: checking,
            });
        }
        let mut encoded = [0; 4];
        self.metrics = MarkdownDocumentMetrics::joined(
            self.metrics,
            MarkdownDocumentMetrics::from_text(scalar.scalar.encode_utf8(&mut encoded)),
        );
        self.raw.original_backing_health()?;
        Ok(())
    }

    /// 只有来源或样式断点才追加一个固定 record，先前同 inode 字节永不改写。
    fn flush(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(run) = self.pending.take() {
            self.index.append(run.encode()).map_err(index_error)?;
        }
        Ok(())
    }

    /// 全部原输入和语义已完成后才封存；空显示不为不存在的目录读取创建 FD。
    pub(crate) fn finish(mut self) -> Result<Arc<DisplayProjection>, MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        self.flush()?;
        let count = self.index.len();
        let observer = self.index.resource_observer();
        let reader = if count == 0 {
            self.index.close().map_err(index_error)?;
            None
        } else {
            Some(self.index.into_reader().map_err(index_error)?)
        };
        self.raw.original_backing_health()?;
        Ok(Arc::new(DisplayProjection {
            raw: self.raw,
            reader: Mutex::new(reader),
            metrics: self.metrics,
            count,
            observer,
            first_failure: AtomicI32::new(0),
        }))
    }
}

/// 原 raw 冻结 pieces 和同一封存目录是唯一强资源；显示字节只在有限 visitor 回调里借用。
pub(crate) struct DisplayProjection {
    raw: MarkdownTextSnapshot,
    reader: Mutex<Option<LayoutIndexReader<RECORD_BYTES>>>,
    metrics: MarkdownDocumentMetrics,
    count: usize,
    observer: LayoutIndexObserver,
    first_failure: AtomicI32,
}

/// 只借同一封存目录，保一个待交记录；显示端已夹到本次真实UTF16额度，
/// 不会为了合并相邻样式先遍历巨格全部run，也不新增FD、正文或坐标索引。
pub(crate) struct DisplayProjectionVisibleRawRanges<'a> {
    projection: &'a DisplayProjection,
    display: Range<usize>,
    next: usize,
    pending: Option<DisplayProjectionRun>,
    limited: bool,
    done: bool,
}

impl DisplayProjectionVisibleRawRanges<'_> {
    pub(crate) fn source_limited(&self) -> bool {
        self.limited
    }
}

impl Iterator for DisplayProjectionVisibleRawRanges<'_> {
    type Item = Result<Range<usize>, MarkdownDocumentBufferError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| {
            self.projection.health()?;
            let Some(mut run) = self
                .pending
                .take()
                .map(Ok)
                .or_else(|| {
                    (self.next < self.projection.count).then(|| {
                        let record = self.projection.run(self.next);
                        self.next += 1;
                        record
                    })
                })
                .transpose()?
            else {
                self.done = true;
                return Ok(None);
            };
            let start = self.display.start.max(run.display_utf16.start);
            let end = self.display.end.min(run.display_utf16.end);
            if start >= end {
                self.done = true;
                return Ok(None);
            }
            let mut raw = run.raw_utf16.start + start - run.display_utf16.start
                ..run.raw_utf16.start + end - run.display_utf16.start;
            // 样式断点不妨碍完整字面查询跨字体；真实raw gap才结束一个可见连续域。
            // 每条record至少含一UTF16单元，所以本循环也受构造时的同一输入额度约束。
            while run.display_utf16.end < self.display.end && self.next < self.projection.count {
                let next = self.projection.run(self.next)?;
                self.next += 1;
                if next.raw_utf16.start != raw.end {
                    self.pending = Some(next);
                    break;
                }
                raw.end = next.raw_utf16.start + self.display.end.min(next.display_utf16.end)
                    - next.display_utf16.start;
                run = next;
            }
            self.done = run.display_utf16.end >= self.display.end;
            self.projection.health()?;
            Ok(Some(raw))
        })();
        match result {
            Ok(value) => value.map(Ok),
            Err(cause) => {
                self.done = true; // 首错只交一次；不能在错误后沿另一个正确record继续给部分成功。
                Some(Err(cause))
            }
        }
    }
}

impl DisplayProjection {
    /// 原可见display端点先核真实标量，再用调用方剩余UTF16额度夹取末端。
    /// 首run只二分一次，后续由同一借用游标逐记录推进；隐藏raw gap始终不被包络补齐。
    pub(crate) fn visible_raw_ranges(
        &self,
        display: Range<usize>,
        maximum_units: usize,
    ) -> Result<DisplayProjectionVisibleRawRanges<'_>, MarkdownDocumentBufferError> {
        self.health()?;
        if display.start > display.end || display.end > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.byte_offset_for_utf16(display.start)?;
        self.byte_offset_for_utf16(display.end)?;
        let mut end = display.start.saturating_add(maximum_units).min(display.end);
        match self.byte_offset_for_utf16(end) {
            Ok(_) => {}
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary) if end > display.start => {
                end -= 1; // UTF16上限只可能切一对代理；只退这一个单元，不吞任何真实Source错误。
                self.byte_offset_for_utf16(end)?;
            }
            Err(cause) => return Err(cause),
        }
        let next = if display.start == end {
            self.count
        } else {
            self.locate(display.start, 1)?
        };
        Ok(DisplayProjectionVisibleRawRanges {
            projection: self,
            display: display.start..end,
            next,
            pending: None,
            limited: end < display.end,
            done: display.start == end,
        })
    }

    /// 原算法只沿该冻结 raw 根读取，每次借用仍归原 4 KiB 资源边界。
    pub(crate) fn raw(&self) -> &MarkdownTextSnapshot {
        &self.raw
    }
    /// 转换时逐个可见 scalar 累计，不能用 raw 树摘要替代显示语义。
    pub(super) fn metrics(&self) -> MarkdownDocumentMetrics {
        self.metrics
    }
    /// 目录记录数量仅用于有限二分和逐条预算，不让正文长度变成常驻分配。
    pub(crate) fn run_count(&self) -> usize {
        self.count
    }
    /// 汇总封存时实际追加的记录量；最后 pending run 也在其中。
    pub(crate) fn record_bytes(&self) -> usize {
        self.count * RECORD_BYTES
    }
    /// 原子观察者不会保活目录 Reader 或原字节映射。
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        self.observer.clone()
    }

    /// 原字节错误始终先于派生目录失败；后者一旦撤销便不能从其他正确 record 恢复成功。
    pub(crate) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        let failure = self.first_failure.load(Ordering::Acquire);
        if failure != 0 {
            return Err(MarkdownDocumentBufferError::DisplayStorageUnavailable(
                if failure == UNKNOWN_FAILURE {
                    None
                } else {
                    Some(failure)
                },
            ));
        }
        let state = self.observer.snapshot();
        if state.cleanup_failed || state.reader_failed {
            return Err(self.fail(MarkdownDocumentBufferError::DisplayStorageUnavailable(None)));
        }
        Ok(())
    }

    /// 保存本来源的实际派生首错；不把普通范围错误升级为 I/O，也不覆盖原 raw errno。
    fn fail(&self, error: MarkdownDocumentBufferError) -> MarkdownDocumentBufferError {
        if let MarkdownDocumentBufferError::DisplayStorageUnavailable(errno) = error {
            let _ = self.first_failure.compare_exchange(
                0,
                errno.unwrap_or(UNKNOWN_FAILURE),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.raw.original_backing_health().err().unwrap_or(error)
    }

    /// 一次只读一个原 256-byte record；没有页预读或另一个源、正文与索引缓存。
    pub(crate) fn run(
        &self,
        ordinal: usize,
    ) -> Result<DisplayProjectionRun, MarkdownDocumentBufferError> {
        self.health()?;
        if ordinal >= self.count {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let result = (|| {
            let mut reader = self
                .reader
                .lock()
                .map_err(|_| MarkdownDocumentBufferError::DisplayStorageUnavailable(None))?;
            let record = reader
                .as_mut()
                .expect("非空封存显示目录必须拥有原 Reader")
                .read(ordinal)
                .map_err(index_error)?;
            DisplayProjectionRun::decode(record)
        })();
        self.raw.original_backing_health()?;
        result.map_err(|error| self.fail(error))
    }

    /// 返回第一个尾端严格大于位置的 run；真实 EOF 是 count，不拿相邻格补位置。
    fn locate(&self, position: usize, axis: u8) -> Result<usize, MarkdownDocumentBufferError> {
        let mut low = 0;
        let mut high = self.count;
        while low != high {
            let middle = low + (high - low) / 2;
            let run = self.run(middle)?;
            let end = match axis {
                0 => run.display_bytes.end,
                1 => run.display_utf16.end,
                _ => run.raw_utf16.end,
            };
            if end <= position {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    /// 显示 UTF16 必须是实际标量边界；raw run 内的线性关系不会把低代理项补成合法 caret。
    pub(crate) fn byte_offset_for_utf16(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        if position > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // 封存显示源的零点属于显示字节域；即使原文前面删过语法，它也恒为字节零。
        // 健康检查仍先完成，不能为每次 Native 回调前后核源而重复读取首条磁盘目录。
        if position == 0 {
            return Ok(0);
        }
        if position == self.metrics.utf16_len {
            return Ok(self.metrics.byte_len);
        }
        let run = self.run(self.locate(position, 1)?)?;
        let raw = self
            .raw
            .byte_offset_for_utf16(run.raw_utf16.start + position - run.display_utf16.start)?;
        Ok(run.display_bytes.start + raw - run.raw_bytes.start)
    }

    /// 字节边界仍由同一原 raw Snapshot 解码确认；隐藏 gap 只能改变目录偏移，不能吞掉标量。
    pub(crate) fn utf16_offset_for_byte(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        if position > self.metrics.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if position == self.metrics.byte_len {
            return Ok(self.metrics.utf16_len);
        }
        let run = self.run(self.locate(position, 0)?)?;
        let raw = self
            .raw
            .utf16_offset_for_byte(run.raw_bytes.start + position - run.display_bytes.start)?;
        Ok(run.display_utf16.start + raw - run.raw_utf16.start)
    }

    /// 多个显示片段直接借原 pieces；调用者可在自己的至多 4 KiB 形状缓冲里按需拼接。
    pub(crate) fn visit_byte_range(
        &self,
        range: Range<usize>,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end || range.end > self.metrics.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() {
            return Ok(());
        }
        let mut ordinal = self.locate(range.start, 0)?;
        let mut position = range.start;
        while position != range.end {
            let run = self.run(ordinal)?;
            let end = range.end.min(run.display_bytes.end);
            let start = run.raw_bytes.start + position - run.display_bytes.start;
            let result = self
                .raw
                // 动态借用在 raw/显示派发边界收敛类型，不把递归转发实例化成不断增长的 &mut 层。
                .visit_byte_range(start..start + end - position, &mut *visit);
            self.health()?;
            result?;
            position = end;
            ordinal += 1;
        }
        Ok(())
    }

    /// NSString 读取保留半代理单元合同，遍历原实际 run，既不构造完整 UTF16 也不保另一份 cache。
    pub(crate) fn copy_utf16_code_units(
        &self,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        let end = offset
            .checked_add(destination.len())
            .filter(|end| *end <= self.metrics.utf16_len)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if destination.is_empty() {
            return Ok(());
        }
        let mut ordinal = self.locate(offset, 1)?;
        let mut position = offset;
        while position != end {
            let run = self.run(ordinal)?;
            let count = (end - position).min(run.display_utf16.end - position);
            let start = run.raw_utf16.start + position - run.display_utf16.start;
            self.raw.copy_utf16_code_units(
                start,
                &mut destination[position - offset..position - offset + count],
            )?;
            position += count;
            ordinal += 1;
        }
        self.health()
    }

    /// 前缀累计排除完整中间 run；仅两端部分 run 解码原有限叶，两个标量端点均验证。
    pub(crate) fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(range.start)?;
        self.byte_offset_for_utf16(range.end)?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() || self.metrics.all_printable_ascii.is_printable() {
            return Ok(true);
        }
        let first_index = self.locate(range.start, 1)?;
        let last_index = self.locate(range.end - 1, 1)?;
        let first = self.run(first_index)?;
        let last = self.run(last_index)?;
        let first_end = range.end.min(first.display_utf16.end);
        let first_raw = first.raw_utf16.start + range.start - first.display_utf16.start;
        let beginning = self
            .raw
            .is_printable_ascii_range(first_raw..first_raw + first_end - range.start)?;
        if first_index == last_index {
            return Ok(beginning);
        }
        let last_end = last.raw_utf16.start + range.end - last.display_utf16.start;
        let ending = self
            .raw
            .is_printable_ascii_range(last.raw_utf16.start..last_end)?;
        self.health()?;
        Ok(beginning && ending && first.non_printable_after == last.non_printable_before)
    }

    /// 文本检查允许半代理单位；只定位两端实际可见终止符，不让 raw 隐藏分隔符切断显示段落。
    pub(crate) fn text_checking_paragraph_range(
        &self,
        request: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        self.health()?;
        if request.start > request.end || request.end > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if self.metrics.text_checking_terminators == 0 {
            return Ok(0..self.metrics.utf16_len);
        }
        let start = self
            .previous_text_terminator(request.start)?
            .map_or(0, |position| position + 1);
        let anchor = if request.is_empty() {
            request.end
        } else {
            request.end - 1
        };
        let Some(terminal) = self.next_text_terminator(anchor)? else {
            return Ok(start..self.metrics.utf16_len);
        };
        let mut end = terminal + 1;
        let mut unit = [0];
        self.copy_utf16_code_units(terminal, &mut unit)?;
        if unit[0] == 13 && end < self.metrics.utf16_len {
            self.copy_utf16_code_units(end, &mut unit)?;
            if unit[0] == 10 {
                end += 1;
            } // 合并的是实际显示 CRLF，不能沿 raw 坐标越过删除区。
        }
        self.health()?;
        Ok(start..end)
    }

    /// 完整中间 run 只看其真实尾摘要，命中端点 run 才沿原同长 raw 区间找最后终止符。
    fn previous_text_terminator(
        &self,
        before: usize,
    ) -> Result<Option<usize>, MarkdownDocumentBufferError> {
        if before == 0 || self.count == 0 {
            return Ok(None);
        }
        let mut ordinal = self.locate(before - 1, 1)?;
        loop {
            let run = self.run(ordinal)?;
            if let Some(last) = run.last_text_terminator {
                if last.utf16 < before {
                    return Ok(Some(last.utf16));
                }
                if run
                    .first_text_terminator
                    .is_some_and(|first| first.utf16 < before)
                {
                    let raw_at = run.raw_utf16.start + before - run.display_utf16.start;
                    let range = self.raw.text_checking_paragraph_range(raw_at..raw_at)?;
                    if range.start > run.raw_utf16.start {
                        return Ok(Some(
                            run.display_utf16.start + range.start - 1 - run.raw_utf16.start,
                        ));
                    }
                }
            }
            let Some(previous) = ordinal.checked_sub(1) else {
                return Ok(None);
            };
            ordinal = previous;
        }
    }

    /// raw 段落 API 可合并 CRLF；只接受当前 run 中真实首终止符，省略外的结果继续查下一显示 run。
    fn next_text_terminator(
        &self,
        at: usize,
    ) -> Result<Option<usize>, MarkdownDocumentBufferError> {
        if at == self.metrics.utf16_len || self.count == 0 {
            return Ok(None);
        }
        let mut ordinal = self.locate(at, 1)?;
        while ordinal < self.count {
            let run = self.run(ordinal)?;
            if let Some(first) = run.first_text_terminator {
                if first.utf16 >= at {
                    return Ok(Some(first.utf16));
                }
                if run
                    .last_text_terminator
                    .is_some_and(|last| last.utf16 >= at)
                {
                    let raw_at = run.raw_utf16.start + at - run.display_utf16.start;
                    let range = self.raw.text_checking_paragraph_range(raw_at..raw_at)?;
                    let mut terminal = range.end - 1;
                    if terminal > raw_at {
                        let mut pair = [0; 2];
                        self.raw.copy_utf16_code_units(terminal - 1, &mut pair)?;
                        if pair == [13, 10] {
                            terminal -= 1;
                        }
                    }
                    if run.raw_utf16.contains(&terminal) {
                        return Ok(Some(
                            run.display_utf16.start + terminal - run.raw_utf16.start,
                        ));
                    }
                }
            }
            ordinal += 1;
        }
        Ok(None)
    }

    /// 显示边界两侧可跨隐藏语法；范围末端取上游可见尾，不能吞掉其后 URL 或闭合标记。
    pub(crate) fn display_to_raw(
        &self,
        position: usize,
        downstream: bool,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(position)?;
        if self.count == 0 {
            return Ok(0);
        }
        if position == self.metrics.utf16_len {
            return Ok(self.run(self.count - 1)?.raw_utf16.end);
        }
        // 先核 position 的真实 scalar 边界；position-1 只选择目录 run，不把代理中点当可读端点。
        let lookup = if !downstream && position != 0 {
            position - 1
        } else {
            position
        };
        let run = self.run(self.locate(lookup, 1)?)?;
        Ok(run.raw_utf16.start + position - run.display_utf16.start)
    }

    /// 原源隐藏 gap 的亲和由真实调用者明确给出，不能将有向选择猜成 display range 的尾端。
    pub(crate) fn raw_to_display(
        &self,
        position: usize,
        downstream: bool,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        self.raw.byte_offset_for_utf16(position)?;
        if self.count == 0 {
            return Ok(0);
        }
        let ordinal = self.locate(position, 2)?;
        if ordinal == self.count {
            return Ok(self.metrics.utf16_len);
        }
        let run = self.run(ordinal)?;
        if position >= run.raw_utf16.start {
            return Ok(run.display_utf16.start + position - run.raw_utf16.start);
        }
        if downstream || ordinal == 0 {
            Ok(run.display_utf16.start)
        } else {
            Ok(self.run(ordinal - 1)?.display_utf16.end)
        }
    }

    /// 样式查询只返回同一封存目录实际输出的 run；EOF 和空格没有可读取字符样式。
    pub(crate) fn styles_at(
        &self,
        position: usize,
    ) -> Result<Option<crate::MarkdownPresentationQuery>, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(position)?;
        if position == self.metrics.utf16_len {
            return Ok(None);
        }
        let run = self.run(self.locate(position, 1)?)?;
        Ok(Some(crate::MarkdownPresentationQuery {
            utf16_range: run.display_utf16,
            styles: run.styles,
        }))
    }
}

/// 派生目录的真实系统错误保留 errno；锁损坏和已撤销索引保持 None，不伪造系统原因。
fn index_error(error: LayoutIndexError) -> MarkdownDocumentBufferError {
    MarkdownDocumentBufferError::DisplayStorageUnavailable(match error {
        LayoutIndexError::Io(error) => error.raw_os_error(),
        _ => None,
    })
}
