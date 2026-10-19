//! Persistent balanced piece-tree storage, coordinate indexes, and streaming traversal.

use super::*;
use std::num::NonZeroUsize;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

#[cfg(feature = "paged-shaping-frontier")]
pub(super) mod ascii_word;
mod original_span;
#[cfg(feature = "s2-table-window-prototype")]
pub(super) use original_span::ORIGINAL_SPAN_METADATA_STEPS;
pub(super) use original_span::{ORIGINAL_SPAN_PIECES, OriginalSpan, OriginalSpanBuilder};
mod composition_allocation;
pub(super) use composition_allocation::{
    CompositionAllocation, CompositionHeapCow, HeapAllocation,
};

/// Match the preserved editor's Unicode 17 `General_Category=L|N` word-run membership.
pub(super) fn is_word_character(character: char) -> bool {
    if character.is_ascii() {
        // ASCII L/N membership is exactly A-Z, a-z, and 0-9; avoid table searches on common Markdown bytes.
        return character.is_ascii_alphanumeric();
    }
    // `is_alphanumeric` includes Alphabetic combining marks; only exact letter/number categories match JS.
    matches!(
        character.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number
    )
}

#[derive(Default)]
/// 会话追加段按原编号寻址；冻结值可共享旧段，只有真正追加的有限尾段需要写时复制。
pub(super) struct MarkdownEditSegments {
    pub(super) segments: Vec<Arc<MarkdownEditSegment>>,
    pub(super) used_bytes: usize,
    // 只有原事务发布才推进；候选文件失败不能被记成尚未改变的原正文Source故障。
    pub(super) published_segments: usize,
}

#[derive(Clone)]
/// 同一编辑段编号只决定字节归属；小输入保原COW尾段，大输入保独立不可变匿名映射。
pub(super) enum MarkdownEditSegment {
    Heap(Vec<u8>),
    Frozen(Arc<OriginalBacking>),
    // 原编号保留真实释放票；这里只持固定原子事实，不再保活旧候选文件或映射。
    RetiredFrozen(Arc<crate::MarkdownOriginalBackingObserver>),
}

#[cfg(test)]
impl std::fmt::Debug for MarkdownEditSegment {
    /// 测试失败只给存储类别与实际长度，不因格式化诊断把整格正文读取出来。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Heap(bytes) => formatter.debug_tuple("Heap").field(&bytes.len()).finish(),
            Self::Frozen(backing) => formatter
                .debug_tuple("Frozen")
                .field(&backing.len())
                .finish(),
            Self::RetiredFrozen(observer) => formatter
                .debug_tuple("RetiredFrozen")
                .field(&observer.mapped_bytes())
                .finish(),
        }
    }
}

#[cfg(test)]
impl PartialEq for MarkdownEditSegment {
    /// 旧堆快照仍比较全部字节；文件段不可变，回滚必须保留原backing身份而非另造同字节文件。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Heap(left), Self::Heap(right)) => left == right,
            (Self::Frozen(left), Self::Frozen(right)) => Arc::ptr_eq(left, right),
            (Self::RetiredFrozen(left), Self::RetiredFrozen(right)) => Arc::ptr_eq(left, right),
            _ => false, // 同一失败事务不能悄悄把原堆段换成文件或把文件复制回堆。
        }
    }
}

#[cfg(test)]
impl Eq for MarkdownEditSegment {}

impl MarkdownEditSegment {
    /// 长度是该段真实extent；封存段不暴露完整slice，也不能转回可写尾段。
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.len(), // 原尾段的有效字节继续决定可追加空间。
            Self::Frozen(backing) => backing.len(), // 映射只提供计量，读取必须另取有限lease。
            Self::RetiredFrozen(_) => 0,      // 退休槽没有可读字节，编号仍为最终释放观察保留。
        }
    }

    /// 缓存命中与空范围同样核真实映射首错；堆段没有文件资源需要查询。
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Heap(_) => Ok(()),
            Self::Frozen(backing) => backing.health().map_err(Into::into),
            Self::RetiredFrozen(observer) => observer.first_errno().map_or(Ok(()), |errno| {
                // 最后快照的延迟Drop仍可能失败；退休不能把原已发布Source首错遗忘。
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    errno,
                ))
            }),
        }
    }

    /// piece偏移仍属于原EditSegment编号；映射分支严格沿原4KiB lease及真实归还错误。
    pub(super) fn read(
        &self,
        range: Range<usize>,
        published: bool,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.health()
            .map_err(|error| edit_read_error(error, published))?;
        match self {
            Self::Heap(bytes) => bytes
                .get(range)
                .map(PieceBytes::Edited)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument),
            Self::Frozen(backing) => backing
                .lease(range)
                .map(|lease| PieceBytes::MappedEdit(lease, published))
                .map_err(|error| edit_read_error(error.into(), published)),
            Self::RetiredFrozen(_) => Err(MarkdownDocumentBufferError::RangeOutsideDocument),
        }
    }

    #[cfg(test)]
    /// 只计堆字节容量；封存映射驻留与最终释放由其真实backing observer分别验证。
    pub(super) fn capacity(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.capacity(),
            Self::Frozen(_) | Self::RetiredFrozen(_) => 0,
        }
    }

    #[cfg(test)]
    /// 旧COW测试只能观察已知堆段；不得借此给封存段增加整段读取入口。
    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Heap(bytes) => bytes,
            Self::Frozen(_) | Self::RetiredFrozen(_) => panic!("封存编辑段只能通过有限lease读取"),
        }
    }
}

