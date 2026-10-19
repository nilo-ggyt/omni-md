//! 流式多范围替换；旧 canonical root 只读，所有候选输出一次发布或完整回滚。

use super::changes::{MarkdownObservedEditError, MarkdownTextChangeObserver};
use super::command_transaction::{PreparedCommand, commit_prepared_command_observed};
use super::formatting::streaming::SourcePieceCursor;
use super::*;
use std::borrow::Cow;

#[derive(Debug)]
/// 一项原文 UTF-16 替换；parts 完整消费后才读取下一项，不保存全量匹配列表。
pub struct MarkdownReplacement<P> {
    pub range_utf16: Range<usize>,
    pub parts: P,
}

#[derive(Debug, Eq, PartialEq)]
/// 字面块或旧 canonical root 的引用区间；Cow 可借动作外层模板，core 不保留该借用。
/// 大区间共享原 piece，短片段复制合并进正常 canonical chunk；Owned String 调用方可用 `.into()`。
pub enum MarkdownReplacementPart<'a> {
    Literal(Cow<'a, str>),
    SourceRange(Range<usize>),
}

impl MarkdownDocumentBuffer {
    /// 将有序、不重叠的原文范围替换为一个原子 history entry。
    ///
    /// 两级 iterator 均只读普通数据，不得回调 native 或重入同一 document。调用方须在取得
    /// mutable document 前检查匹配时的 generation。任意输入、边界、大小、预算或最终选区
    /// 错误保留原文、redo、选区与 generation；空输入验证选区后返回 None，不改变状态。
    /// 巨型 SourceRange 共享旧 subtree，小片段通过 4 KiB scratch 合并，避免密集匹配各占 leaf。
    pub fn apply_replacements<'a, I, P>(
        &mut self,
        replacements: I,
        before: MarkdownTextSelection,
        after: MarkdownTextSelection,
    ) -> Result<Option<MarkdownHistoryTextEdit>, MarkdownDocumentBufferError>
    where
        I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
        P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
    {
        self.apply_replacements_observed(replacements, before, after, changes::NoopObserver)
            .map(|(edit, ())| edit)
            .map_err(changes::legacy_error)
    }

    /// 原子替换的可失败观察入口；每项parts消费完成后立即发出真实变化，不驻留匹配列表。
    pub fn apply_replacements_observed<'a, I, P, O>(
        &mut self,
        replacements: I,
        before: MarkdownTextSelection,
        after: MarkdownTextSelection,
        observer: O,
    ) -> changes::MarkdownObservedReplacementsResult<O>
    where
        I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
        P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
        O: MarkdownTextChangeObserver,
    {
        self.ensure_text_input_idle()?;
        self.validate_selection(before)?;
        let mut replacements = replacements.into_iter();
        let Some(first) = replacements.next() else {
            // 空输入不采用after/清redo；仍允许观察者根据显式head准备派生状态，失败不发布。
            self.validate_selection(after)?;
            let output = observer
                .finish(
                    changes::MarkdownPreparedText {
                        document: self,
                        root: &self.root,
                    },
                    self.selection,
                )
                .map_err(MarkdownObservedEditError::Observer)?;
            return Ok((None, output));
        };
        commit_prepared_command_observed(self, before, observer, |document, observer| {
            let original = document.root.clone();
            prepare_replacements(
                document,
                original,
                std::iter::once(first).chain(replacements),
                after,
                observer,
            )
        })
        .map(|(edit, output)| (Some(edit), output))
    }
}

/// 一次消费原文坐标；顺序边界不反复从同一 leaf 开头解码，密集匹配只扫描标量一次。
struct OrderedBoundaries {
    cursor: SourcePieceCursor,
    piece: Option<MarkdownPiece>,
    local_byte: usize,
    remaining_units: usize,
    utf16: usize,
    byte: usize,
}

impl OrderedBoundaries {
    fn new(root: PieceTreeLink, byte_len: usize) -> Self {
        Self {
            cursor: SourcePieceCursor::new(root, 0..byte_len),
            piece: None,
            local_byte: 0,
            remaining_units: 0,
            utf16: 0,
            byte: 0,
        }
    }

