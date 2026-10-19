//! 可打印ASCII的精确范围谓词；正聚合覆盖任意子范围，混合部分叶同时验证UTF-16端点。

use super::*;

#[cfg(feature = "paged-shaping-frontier")]
#[path = "printable_ascii/suffix.rs"]
mod suffix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use suffix::{FrozenPrintableAsciiSuffix, FrozenPrintableAsciiSuffixStep};

pub(super) fn query(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
) -> Result<bool, MarkdownDocumentBufferError> {
    source.source_health()?;
    let result = (|| {
        let length = root.map_or(0, |node| node.metrics().utf16_len);
        if range.start > range.end || range.end > length {
            // 不夹取错误范围，否则调用者可能把小范围的正面事实套到原item上。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if root.is_some_and(|node| node.metrics().all_printable_ascii.is_printable()) {
            // 原冻结root已逐scalar证明全为单UTF16单元ASCII；范围内任意整数端点都合法，
            // 包括空范围。无需为非零style/item重复读取边叶，健康仍由外层前后两次核验。
            return Ok(true);
        }
        if range.is_empty() {
            // 即使没有字符，也必须拒绝emoji低代理项等不可编辑位置；只解码一个有限片。
            byte_offset_for_utf16_in_tree(source, root, range.start, 0)?;
            return Ok(true);
        }
        within(source, root, range, 0)
    })();
    // 完整聚合可能没有借正文，仍须核对真实I/O健康；晚到的来源错误不能变成false或旧true。
    source.source_health()?;
    result
}

fn within(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    range: Range<usize>,
    base: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let Some(node) = root else {
        // 空子树只贡献AND单位元；公开入口已验证真实范围。
        return Ok(true);
    };
    let metrics = node.metrics();
    let end = base + metrics.utf16_len;
    if range.end <= base || range.start >= end {
        // 不相交路径既不读正文，也不因其包含控制符而污染当前范围。
        return Ok(true);
    }
    if metrics.all_printable_ascii.is_printable() {
        // 正事实对节点的每个交集都成立，且所有UTF16端点均是scalar边界。只有混合节点
        // 的部分叶需要实际解码；不能让外部Unicode迫使已证明ASCII的子树重复读正文。
        return Ok(true);
    }
    if range.start <= base && range.end >= end {
        // 整树边界由原标量片构造保证合法；缓存可精确覆盖巨范围而不触碰正文页。
        return Ok(metrics.all_printable_ascii.is_printable());
    }
    match node {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // 两个端点都必须验证；左侧false不能短路右侧低代理项或真实来源失败。
            let first = within(source, Some(left), range.clone(), base)?;
            let second = within(source, Some(right), range, base + left.metrics().utf16_len)?;
            Ok(first && second)
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 至多16项目录；完整小片直接用聚合，只有两端部分片进入实际字节借用。
            let mut offset = base;
            let mut printable = true;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = offset + piece.metrics.utf16_len;
                printable &= within(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    range.clone(),
                    offset,
                )?;
                offset = next;
            }
            Ok(printable)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // 一个实际小片同时核对范围两端，避免先转byte坐标再对相同正文借第二次。
            let text = std::str::from_utf8(bytes).expect("原始piece已经验证UTF-8");
            let start = range.start.saturating_sub(base);
            let end = (range.end - base).min(piece.metrics.utf16_len);
            let mut offset = 0;
            let mut start_valid = start == 0;
            let mut end_valid = end == 0;
            let mut printable = true;
            for scalar in text.chars() {
                // 只对真正所选标量计算谓词；范围外的Unicode或控制符不能影响结果。
                if offset >= start && offset < end {
                    printable &= (' '..='~').contains(&scalar);
                }
                offset += scalar.len_utf16();
                start_valid |= offset == start;
                end_valid |= offset == end;
            }
            if !start_valid || !end_valid {
                // 任一端切入代理项都不能由已有false掩盖，否则上层会误认原范围已验证。
                return Err(MarkdownDocumentBufferError::NotUnicodeBoundary);
            }
            Ok(printable)
        }),
    }
}

#[cfg(test)]
#[path = "printable_ascii/tests.rs"]
mod tests;