impl MarkdownEditSegments {
    /// 按原累计预算追加标量对齐的UTF-8叶；旧快照共享的尾段保持不可变。
    pub(super) fn append(
        &mut self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        if text.is_empty() {
            // 空插入没有新增字节，不分配段、文件或合成零长叶。
            return Ok(Vec::new());
        }
        if text.len() > EDIT_SEGMENT_BYTES {
            // 超过原单尾段的显式输入不再复制成整份堆库存；调用者的str不被保留。
            let backing = self.freeze_input(text, maximum_session_edit_bytes)?;
            return self.append_frozen(backing, maximum_session_edit_bytes);
        }
        self.require_append_capacity(text.len(), maximum_session_edit_bytes)?;
        let mut leaves = Vec::with_capacity(text.len().div_ceil(ORIGINAL_PIECE_BYTES));
        let mut consumed = 0;
        while consumed < text.len() {
            // 每轮消费一个完整标量小片；其字节只进入当前真实可写堆尾一次。
            if self.segments.last().is_none_or(|segment| {
                // 只有尚有空间的堆尾可追加；封存文件从来不重新取得写权限。
                !matches!(segment.as_ref(), MarkdownEditSegment::Heap(bytes) if bytes.len() < EDIT_SEGMENT_BYTES)
            }) {
                // 没有可写尾段才建立原64KiB堆段，已有不可变段编号保持不变。
                self.segments
                    .push(Arc::new(MarkdownEditSegment::Heap(Vec::with_capacity(EDIT_SEGMENT_BYTES))));
            }
            let segment_index = self.segments.len() - 1;
            // 先只读取尾长，避免剩余空间不足一个标量时无意义地复制共享段。
            let segment_start = self.segments[segment_index].len();
            let available = EDIT_SEGMENT_BYTES - segment_start;
            let requested = ORIGINAL_PIECE_BYTES
                .min(available)
                .min(text.len() - consumed);
            let mut chunk_end = consumed + requested;
            while !text.is_char_boundary(chunk_end) {
                // 尾端回退到UTF-8标量边界；下一叶从完整标量开始。
                chunk_end -= 1;
            }
            if chunk_end == consumed {
                // 当前剩余空间容不下下个完整标量时放弃这点尾隙，新段保证输入继续前进。
                self.segments
                    .push(Arc::new(MarkdownEditSegment::Heap(Vec::with_capacity(
                        EDIT_SEGMENT_BYTES,
                    ))));
                continue;
            }
            // 仅当前尾段会发生COW；历史根与冻结根继续按相同编号读取各自的原段。
            let segment = Self::writable_segment(&mut self.segments[segment_index]);
            let chunk = &text.as_bytes()[consumed..chunk_end];
            segment.extend_from_slice(chunk);
            self.used_bytes = self
                .used_bytes
                .checked_add(chunk.len())
                .ok_or(MarkdownDocumentBufferError::EditBufferLimitExceeded)?;
            let piece = MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(segment_index),
                byte_start: segment_start,
                byte_len: chunk.len(),
                metrics: MarkdownDocumentMetrics::from_text(
                    std::str::from_utf8(chunk).expect("显式str按完整UTF-8标量切片"),
                ),
            };
            leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(piece)));
            consumed = chunk_end;
        }
        Ok(leaves)
    }

    /// 显式长输入沿同一4KiB工厂封存；尚未进入段表，原事务可先接管实际释放票。
    pub(super) fn freeze_input(
        &self,
        text: &str,
        maximum_session_edit_bytes: usize,
    ) -> Result<Arc<OriginalBacking>, MarkdownDocumentBufferError> {
        self.require_append_capacity(text.len(), maximum_session_edit_bytes)?;
        let mut output = crate::immutable_snapshot::CanonicalSnapshotOutput::new()
            .map_err(edit_snapshot_error)?;
        let mut consumed = 0;
        while consumed < text.len() {
            // 每次只交付一个完整UTF-8小片；匿名文件写失败前段表与预算仍未改变。
            let mut end = (consumed + ORIGINAL_PIECE_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1; // 最多回退一个多字节标量，不把不完整UTF-8写进候选。
            }
            output
                .write_all(&text.as_bytes()[consumed..end])
                .map_err(edit_snapshot_error)?;
            consumed = end;
        }
        let backing = output
            .freeze()
            .map_err(edit_snapshot_error)?
            .expect("非空显式输入已完整写入匿名文件");
        Ok(Arc::new(backing))
    }

    /// 真实累计预算必须先于堆追加或匿名文件副作用；删除和保存不能补充会话库存。
    fn require_append_capacity(
        &self,
        bytes: usize,
        maximum_session_edit_bytes: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self
            .used_bytes
            .checked_add(bytes)
            .is_none_or(|result| result > maximum_session_edit_bytes)
        {
            return Err(MarkdownDocumentBufferError::EditBufferLimitExceeded);
        }
        Ok(())
    }

    /// 同一段表接管一个已封存的真实文件；全部有限叶读取成功后才发布描述符与累计预算。
    pub(super) fn append_frozen(
        &mut self,
        backing: Arc<OriginalBacking>,
        maximum_session_edit_bytes: usize,
    ) -> Result<Vec<Arc<MarkdownPieceTreeNode>>, MarkdownDocumentBufferError> {
        backing
            .health()
            .map_err(|error| edit_read_error(error.into(), false))?;
        let used = self
            .used_bytes
            .checked_add(backing.len())
            .filter(|used| {
                // 与堆append共用原累计字节预算；删除、Undo或保存都不补充会话库存。
                *used <= maximum_session_edit_bytes
            })
            .ok_or(MarkdownDocumentBufferError::EditBufferLimitExceeded)?;
        let index = self.segments.len();
        let mut leaves = Vec::with_capacity(backing.len().div_ceil(ORIGINAL_PIECE_BYTES));
        let mut offset = 0;
        while offset < backing.len() {
            // 固定请求可能结束在标量内部；只把前面的完整UTF-8前缀建成同一个普通piece。
            let lease = backing
                .lease(offset..(offset + ORIGINAL_PIECE_BYTES).min(backing.len()))
                .map_err(|error| edit_read_error(error.into(), false))?;
            let text = match std::str::from_utf8(&lease) {
                Ok(text) => Ok(text),
                Err(error) if error.error_len().is_none() => {
                    std::str::from_utf8(&lease[..error.valid_up_to()])
                        .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)
                }
                Err(_) => Err(MarkdownDocumentBufferError::InvalidUtf8Document),
            };
            let measured = text.map(|text| (text.len(), MarkdownDocumentMetrics::from_text(text)));
            lease
                .finish()
                .map_err(|error| edit_read_error(error.into(), false))?; // 真实回收失败压过正文验证；候选描述符尚未进入文档。
            let (length, metrics) = measured?;
            if length == 0 {
                // 输出工厂接受任意字节；不完整或非法UTF-8不能制造零长叶或无限重试。
                return Err(MarkdownDocumentBufferError::InvalidUtf8Document);
            }
            leaves.push(Arc::new(MarkdownPieceTreeNode::Leaf(MarkdownPiece {
                source: MarkdownPieceSource::EditSegment(index),
                byte_start: offset,
                byte_len: length,
                metrics,
            })));
            offset += length;
        }
        backing
            .health()
            .map_err(|error| edit_read_error(error.into(), false))?;
        self.segments
            .push(Arc::new(MarkdownEditSegment::Frozen(backing)));
        self.used_bytes = used;
        Ok(leaves)
    }

    /// 原始映射之后再核所有会话封存段；元数据路径也不能复活已失败的编辑源。
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        for segment in &self.segments[..self.published_segments] {
            segment.health()?; // 单个真实文件失败即中止，不以其他可读段替代原输入。
        }
        Ok(())
    }

    /// 候选仍归原事务；真实文件错误拒绝本次输入，不把尚未发布的资源归入旧Source首因。
    pub(super) fn pending_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        for segment in &self.segments[self.published_segments..] {
            segment
                .health()
                .map_err(|error| edit_read_error(error, false))?;
        }
        Ok(())
    }

    /// 原root发布点只移交段的Source归属，不做I/O、分配或可失败验证。
    pub(super) fn publish(&mut self) {
        self.published_segments = self.segments.len();
    }

    /// 同一段表决定其当前归属；外部不传公开编号来给候选补签Source身份。
    pub(super) fn read(
        &self,
        index: usize,
        range: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        self.segments
            .get(index)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?
            .read(range, index < self.published_segments)
    }

    #[cfg(test)]
    /// Measure retained edit capacity for storage regression tests, including unused segment tails.
    pub(super) fn allocated_bytes(&self) -> usize {
        self.segments.iter().map(|segment| segment.capacity()).sum()
    }

    /// Arc复制Vec时只保留有效字节；补足原固定段容量，避免COW后每个小输入重复扩容。
    pub(super) fn writable_segment(segment: &mut Arc<MarkdownEditSegment>) -> &mut Vec<u8> {
        let MarkdownEditSegment::Heap(writable) = Arc::make_mut(segment) else {
            unreachable!("封存段不能进入可写堆尾路径");
        };
        if writable.capacity() < EDIT_SEGMENT_BYTES {
            // 新分配仍仅限原64KiB段；累计append预算与段编号不因快照发生变化。
            writable.reserve_exact(EDIT_SEGMENT_BYTES - writable.len());
        }
        writable
    }
}

/// 匿名输出的真实系统错误保持errno；工厂的取消/extent错误不能被解释成一次成功append。
pub(super) fn edit_snapshot_error(error: SnapshotError) -> MarkdownDocumentBufferError {
    match error {
        SnapshotError::Storage(error) => MarkdownDocumentBufferError::EditStorageUnavailable(
            error.raw_os_error().unwrap_or(libc::EIO),
        ),
        SnapshotError::SourceChanged => {
            MarkdownDocumentBufferError::EditStorageUnavailable(libc::EIO)
        }
        SnapshotError::Cancelled => MarkdownDocumentBufferError::PreparedTextInputStale,
    }
}