    fn byte_for_utf16(
        &mut self,
        document: &MarkdownDocumentBuffer,
        target: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        if target < self.utf16 {
            // 外层本应先检查原文次序；游标也拒绝倒退，不能将新坐标误解为相对偏移。
            return Err(MarkdownDocumentBufferError::ReplacementRangesNotOrdered);
        }
        while self.utf16 < target {
            if self.piece.is_none() {
                // 完整 leaf 只保留描述符，没有字节借用跨越后续 append。
                let source = self
                    .cursor
                    .next_piece()
                    .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
                self.remaining_units = source.piece.metrics.utf16_len;
                self.local_byte = 0;
                self.piece = Some(source.piece);
            }
            let piece = self.piece.as_ref().expect("the cursor installed one leaf");
            if target - self.utf16 >= self.remaining_units {
                // 使用聚合跳过完整尾部，不读取没有边界落入的原文页。
                self.utf16 += self.remaining_units;
                self.byte += piece.byte_len - self.local_byte;
                self.piece = None;
                continue;
            }
            document.with_piece_bytes(piece, |bytes| {
                let text = std::str::from_utf8(&bytes[self.local_byte..])
                    .expect("canonical leaf boundaries remain UTF-8 aligned");
                for scalar in text.chars() {
                    let units = scalar.len_utf16();
                    if self.utf16 + units > target {
                        // surrogate 内部不能被解释为可替换边界，整个事务随后回滚。
                        return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
                    }
                    self.utf16 += units;
                    self.byte += scalar.len_utf8();
                    self.local_byte += scalar.len_utf8();
                    self.remaining_units -= units;
                    if self.utf16 == target {
                        // 下一边界从已消费位置继续，避免每个字符匹配都扫描整个 4 KiB leaf。
                        break;
                    }
                }
                Ok(())
            })?;
        }
        Ok(self.byte)
    }
}

/// 只有一个输出 scratch；长原文共享 metadata，短原文复制前另用一个固定 scratch 结束借用。
struct ReplacementOutput {
    root: PieceTreeLink,
    scratch: [u8; ORIGINAL_PIECE_BYTES],
    length: usize,
    byte_len: usize,
}

impl ReplacementOutput {
    fn new() -> Self {
        Self {
            root: None,
            scratch: [0; ORIGINAL_PIECE_BYTES],
            length: 0,
            byte_len: 0,
        }
    }

    fn grow(
        &mut self,
        additional: usize,
        maximum: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.byte_len = self
            .byte_len
            .checked_add(additional)
            .filter(|length| *length <= maximum)
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        Ok(())
    }

    fn literal(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
        mut text: &str,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // 输出只会增长，后续删除不会移除已发出的前缀；超限可在分配/复制前拒绝。
        self.grow(text.len(), document.maximum_document_bytes)?;
        while !text.is_empty() {
            let mut end = text.len().min(self.scratch.len() - self.length);
            while !text.is_char_boundary(end) {
                // 最多退让一个 scalar 的 UTF-8 尾部，不把 surrogate/字节碎片写进 leaf。
                end -= 1;
            }
            if end == 0 {
                // 剩余空间容不下下个 scalar，先 flush；空 scratch 至少能容纳四字节 scalar。
                self.flush(document)?;
                continue;
            }
            self.scratch[self.length..self.length + end].copy_from_slice(&text.as_bytes()[..end]);
            self.length += end;
            text = &text[end..];
            if self.length == self.scratch.len() {
                // 按块追加 canonical 字节，密集小替换不会各分配一个 leaf。
                self.flush(document)?;
            }
        }
        Ok(())
    }

    fn source(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
        original: &PieceTreeLink,
        range: Range<usize>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if range.is_empty() {
            // 空 source span 不生成 leaf 或 append，也不会消费 scratch 容量。
            return Ok(());
        }
        if range.len() >= ORIGINAL_PIECE_BYTES {
            self.grow(range.len(), document.maximum_document_bytes)?;
            self.flush(document)?;
            // 两次 path-copy 只切边界；中间旧 subtree 和 backing 字节均继续共享。
            let (_, suffix) = split_piece_tree(document, original.clone(), range.start)?;
            let (selected, _) = split_piece_tree(document, suffix, range.len())?;
            self.root = join_piece_trees(self.root.take(), selected);
            return Ok(());
        }
        let mut scratch = [0_u8; ORIGINAL_PIECE_BYTES];
        let mut copied = 0;
        visit_piece_tree_range(document, original.as_deref(), range, 0, &mut |bytes| {
            // 先结束只读 source 借用，再允许 literal flush 扩展 edit segment Vec。
            scratch[copied..copied + bytes.len()].copy_from_slice(bytes);
            copied += bytes.len();
            Ok(())
        })?;
        let text = std::str::from_utf8(&scratch[..copied])
            .expect("validated source spans contain whole Unicode scalars");
        self.literal(document, text)
    }