/// 只有实际发布过的映射才能撤销原Source；准备期间同一系统错误属于本次候选存储失败。
pub(super) fn edit_read_error(
    error: MarkdownDocumentBufferError,
    published: bool,
) -> MarkdownDocumentBufferError {
    match error {
        MarkdownDocumentBufferError::OriginalBackingUnavailable(errno) if !published => {
            MarkdownDocumentBufferError::EditStorageUnavailable(errno)
        }
        other => other, // 范围与lease限额仍保原具体错误，不伪装成存储成功或默认空值。
    }
}

/// 树只请求有限叶；回调结果须在归还原pin、完成必要页驱逐并核源健康后才能发布。
pub(super) trait PieceByteSource {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError>;

    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError>;

    /// 借用原叶内的实际字节范围；Word量子不能以只检查一字节掩盖整叶lease请求。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError>;

    fn with_piece_bytes<R>(
        &self,
        piece: &MarkdownPiece,
        visit: impl FnOnce(&[u8]) -> Result<R, MarkdownDocumentBufferError>,
    ) -> Result<R, MarkdownDocumentBufferError> {
        let bytes = self.piece_bytes(piece)?;
        let result = visit(&bytes);
        // 先交还原pin并完成本次必要驱逐，再传播回调错误；sentinel不能遮住真实I/O失败。
        bytes.finish()?;
        result
    }
}

/// 编辑段只借原Arc字节；原文件片段另持真实回收lease，不允许裸slice越过lease寿命。
pub(super) enum PieceBytes<'a> {
    Original(crate::original_backing::OriginalReadLease<'a>),
    MappedEdit(crate::original_backing::OriginalReadLease<'a>, bool),
    Edited(&'a [u8]),
}

impl std::ops::Deref for PieceBytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Original(lease) | Self::MappedEdit(lease, _) => lease,
            Self::Edited(bytes) => bytes,
        }
    }
}

impl PieceBytes<'_> {
    pub(super) fn finish(self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Original(lease) => lease.finish().map_err(Into::into),
            Self::MappedEdit(lease, published) => lease
                .finish()
                .map_err(|error| edit_read_error(error.into(), published)),
            // 编辑段没有文件驻留页；生命周期仍由调用方持有的Arc控制。
            Self::Edited(_) => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies the document-owned backing allocation for one immutable piece.
pub(super) enum MarkdownPieceSource {
    Original,
    EditSegment(usize),
}

#[derive(Clone, Debug)]
/// A scalar-aligned immutable byte view and its cached local metrics.
pub(super) struct MarkdownPiece {
    pub(super) source: MarkdownPieceSource,
    pub(super) byte_start: usize,
    pub(super) byte_len: usize,
    pub(super) metrics: MarkdownDocumentMetrics,
}

/// 一个字节保存完整字符域与 ASCII 字母存在性；空文本采用可打印且无字母的合并单位元。
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum PrintableAsciiClass {
    NonPrintable,
    #[default]
    WithoutLetters,
    WithLetters,
}
impl PrintableAsciiClass {
    pub(super) fn is_printable(self) -> bool {
        self != Self::NonPrintable
    }
    pub(super) fn has_letters(self) -> Option<bool> {
        match self {
            Self::NonPrintable => None, // 混合 Unicode/控制符不授予脚本快路，不能只报告是否曾见字母。
            Self::WithoutLetters => Some(false),
            Self::WithLetters => Some(true),
        }
    }
    fn scalar(scalar: char) -> Self {
        if !(' '..='~').contains(&scalar) {
            // 任一控制符或 Unicode 立即撤销整个聚合的可打印域权限。
            Self::NonPrintable
        } else if scalar.is_ascii_alphabetic() {
            // Latin 事实仅由真实 ASCII 字母授予；数字和标点不能代替。
            Self::WithLetters
        } else {
            // 可打印数字、空格和标点保持 Common 候选，等待其它子树的字母事实。
            Self::WithoutLetters
        }
    }
    fn joined(self, other: Self) -> Self {
        match (self, other) {
            // 非可打印事实具有吸收性，不能因另一侧有字母而恢复快路权限。
            (Self::NonPrintable, _) | (_, Self::NonPrintable) => Self::NonPrintable,
            // 两侧均已属于可打印域时，任一真实字母使合并结果具有 Latin 事实。
            (Self::WithLetters, _) | (_, Self::WithLetters) => Self::WithLetters,
            // 空文本和纯数字/标点共同组成无字母的合并单位元。
            _ => Self::WithoutLetters,
        } // 任一负面字符域保守传播；只有两侧可打印时才按 OR 合并字母存在性。
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 结合跨片 CRLF、单词边界与精确字符域的可合并文本聚合。
pub(super) struct MarkdownDocumentMetrics {
    pub(super) byte_len: usize,
    pub(super) utf16_len: usize,
    pub(super) character_count: usize,
    pub(super) word_count: usize,
    pub(super) starts_in_word: bool,
    pub(super) ends_in_word: bool,
    pub(super) line_breaks: usize,
    // 文本检查的段落终止单元独立计数：CRLF为两个，PS也计入，NEL/LS不计入。
    pub(super) text_checking_terminators: usize,
    // 以UTF-16 offset+1编码首尾终止符；None不占额外tag，也不建立每叶字符索引。
    pub(super) first_text_checking_terminator: Option<NonZeroUsize>,
    pub(super) last_text_checking_terminator: Option<NonZeroUsize>,
    pub(super) starts_with_line_feed: bool,
    pub(super) ends_with_carriage_return: bool,
    pub(super) contains_carriage_return: bool,
    pub(super) contains_crlf: bool,
    /// A raw '<' anywhere can introduce an inherited HTML block outside a local cue envelope.
    pub(super) contains_ascii_less_than: bool,
    /// 复用一个字节区分非可打印、可打印无字母、可打印含字母；不增加每个树节点的尺寸。
    pub(super) all_printable_ascii: PrintableAsciiClass,
    // 仅附记真实Unicode属性集合；不包含Bidi/分项支持策略或任何Source读取权限。
    pub(super) unicode_properties: MarkdownUnicodeProperties,
}

impl Default for MarkdownDocumentMetrics {
    fn default() -> Self {
        // 空文本不包含风险字符，但满足“所有字符均可打印ASCII”；不能沿用bool默认false。
        Self {
            byte_len: 0,
            utf16_len: 0,
            character_count: 0,
            word_count: 0,
            starts_in_word: false,
            ends_in_word: false,
            line_breaks: 0,
            text_checking_terminators: 0,
            first_text_checking_terminator: None,
            last_text_checking_terminator: None,
            starts_with_line_feed: false,
            ends_with_carriage_return: false,
            contains_carriage_return: false,
            contains_crlf: false,
            contains_ascii_less_than: false,
            all_printable_ascii: PrintableAsciiClass::WithoutLetters,
            unicode_properties: MarkdownUnicodeProperties::default(),
        }
    }
}

impl MarkdownDocumentMetrics {
    /// Compute associative document aggregates for one scalar-aligned leaf without materializing auxiliary indexes.
    pub(super) fn from_text(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut metrics = Self {
            byte_len: bytes.len(),
            starts_with_line_feed: bytes.first() == Some(&b'\n'),
            ends_with_carriage_return: bytes.last() == Some(&b'\r'),
            starts_in_word: text.chars().next().is_some_and(|character| {
                // A piece beginning with an L/N scalar may continue a word ending in its left neighbor.
                is_word_character(character)
            }),
            ..Self::default()
        };
        let mut previous_was_carriage_return = false;
        for character in text.chars() {
            // 原4KiB建叶遍历同时收集通用属性；后继布局不为确认未读后缀再次顺扫正文。
            metrics.unicode_properties.observe(character);
            // Visit each Unicode scalar once; only the previous CR and word state cross iterations.
            // Normalize a CRLF pair to one visible character, including across piece joins below.
            if character != '\n' || !previous_was_carriage_return {
                // A CRLF continuation shares its visible-character count with the preceding CR.
                metrics.character_count += 1;
            }
            // Cache the exact baseline regex runs without allocating tokens or applying locale segmentation.
            let in_word = is_word_character(character);
            if in_word && !metrics.ends_in_word {
                // Punctuation, marks, emoji, controls, and line endings separate successive L/N runs.
                metrics.word_count += 1;
            }
            metrics.ends_in_word = in_word;
            // 复用原标量遍历；C0、DEL及全部非ASCII均使聚合为false，不再补扫正文。
            metrics.all_printable_ascii = metrics
                .all_printable_ascii
                .joined(PrintableAsciiClass::scalar(character));
            if matches!(character, '\r' | '\n' | '\u{2029}') {
                // 同一已有标量遍历取得段落端点，不为新查询再次扫描原始正文。
                // 记录发生在长度累加前，因此低代理项等请求坐标不影响终止符自身的位置。
                let offset = NonZeroUsize::new(metrics.utf16_len + 1)
                    .expect("a UTF-16 terminator offset plus one is nonzero");
                metrics.text_checking_terminators += 1;
                if metrics.first_text_checking_terminator.is_none() {
                    // 首个终止符只记录一次；后续终止符只更新尾端点与原始单元数。
                    metrics.first_text_checking_terminator = Some(offset);
                }
                metrics.last_text_checking_terminator = Some(offset);
            }
            metrics.utf16_len += character.len_utf16();
            // Maintain conservative syntax risk in the existing aggregate pass, including edited pieces.
            metrics.contains_ascii_less_than |= character == '<';
            match character {
                // A CR starts a line break immediately; a later LF may merge with it.
                '\r' => {
                    metrics.line_breaks += 1;
                    metrics.contains_carriage_return = true;
                    previous_was_carriage_return = true;
                }
                // The LF completes the existing CRLF break and records the line-ending form.
                '\n' if previous_was_carriage_return => {
                    metrics.contains_crlf = true;
                    previous_was_carriage_return = false;
                }
                // A standalone LF starts its own line break.
                '\n' => {
                    metrics.line_breaks += 1;
                    previous_was_carriage_return = false;
                }
                // Ordinary text ends the CRLF lookbehind state.
                _ => previous_was_carriage_return = false,
            }
        }
        metrics
    }

    /// Merge adjacent subtree aggregates, reconciling CRLF and word runs split at the join.
    pub(super) fn joined(left: Self, right: Self) -> Self {
        if left.byte_len == 0 {
            // An empty left side contributes no boundary state and acts as the identity.
            return right;
        }
        if right.byte_len == 0 {
            // An empty right side likewise preserves all left aggregate state.
            return left;
        }
        let boundary_is_crlf = left.ends_with_carriage_return && right.starts_with_line_feed;
        Self {
            byte_len: left.byte_len + right.byte_len,
            utf16_len: left.utf16_len + right.utf16_len,
            character_count: left.character_count + right.character_count
                - usize::from(boundary_is_crlf),
            word_count: left.word_count + right.word_count
                - usize::from(left.ends_in_word && right.starts_in_word),
            starts_in_word: left.starts_in_word,
            ends_in_word: right.ends_in_word,
            // 只有两侧都满足才可为未读正文签同一精确字符域，不能以byte_len==utf16_len代替。
            all_printable_ascii: left.all_printable_ascii.joined(right.all_printable_ascii),
            unicode_properties: left.unicode_properties.joined(right.unicode_properties),
            line_breaks: left.line_breaks + right.line_breaks - usize::from(boundary_is_crlf),
            // 该查询必须区分CR与紧随的LF，不继承物理行计数的跨叶CRLF合并。
            text_checking_terminators: left.text_checking_terminators
                + right.text_checking_terminators,
            first_text_checking_terminator: left.first_text_checking_terminator.or_else(|| {
                // 右端点相对右子树；移到合并坐标时只加左侧UTF-16长度，保留offset+1编码。
                right.first_text_checking_terminator.map(|offset| {
                    NonZeroUsize::new(left.utf16_len + offset.get())
                        .expect("a shifted terminator offset remains nonzero")
                })
            }),
            last_text_checking_terminator: right
                .last_text_checking_terminator
                .map(|offset| {
                    // 右侧存在终止符时它必为合并尾端点；否则直接保留左侧原坐标。
                    NonZeroUsize::new(left.utf16_len + offset.get())
                        .expect("a shifted terminator offset remains nonzero")
                })
                .or(left.last_text_checking_terminator),
            starts_with_line_feed: left.starts_with_line_feed,
            ends_with_carriage_return: right.ends_with_carriage_return,
            contains_carriage_return: left.contains_carriage_return
                || right.contains_carriage_return,
            contains_crlf: left.contains_crlf || right.contains_crlf || boundary_is_crlf,
            contains_ascii_less_than: left.contains_ascii_less_than
                || right.contains_ascii_less_than,
        }
    }
}

/// An absent root denotes empty text without a synthetic sentinel allocation.
pub(super) type PieceTreeLink = Option<Arc<MarkdownPieceTreeNode>>;

#[derive(Clone, Debug)]
/// Persistent AVL node; Arc ownership lets history share unchanged immutable subtrees.
pub(super) enum MarkdownPieceTreeNode {
    Leaf(MarkdownPiece),
    /// 物理原始叶共享有限小目录；读者仍只能取得其中一个≤4KiB逻辑片。
    OriginalSpan(OriginalSpan),
    Branch {
        left: Arc<MarkdownPieceTreeNode>,
        right: Arc<MarkdownPieceTreeNode>,
        metrics: MarkdownDocumentMetrics,
        height: u16,
    },
}

impl MarkdownPieceTreeNode {
    /// Read cached subtree aggregates without scanning document text.
    pub(super) fn metrics(&self) -> MarkdownDocumentMetrics {
        match self {
            // A leaf owns the metrics of its immutable byte slice.
            Self::Leaf(piece) => piece.metrics,
            // 合组叶保留原joined计量，调用方不必展开目录才能读取全文聚合。
            Self::OriginalSpan(span) => span.metrics,
            // A branch stores the already-reconciled aggregate of its children.
            Self::Branch { metrics, .. } => *metrics,
        }
    }

    /// Read the cached AVL height used by path-copying joins and rotations.
    pub(super) fn height(&self) -> u16 {
        match self {
            // A leaf is the nonempty tree height base case.
            Self::Leaf(_) => 1,
            // 小目录是一个真实物理叶，内部4KiB记录不增加AVL层级。
            Self::OriginalSpan(_) => 1,
            // A branch carries its height so balancing never walks the whole subtree.
            Self::Branch { height, .. } => *height,
        }
    }
}

pub(super) struct OriginalTreeBuild {
    pub(super) root: PieceTreeLink,
    pub(super) revision: MarkdownDocumentRevision,
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    pub(super) checkpoints: MarkdownBlockCheckpointIndex,
}

/// 初开在同一不可变映射上逐4KiB借用，UTF-8尾标量留在原映射供下一片读取。
/// 每个有限片在hash、metrics与checkpoint事实消费后真实回收，不形成完整str或正文副本。
pub(super) fn build_original_backing_tree(
    backing: Option<&crate::original_backing::OriginalBacking>,
    range: Range<usize>,
) -> Result<OriginalTreeBuild, MarkdownDocumentBufferError> {
    let mut leaves = Vec::with_capacity(
        range
            .len()
            .div_ceil(ORIGINAL_PIECE_BYTES * ORIGINAL_SPAN_PIECES),
    );
    let mut spans = OriginalSpanBuilder::default();
    let mut hasher = Sha256::new();
    #[cfg(any(test, feature = "native-markdown-candidate"))]
    let mut scanner = InitialBlockScanner::new();
    let mut byte_start = range.start;
    while byte_start < range.end {
        let backing = backing.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        let end = byte_start
            .saturating_add(ORIGINAL_PIECE_BYTES)
            .min(range.end);
        let consumed = backing.with_bytes(byte_start..end, |bytes| {
            let text = match std::str::from_utf8(bytes) {
                Ok(text) => text,
                // 唯一可续接的错误是块尾不完整标量；真实EOF或非法序列必须立即拒绝。
                Err(error)
                    if error.error_len().is_none()
                        && end < range.end
                        && error.valid_up_to() > 0 =>
                {
                    std::str::from_utf8(&bytes[..error.valid_up_to()])
                        .expect("validated UTF-8 prefix")
                }
                Err(_) => return Err(MarkdownDocumentBufferError::InvalidUtf8Document),
            };
            hasher.update(text.as_bytes());
            #[cfg(any(test, feature = "native-markdown-candidate"))]
            scanner.push(text);
            spans.push(
                MarkdownPiece {
                    source: MarkdownPieceSource::Original,
                    byte_start: byte_start - range.start,
                    byte_len: text.len(),
                    metrics: MarkdownDocumentMetrics::from_text(text),
                },
                &mut leaves,
            )?;
            Ok(text.len())
        })??;
        byte_start += consumed;
    }
    spans.flush(&mut leaves)?;
    Ok(OriginalTreeBuild {
        root: build_balanced_piece_tree(leaves),
        revision: MarkdownDocumentRevision(hasher.finalize().into()),
        #[cfg(any(test, feature = "native-markdown-candidate"))]
        checkpoints: scanner.finish(),
    })
}

#[cfg(test)]
/// Index the mapped original text in bounded UTF-8 leaves while hashing the initial revision.
pub(super) fn build_original_piece_tree(text: &str) -> (PieceTreeLink, MarkdownDocumentRevision) {
    let mut leaves = Vec::with_capacity(
        text.len()
            .div_ceil(ORIGINAL_PIECE_BYTES * ORIGINAL_SPAN_PIECES),
    );
    let mut spans = OriginalSpanBuilder::default();
    let mut hasher = Sha256::new();
    let mut byte_start = 0;
    while byte_start < text.len() {
        // Build each leaf and hash the same byte sequence in document order.
        let mut byte_end = (byte_start + ORIGINAL_PIECE_BYTES).min(text.len());
        while !text.is_char_boundary(byte_end) {
            // Keep leaf endpoints on scalar boundaries so later slices can be decoded independently.
            byte_end -= 1;
        }
        let slice = &text[byte_start..byte_end];
        hasher.update(slice.as_bytes());
        spans
            .push(
                MarkdownPiece {
                    source: MarkdownPieceSource::Original,
                    byte_start,
                    byte_len: slice.len(),
                    metrics: MarkdownDocumentMetrics::from_text(slice),
                },
                &mut leaves,
            )
            .expect("有限已验证原始片必须可合组");
        byte_start = byte_end;
    }
    if text.is_empty() {
        // The empty document still receives the canonical SHA-256 digest for zero bytes.
        hasher.update([]);
    }
    let revision = MarkdownDocumentRevision(hasher.finalize().into());
    spans
        .flush(&mut leaves)
        .expect("有限原始Span目录必须可封闭");
    (build_balanced_piece_tree(leaves), revision)
}

/// Turn ordered leaves into a balanced persistent root, using None for empty text.
pub(super) fn build_balanced_piece_tree(nodes: Vec<Arc<MarkdownPieceTreeNode>>) -> PieceTreeLink {
    if nodes.is_empty() {
        // An empty leaf collection has no root and allocates no sentinel node.
        return None;
    }
    Some(build_balanced_piece_tree_slice(&nodes))
}

/// Recursively bisect ordered leaves to establish a balanced initial tree.
pub(super) fn build_balanced_piece_tree_slice(
    nodes: &[Arc<MarkdownPieceTreeNode>],
) -> Arc<MarkdownPieceTreeNode> {
    if nodes.len() == 1 {
        // Reuse a singleton leaf by Arc rather than copying its text or metadata.
        return Arc::clone(&nodes[0]);
    }
    let middle = nodes.len() / 2;
    make_piece_tree_branch(
        build_balanced_piece_tree_slice(&nodes[..middle]),
        build_balanced_piece_tree_slice(&nodes[middle..]),
    )
}

/// Create one immutable branch with recomputed metrics and height from its owned children.
pub(super) fn make_piece_tree_branch(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    Arc::new(MarkdownPieceTreeNode::Branch {
        metrics: MarkdownDocumentMetrics::joined(left.metrics(), right.metrics()),
        height: left.height().max(right.height()) + 1,
        left,
        right,
    })
}

/// Concatenate optional trees while preserving byte order and sharing unchanged roots.
pub(super) fn join_piece_trees(left: PieceTreeLink, right: PieceTreeLink) -> PieceTreeLink {
    match (left, right) {
        // An absent prefix leaves the suffix root unchanged.
        (None, right) => right,
        // An absent suffix leaves the prefix root unchanged.
        (left, None) => left,
        // Two nonempty sides require a height-aware persistent join.
        (Some(left), Some(right)) => Some(join_piece_tree_nodes(left, right)),
    }
}

/// Join nonempty AVL trees by descending only the taller spine and rebuilding that path.
pub(super) fn join_piece_tree_nodes(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    let left_height = left.height();
    let right_height = right.height();
    if left_height > right_height + 1 {
        // Descend the left tree's right spine so the joined subtree remains height compatible.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            Arc::clone(left_left),
            join_piece_tree_nodes(Arc::clone(left_right), right),
        );
    }
    if right_height > left_height + 1 {
        // Descend the right tree's left spine for the symmetric height imbalance.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        return rebalance_piece_tree(
            join_piece_tree_nodes(left, Arc::clone(right_left)),
            Arc::clone(right_right),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Restore the AVL height invariant with order-preserving single or double rotations.
pub(super) fn rebalance_piece_tree(
    left: Arc<MarkdownPieceTreeNode>,
    right: Arc<MarkdownPieceTreeNode>,
) -> Arc<MarkdownPieceTreeNode> {
    if left.height() > right.height() + 1 {
        // A left-heavy pair must rotate before becoming a reusable branch.
        let MarkdownPieceTreeNode::Branch {
            left: left_left,
            right: left_right,
            ..
        } = left.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if left_left.height() >= left_right.height() {
            // An outer-heavy left child needs one right rotation; all unaffected subtrees remain shared.
            return make_piece_tree_branch(
                Arc::clone(left_left),
                make_piece_tree_branch(Arc::clone(left_right), right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = left_right.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(Arc::clone(left_left), Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), right),
        );
    }
    if right.height() > left.height() + 1 {
        // A right-heavy pair uses the symmetric rotation rules.
        let MarkdownPieceTreeNode::Branch {
            left: right_left,
            right: right_right,
            ..
        } = right.as_ref()
        else {
            // A leaf has height one, so the validated height imbalance requires a branch here.
            unreachable!("an imbalanced AVL child cannot be a leaf");
        };
        if right_right.height() >= right_left.height() {
            // An outer-heavy right child needs one left rotation rather than a double rotation.
            return make_piece_tree_branch(
                make_piece_tree_branch(left, Arc::clone(right_left)),
                Arc::clone(right_right),
            );
        }
        let MarkdownPieceTreeNode::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = right_left.as_ref()
        else {
            // The inner child is taller than its leaf sibling, which requires another branch.
            unreachable!("a double rotation requires an inner branch");
        };
        return make_piece_tree_branch(
            make_piece_tree_branch(left, Arc::clone(middle_left)),
            make_piece_tree_branch(Arc::clone(middle_right), Arc::clone(right_right)),
        );
    }
    make_piece_tree_branch(left, right)
}

/// Split at a scalar-aligned byte position, sharing untouched subtrees for undo history.
pub(super) fn split_piece_tree(
    document: &impl PieceByteSource,
    root: PieceTreeLink,
    byte_offset: usize,
) -> Result<(PieceTreeLink, PieceTreeLink), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // Only offset zero exists in an empty document.
            Ok((None, None))
        } else {
            // A nonzero position cannot be represented by an absent root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_offset == 0 {
        // A zero split transfers the complete existing tree to the suffix.
        return Ok((None, Some(root)));
    }
    if byte_offset == root.metrics().byte_len {
        // An end split transfers the complete existing tree to the prefix.
        return Ok((Some(root), None));
    }
    if byte_offset > root.metrics().byte_len {
        // Offsets beyond the aggregate byte length are rejected before slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }

    match root.as_ref() {
        // Only the intersected leaf is divided; its bytes continue to borrow their original storage.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            if !std::str::from_utf8(bytes)
                .expect("piece bytes remain valid UTF-8")
                .is_char_boundary(byte_offset)
            {
                // Reject a split inside a UTF-8 scalar rather than producing invalid leaf slices.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            let left_piece = markdown_piece_subrange(piece, bytes, 0..byte_offset);
            let right_piece = markdown_piece_subrange(piece, bytes, byte_offset..bytes.len());
            Ok((
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(left_piece))),
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(right_piece))),
            ))
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 分裂先定位真实小片；完整目录前缀不借入Source，也不扩大原Leaf的正文大小。
            let hit = span.locate_byte(byte_offset, true)?;
            if hit.local == hit.piece.byte_len {
                // 小片边界只切共享目录范围，不读取正文或把完整Span降成普通大Leaf。
                return Ok((
                    span.subrange(span.record_range().start..hit.ordinal + 1)?,
                    span.subrange(hit.ordinal + 1..span.record_range().end)?,
                ));
            }
            let before = span.subrange(span.record_range().start..hit.ordinal)?;
            let after = span.subrange(hit.ordinal + 1..span.record_range().end)?;
            // 只有一个真实≤4KiB小片复用原Unicode分裂；前后完整小片继续共享同一目录。
            let (left, right) = split_piece_tree(
                document,
                Some(Arc::new(MarkdownPieceTreeNode::Leaf(hit.piece))),
                hit.local,
            )?;
            Ok((
                join_piece_trees(before, left),
                join_piece_trees(right, after),
            ))
        }
        // Use subtree byte aggregates to split only the side containing the requested boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_bytes = left.metrics().byte_len;
            if byte_offset < left_bytes {
                // A boundary inside the left child retains the right child as part of the suffix.
                let (prefix, left_suffix) =
                    split_piece_tree(document, Some(Arc::clone(left)), byte_offset)?;
                Ok((
                    prefix,
                    join_piece_trees(left_suffix, Some(Arc::clone(right))),
                ))
            } else if byte_offset == left_bytes {
                // A boundary between children can reuse both roots without allocating leaf fragments.
                Ok((Some(Arc::clone(left)), Some(Arc::clone(right))))
            } else {
                // A boundary inside the right child retains the left child as part of the prefix.
                let (right_prefix, suffix) =
                    split_piece_tree(document, Some(Arc::clone(right)), byte_offset - left_bytes)?;
                Ok((
                    join_piece_trees(Some(Arc::clone(left)), right_prefix),
                    suffix,
                ))
            }
        }
    }
}