    fn flush(
        &mut self,
        document: &mut MarkdownDocumentBuffer,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if self.length == 0 {
            // 纯 SourceRange 命令不创建任何新字节，包含完整文档 capture 的重复使用。
            return Ok(());
        }
        let text = std::str::from_utf8(&self.scratch[..self.length])
            .expect("scratch ends at a complete Unicode scalar");
        let leaves = document
            .edits
            .append(text, document.maximum_document_bytes)?;
        self.root = join_piece_trees(self.root.take(), build_balanced_piece_tree(leaves));
        self.length = 0;
        Ok(())
    }
}

fn prepare_replacements<'a, I, P, O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    original: PieceTreeLink,
    replacements: I,
    after: MarkdownTextSelection,
    observer: &mut O,
) -> Result<PreparedCommand, MarkdownObservedEditError<O::Error>>
where
    I: IntoIterator<Item = Result<MarkdownReplacement<P>, MarkdownDocumentBufferError>>,
    P: IntoIterator<Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>>,
{
    let original_bytes = document.byte_len();
    let original_units = document.utf16_len();
    let mut boundaries = OrderedBoundaries::new(original.clone(), original_bytes);
    let mut output = ReplacementOutput::new();
    let mut previous: Option<Range<usize>> = None;
    let mut first_start = 0;
    let mut consumed_bytes = 0;
    for replacement in replacements {
        let MarkdownReplacement { range_utf16, parts } = replacement?;
        if range_utf16.start > range_utf16.end || range_utf16.end > original_units {
            // 先查完整范围，不允许无符号 len 计算掩盖倒置或越界。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
        }
        if let Some(previous) = &previous {
            if range_utf16.start < previous.end
                || (previous.is_empty()
                    && range_utf16.is_empty()
                    && previous.start == range_utf16.start)
            {
                // touching 允许，同位置的连续空插入及倒退/重叠都明确拒绝。
                return Err(MarkdownDocumentBufferError::ReplacementRangesNotOrdered.into());
            }
        } else {
            // 非空外层 iterator 才进入本函数，第一项建立唯一 native 通知包络。
            first_start = range_utf16.start;
        }
        let start = boundaries.byte_for_utf16(document, range_utf16.start)?;
        let end = boundaries.byte_for_utf16(document, range_utf16.end)?;
        output.source(document, &original, consumed_bytes..start)?;
        let mut inserted = 0_usize;
        for part in parts {
            match part? {
                MarkdownReplacementPart::Literal(text) => {
                    // 借用模板只在此次调用内可达，Owned 值消费后立即 drop；二者均不存入 history/root。
                    inserted = inserted
                        .checked_add(text.encode_utf16().count())
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                    output.literal(document, &text)?;
                }
                MarkdownReplacementPart::SourceRange(range) => {
                    if range.start > range.end || range.end > original_units {
                        // capture/prefix/suffix 也必须在旧 snapshot 内，不能逃到本次输出。
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument.into());
                    }
                    inserted = inserted
                        .checked_add(range.len())
                        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
                    let start = document.byte_offset_for_utf16(range.start)?;
                    let end = document.byte_offset_for_utf16(range.end)?;
                    output.source(document, &original, start..end)?;
                }
            }
        }
        // 一项完整parts的实际长度确定后立即记录；输入/observer失败由共享事务回滚全部append。
        changes::emit(document, observer, range_utf16.clone(), inserted)?;
        consumed_bytes = end;
        previous = Some(range_utf16);
    }
    let last = previous.expect("the public entry passes at least one replacement");
    output.source(document, &original, consumed_bytes..original_bytes)?;
    output.flush(document)?;
    let result_units = piece_tree_metrics(&output.root).utf16_len;
    let outside_units = first_start
        .checked_add(original_units - last.end)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    let inserted_units = result_units
        .checked_sub(outside_units)
        .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
    let metrics =
        MarkdownReplacementMetrics::new(first_start, last.end - first_start, inserted_units)?;
    Ok(PreparedCommand {
        root: output.root,
        after,
        metrics,
    })
}

#[cfg(test)]
mod tests;