/// Create a metadata-only slice of a validated UTF-8 piece and recompute its local aggregates.
pub(super) fn markdown_piece_subrange(
    piece: &MarkdownPiece,
    bytes: &[u8],
    range: Range<usize>,
) -> MarkdownPiece {
    let text = std::str::from_utf8(&bytes[range.clone()]).expect("piece subrange is UTF-8");
    MarkdownPiece {
        source: piece.source,
        byte_start: piece.byte_start + range.start,
        byte_len: range.len(),
        metrics: MarkdownDocumentMetrics::from_text(text),
    }
}

/// 通过缓存子树长度和至多一个叶扫描翻译UTF-16标量边界；字节来源不持有文档权限。
pub(super) fn byte_offset_for_utf16_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    if utf16_offset == 0 {
        // 同一子树的起点必是已验证UTF8边界。健康门仍先行，但不能为查询固定起点
        // 再借首叶、验证整4KiB或触碰原映射页目录，扰动远端正在使用的有界窗口。
        return Ok(accumulated_bytes);
    }
    let Some(root) = root else {
        // 空树的唯一起点已在上方处理；剩余正位置不可能属于空子树。
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    };
    match root {
        // Decode the selected leaf to distinguish scalar boundaries from surrogate interiors.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut traversed_utf16 = 0;
            for (byte_index, character) in text.char_indices() {
                // Advance by Unicode scalar while retaining the UTF-16 length consumed before it.
                if traversed_utf16 == utf16_offset {
                    // A completed scalar prefix identifies an exact byte boundary.
                    return Ok(accumulated_bytes + byte_index);
                }
                let next = traversed_utf16 + character.len_utf16();
                if utf16_offset < next {
                    // The requested unit lies inside a surrogate pair and cannot define a text edit boundary.
                    return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                }
                traversed_utf16 = next;
            }
            if traversed_utf16 == utf16_offset {
                // The position immediately after the final scalar is a valid leaf boundary.
                Ok(accumulated_bytes + text.len())
            } else {
                // Any remaining requested units lie beyond this leaf.
                Err(MarkdownDocumentBufferError::RangeOutsideDocument)
            }
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 目录只给出命中小片及缓存前缀，标量边界判断仍复用原4KiB解码路径。
            let hit = span.locate_utf16(utf16_offset, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            byte_offset_for_utf16_in_tree(
                document,
                Some(&leaf),
                hit.local,
                accumulated_bytes + hit.prefix.byte_len,
            )
        }
        // Descend using cached UTF-16 lengths while carrying the corresponding byte prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset <= left.metrics().utf16_len {
                // The target boundary belongs to the left subtree, including its end position.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(left.as_ref()),
                    utf16_offset,
                    accumulated_bytes,
                )
            } else {
                // Subtract the left UTF-16 prefix and add its bytes before entering the right subtree.
                byte_offset_for_utf16_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                    accumulated_bytes + left.metrics().byte_len,
                )
            }
        }
    }
}

/// Translate a small standalone string boundary with the same surrogate rejection as tree coordinates.
pub(super) fn byte_offset_for_utf16_text(
    text: &str,
    utf16_offset: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    let mut traversed_utf16 = 0;
    for (byte_index, character) in text.char_indices() {
        // Inspect scalar boundaries in order without allocating a UTF-16 copy.
        if traversed_utf16 == utf16_offset {
            // The consumed unit count identifies an exact byte boundary.
            return Ok(byte_index);
        }
        let next = traversed_utf16 + character.len_utf16();
        if utf16_offset < next {
            // A partial surrogate pair cannot be used as a replacement endpoint.
            return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
        }
        traversed_utf16 = next;
    }
    if traversed_utf16 == utf16_offset {
        // The end-of-string boundary is valid after all scalars are consumed.
        Ok(text.len())
    } else {
        // An offset beyond the encoded length is outside the supplied text.
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    }
}

/// 翻译标量对齐的字节位置；活动文档与冻结值使用相同边界拒绝规则。
pub(super) fn utf16_offset_for_byte_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    byte_offset: usize,
    accumulated_utf16: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_offset == 0 {
            // The empty subtree contributes no UTF-16 units at its only boundary.
            Ok(accumulated_utf16)
        } else {
            // A positive byte offset requires nonempty storage.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    match root {
        // Decode only the selected leaf to count the UTF-16 prefix.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_offset) {
                // A byte inside a multibyte scalar has no valid UTF-16 edit boundary.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(accumulated_utf16 + text[..byte_offset].encode_utf16().count())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 字节到UTF16只解码命中小片；完整目录前缀不借入Source。
            let hit = span.locate_byte(byte_offset, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            utf16_offset_for_byte_in_tree(
                document,
                Some(&leaf),
                hit.local,
                accumulated_utf16 + hit.prefix.utf16_len,
            )
        }
        // Branch aggregates select a single descent and preserve the accumulated UTF-16 prefix.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_offset <= left.metrics().byte_len {
                // The boundary lies in the left subtree, including the shared child boundary.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(left.as_ref()),
                    byte_offset,
                    accumulated_utf16,
                )
            } else {
                // The right-side coordinate excludes the left bytes and includes its UTF-16 length.
                utf16_offset_for_byte_in_tree(
                    document,
                    Some(right.as_ref()),
                    byte_offset - left.metrics().byte_len,
                    accumulated_utf16 + left.metrics().utf16_len,
                )
            }
        }
    }
}

/// 读取单个UTF-16单元；NSString允许读取代理对的任意半边，不要求编辑边界。
pub(super) fn utf16_code_unit_in_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    utf16_offset: usize,
) -> Result<u16, MarkdownDocumentBufferError> {
    document.source_health()?;
    let root = root.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
    match root {
        // Only the selected leaf needs scalar decoding; nth may intentionally return a surrogate half.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            text.encode_utf16()
                .nth(utf16_offset)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 原始单元必须真实包含在一片内，片尾与EOF不能借前一个字符冒充。
            let hit = span.locate_utf16(utf16_offset, false)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            utf16_code_unit_in_tree(document, Some(&leaf), hit.local)
        }
        // Cached UTF-16 lengths locate the leaf without flattening the document.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if utf16_offset < left.metrics().utf16_len {
                // A strictly smaller position addresses a unit in the left child.
                utf16_code_unit_in_tree(document, Some(left.as_ref()), utf16_offset)
            } else {
                // The right child receives an offset relative to its own first unit.
                utf16_code_unit_in_tree(
                    document,
                    Some(right.as_ref()),
                    utf16_offset - left.metrics().utf16_len,
                )
            }
        }
    }
}

/// 只将请求范围复制到调用方缓冲，剪除所有不相交子树，不建立完整UTF-16镜像。
pub(super) fn copy_utf16_tree_range(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_utf16: usize,
    destination: &mut [u16],
    written: &mut usize,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    let node_start = accumulated_utf16;
    let node_end = node_start + root.metrics().utf16_len;
    if range.end <= node_start || range.start >= node_end {
        // A disjoint node contributes no units and requires no text decoding.
        return Ok(());
    }

    match root {
        // Clamp the overlap to this leaf and reserve only the corresponding destination slice.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.metrics.utf16_len);
            let requested_units = local_end.saturating_sub(local_start);
            let destination_end = written
                .checked_add(requested_units)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let output = destination
                .get_mut(*written..destination_end)
                .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            for (slot, code_unit) in output
                .iter_mut()
                .zip(text.encode_utf16().skip(local_start).take(requested_units))
            {
                // Copy exactly the requested code units; partial surrogate ranges are valid NSString reads.
                *slot = code_unit;
            }
            *written = destination_end;
            Ok(())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 只沿有限目录按序提交相交小片；递归入口先剪枝，再借用至多4KiB正文。
            let mut base = accumulated_utf16;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = base + piece.metrics.utf16_len;
                let leaf = MarkdownPieceTreeNode::Leaf(piece);
                copy_utf16_tree_range(
                    document,
                    Some(&leaf),
                    range.clone(),
                    base,
                    destination,
                    written,
                )?;
                base = next;
            }
            Ok(())
        }
        // Visit children in document order so the mutable output cursor advances monotonically.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            copy_utf16_tree_range(
                document,
                Some(left.as_ref()),
                range.clone(),
                accumulated_utf16,
                destination,
                written,
            )?;
            copy_utf16_tree_range(
                document,
                Some(right.as_ref()),
                range,
                accumulated_utf16 + left.metrics().utf16_len,
                destination,
                written,
            )
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Locate the byte after one logical line break while merging cross-leaf CRLF pairs.
pub(super) fn find_line_break_end(
    document: &MarkdownDocumentBuffer,
    root: &MarkdownPieceTreeNode,
    break_index: usize,
    accumulated_bytes: usize,
) -> Result<usize, MarkdownDocumentBufferError> {
    document.source_health()?;
    match root {
        // Scan only the selected leaf and distinguish paired from standalone line endings.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let mut seen = 0;
            let mut index = 0;
            while index < bytes.len() {
                // Each iteration consumes at least one byte and counts only logical line breaks.
                let break_end = match bytes[index] {
                    // A local CRLF is one break whose endpoint follows both bytes.
                    b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                        index += 2;
                        Some(index)
                    }
                    // A standalone CR or LF contributes one one-byte break.
                    b'\r' | b'\n' => {
                        index += 1;
                        Some(index)
                    }
                    // Ordinary UTF-8 bytes cannot be ASCII line-ending bytes and need no decoding here.
                    _ => {
                        index += 1;
                        None
                    }
                };
                if let Some(break_end) = break_end {
                    // Only a recognized line ending advances the logical break index.
                    if seen == break_index {
                        // The requested break has been reached; publish its document-relative endpoint.
                        return Ok(accumulated_bytes + break_end);
                    }
                    seen += 1;
                }
            }
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 小片计量确定唯一命中的逻辑换行；跨小片CRLF需要返回LF之后的真实端点。
            let mut prefix = MarkdownDocumentMetrics::default();
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let joined = MarkdownDocumentMetrics::joined(prefix, piece.metrics);
                if break_index < joined.line_breaks {
                    let continued_lf = usize::from(
                        prefix.ends_with_carriage_return && piece.metrics.starts_with_line_feed,
                    );
                    let local = break_index - prefix.line_breaks + continued_lf;
                    if piece.metrics.ends_with_carriage_return
                        && local + 1 == piece.metrics.line_breaks
                        && ordinal + 1 < span.record_range().end
                        && span.piece(ordinal + 1)?.metrics.starts_with_line_feed
                    {
                        return Ok(accumulated_bytes + prefix.byte_len + piece.byte_len + 1);
                    } // CRLF可跨目录小片，仍只有一个逻辑换行且端点在LF之后。
                    return find_line_break_end(
                        document,
                        &MarkdownPieceTreeNode::Leaf(piece),
                        local,
                        accumulated_bytes + prefix.byte_len,
                    );
                }
                prefix = joined;
            }
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        }
        // Use subtree counts but reconcile a CRLF split across the child boundary.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            let left_metrics = left.metrics();
            let right_metrics = right.metrics();
            let boundary_is_crlf =
                left_metrics.ends_with_carriage_return && right_metrics.starts_with_line_feed;
            if boundary_is_crlf {
                // The left trailing CR and right leading LF represent a single shared line break.
                let independent_left_breaks = left_metrics.line_breaks - 1;
                if break_index < independent_left_breaks {
                    // Breaks before the shared CRLF remain wholly inside the left child.
                    return find_line_break_end(document, left, break_index, accumulated_bytes);
                }
                if break_index == independent_left_breaks {
                    // The shared CRLF ends one byte into the right child.
                    return Ok(accumulated_bytes + left_metrics.byte_len + 1);
                }
                return find_line_break_end(
                    document,
                    right,
                    break_index - independent_left_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                );
            }
            if break_index < left_metrics.line_breaks {
                // Without a boundary CRLF, ordinary subtree break counts directly select the left side.
                find_line_break_end(document, left, break_index, accumulated_bytes)
            } else {
                // The right side receives the remaining break index after the complete left prefix.
                find_line_break_end(
                    document,
                    right,
                    break_index - left_metrics.line_breaks,
                    accumulated_bytes + left_metrics.byte_len,
                )
            }
        }
    }
}

#[cfg(any(test, feature = "native-markdown-candidate"))]
/// Aggregate a validated byte prefix using cached complete subtrees and one partial leaf.
pub(super) fn metrics_for_tree_prefix(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    byte_count: usize,
) -> Result<MarkdownDocumentMetrics, MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty storage has only its zero boundary; reject any positive requested extent below.
        return if byte_count == 0 {
            // An empty tree has a valid empty prefix with identity metrics.
            Ok(MarkdownDocumentMetrics::default())
        } else {
            // A nonempty prefix cannot be represented without a root.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        };
    };
    if byte_count == 0 {
        // The empty prefix requires no leaf decoding.
        return Ok(MarkdownDocumentMetrics::default());
    }
    if byte_count == root.metrics().byte_len {
        // A complete prefix can reuse all cached aggregates.
        return Ok(root.metrics());
    }
    if byte_count > root.metrics().byte_len {
        // Reject oversized prefixes before arithmetic or slicing.
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    match root {
        // Only a partial leaf requires rescanning its scalar-aligned prefix.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            if !text.is_char_boundary(byte_count) {
                // Prefix boundaries cannot divide a UTF-8 scalar.
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(MarkdownDocumentMetrics::from_text(&text[..byte_count]))
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 完整小片使用缓存，只有末端部分仍交原叶函数检查Unicode并计算计量。
            let hit = span.locate_byte(byte_count, true)?;
            let leaf = MarkdownPieceTreeNode::Leaf(hit.piece);
            let tail = metrics_for_tree_prefix(document, Some(&leaf), hit.local)?;
            Ok(MarkdownDocumentMetrics::joined(hit.prefix, tail))
        }
        // Complete child metrics avoid rescanning unaffected text.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            if byte_count <= left.metrics().byte_len {
                // A prefix contained in the left child excludes the right child entirely.
                metrics_for_tree_prefix(document, Some(left.as_ref()), byte_count)
            } else {
                // A prefix entering the right child combines the complete left aggregate with a partial right one.
                let right_prefix = metrics_for_tree_prefix(
                    document,
                    Some(right.as_ref()),
                    byte_count - left.metrics().byte_len,
                )?;
                Ok(MarkdownDocumentMetrics::joined(
                    left.metrics(),
                    right_prefix,
                ))
            }
        }
    }
}

/// 按正文顺序借用请求范围内的原有有限叶；每次回调都不超过该叶的字节边界。
pub(super) fn visit_piece_tree_range(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    accumulated_bytes: usize,
    visit: &mut impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    let node_start = accumulated_bytes;
    let node_end = node_start + root.metrics().byte_len;
    if range.end <= node_start || range.start >= node_end {
        // Prune disjoint subtrees before borrowing any backing bytes.
        return Ok(());
    }
    match root {
        // Clamp the overlap to the selected leaf and lend it only for this callback.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            let local_start = range.start.saturating_sub(node_start);
            let local_end = (range.end - node_start).min(piece.byte_len);
            visit(&bytes[local_start..local_end])
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // Span不成为大正文切片；每个实际回调仍由一个真实4KiB逻辑片产生。
            let mut base = accumulated_bytes;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = base + piece.byte_len;
                let leaf = MarkdownPieceTreeNode::Leaf(piece);
                visit_piece_tree_range(document, Some(&leaf), range.clone(), base, visit)?;
                base = next;
            }
            Ok(())
        }
        // Visit the left range first, propagating callback failures before entering the right side.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            visit_piece_tree_range(
                document,
                Some(left.as_ref()),
                range.clone(),
                node_start,
                visit,
            )?;
            visit_piece_tree_range(
                document,
                Some(right.as_ref()),
                range,
                node_start + left.metrics().byte_len,
                visit,
            )
        }
    }
}

/// Stream the live document into a writer without allocating a document-sized String.
pub(super) fn write_piece_tree(
    document: &MarkdownDocumentBuffer,
    root: Option<&MarkdownPieceTreeNode>,
    destination: &mut impl Write,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // Empty text contributes no bytes or code units and cannot invoke the output callback.
        return Ok(());
    };
    match root {
        // Write an immutable backing slice directly; I/O failure aborts the caller's save transaction.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            destination.write_all(bytes).map_err(|_| {
                // 写入失败保留原模型；真实映射回收在传播此错误前完成。
                MarkdownDocumentBufferError::WriteUnavailable
            })
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 保存仍逐4KiB原片直接写入；任一写入或lease回收失败即停止，不改保存事务语义。
            for ordinal in span.record_range() {
                write_piece_tree(
                    document,
                    Some(&MarkdownPieceTreeNode::Leaf(span.piece(ordinal)?)),
                    destination,
                )?;
            }
            Ok(())
        }
        // Preserve text order and stop at the first failed child write.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            write_piece_tree(document, Some(left.as_ref()), destination)?;
            write_piece_tree(document, Some(right.as_ref()), destination)
        }
    }
}

/// Feed live bytes into a revision hash without copying or changing tree ownership.
pub(super) fn hash_piece_tree(
    document: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    hasher: &mut Sha256,
) -> Result<(), MarkdownDocumentBufferError> {
    document.source_health()?;
    let Some(root) = root else {
        // 空树不读取正文，仍是SHA-256的真实空输入。
        return Ok(());
    };
    match root {
        // Hash the leaf's exact stored bytes, preserving the document's original line endings.
        MarkdownPieceTreeNode::Leaf(piece) => document.with_piece_bytes(piece, |bytes| {
            hasher.update(bytes);
            Ok(())
        }),
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 摘要按原源字节序消费有限片；压缩元数据与树形不会进入文档revision。
            for ordinal in span.record_range() {
                hash_piece_tree(
                    document,
                    Some(&MarkdownPieceTreeNode::Leaf(span.piece(ordinal)?)),
                    hasher,
                )?;
            }
            Ok(())
        }
        // Hash children in text order so tree shape never affects the revision.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            hash_piece_tree(document, Some(left.as_ref()), hasher)?;
            hash_piece_tree(document, Some(right.as_ref()), hasher)
        }
    }
}

/// Return the root aggregate or the empty-document identity in constant time.
pub(super) fn piece_tree_metrics(root: &PieceTreeLink) -> MarkdownDocumentMetrics {
    root.as_ref()
        .map_or(MarkdownDocumentMetrics::default(), |root| {
            // Use the cached nonempty root aggregate without rescanning source bytes.
            root.metrics()
        })
}

#[cfg(test)]
/// Expose the cached tree height for balance regression assertions.
pub(super) fn piece_tree_height(root: &PieceTreeLink) -> u16 {
    root.as_ref().map_or(0, |root| {
        // Use the cached root height for structural regression measurements.
        root.height()
    })
}

#[cfg(test)]
/// Count retained leaves recursively for structural sharing regression tests.
pub(super) fn piece_tree_leaf_count(root: Option<&MarkdownPieceTreeNode>) -> usize {
    let Some(root) = root else {
        // Empty documents own no leaf allocations.
        return 0;
    };
    match root {
        // A leaf contributes one immutable piece regardless of byte length.
        MarkdownPieceTreeNode::Leaf(_) => 1,
        MarkdownPieceTreeNode::OriginalSpan(_) => 1,
        // Count both disjoint child trees to measure the full persistent shape.
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            piece_tree_leaf_count(Some(left.as_ref())) + piece_tree_leaf_count(Some(right.as_ref()))
        }
    }
}

/// Compare persistent root identity to recognize an unchanged saved document.
pub(super) fn piece_tree_links_share_root(left: &PieceTreeLink, right: &PieceTreeLink) -> bool {
    match (left, right) {
        // Two absent roots represent the same empty tree.
        (None, None) => true,
        // Pointer equality recognizes the exact saved root without rescanning text.
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        // An empty and nonempty root cannot represent an unchanged tree.
        _ => false,
    }
}
