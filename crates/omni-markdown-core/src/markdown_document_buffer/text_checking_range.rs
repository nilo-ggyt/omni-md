//! 冻结值的文本检查段落定位；聚合跳过无终止符子树，不读取或复制整段正文。

use super::*;

/// 聚合已经给出位置时暂不读取字符；叶内查找则顺手保留真实BMP终止单元。
struct Terminator {
    offset: usize,
    unit: Option<u16>,
}

/// 活动及冻结source均可复用的同一范围算法；本切片只由冻结值公开API消费。
pub(super) fn paragraph_range(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    request: Range<usize>,
) -> Result<Range<usize>, MarkdownDocumentBufferError> {
    source.source_health()?;
    let length = root.map_or(0, |node| node.metrics().utf16_len);
    if request.start > request.end || request.end > length {
        // 原生已完成标准夹取；此处必须先拒绝错误坐标，不能读字节后再补范围。
        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
    }
    let start = previous_terminator(source, root, request.start, 0)?.map_or(0, |offset| offset + 1);
    let anchor = if request.is_empty() {
        request.end // 空caret查看当前位置；EOF本身不构造不存在的单元。
    } else {
        request.end - 1 // 非空请求不能把紧邻下一段的范围结束边界误当作选中了下一段。
    };
    let Some(terminator) = next_terminator(source, root, anchor, 0)? else {
        // 无后继终止符时EOF就是尾段边界；巨单段通过零聚合直接抵达这里，不触碰正文页。
        return Ok(start..length);
    };
    let mut end = terminator.offset + 1;
    let unit = match terminator.unit {
        Some(unit) => unit, // 叶内命中已经取得字符，不重复读取同一个叶。
        None => utf16_code_unit_in_tree(source, root, terminator.offset)?,
    };
    // CRLF可以跨原始叶、编辑叶或source；短路条件只多读一个真实后继单元。
    if unit == u16::from(b'\r')
        && end < length
        && utf16_code_unit_in_tree(source, root, end)? == u16::from(b'\n')
    {
        end += 1;
    }
    Ok(start..end)
}

/// 找严格位于before之前的最后一个终止符；首尾聚合也排除部分叶中的错误一侧。
fn previous_terminator(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    before: usize,
    base: usize,
) -> Result<Option<usize>, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metrics = root.metrics();
    if metrics.text_checking_terminators == 0 || before <= base {
        // 空子树或起点之前没有候选；不为了验证“没有分隔符”读取该子树的叶。
        return Ok(None);
    }
    let first = base
        + metrics
            .first_text_checking_terminator
            .expect("a nonempty terminator aggregate has a first offset")
            .get()
        - 1;
    let last = base
        + metrics
            .last_text_checking_terminator
            .expect("a nonempty terminator aggregate has a last offset")
            .get()
        - 1;
    if last < before {
        return Ok(Some(last)); // 整个子树的尾端点已是所求结果，不解码其正文。
    }
    if first >= before {
        return Ok(None); // 该部分子树的全部终止符都在请求之后，不能先读一个失败叶。
    }
    match root {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // 从较近的右侧开始；其聚合排除后再查左侧，AVL高度限制元数据下降深度。
            match previous_terminator(source, Some(right), before, base + left.metrics().utf16_len)?
            {
                Some(offset) => Ok(Some(offset)),
                None => previous_terminator(source, Some(left), before, base),
            }
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 反向先看较近的小片；每片聚合先剪枝，只有夹住before的单片可能读取正文。
            let mut end = base + span.metrics.utf16_len;
            for ordinal in span.record_range().rev() {
                let piece = span.piece(ordinal)?;
                end -= piece.metrics.utf16_len;
                if let Some(offset) = previous_terminator(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    before,
                    end,
                )? {
                    return Ok(Some(offset));
                }
            }
            Ok(None)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // 已证明first<before<=last，该唯一命中叶必有目标；正文借用最多原4KiB。
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut offset = base;
            let mut found = None;
            for character in text.chars() {
                if offset >= before {
                    break; // 请求可以位于emoji低代理项，只比较UTF-16坐标，不要求编辑边界。
                }
                if is_terminator(character) {
                    found = Some(offset);
                }
                offset += character.len_utf16();
            }
            Ok(Some(found.expect(
                "the immutable first terminator precedes the request",
            )))
        }),
    }
}

/// 找位于at或其后的首个终止符；只有终止符夹住目标的单个叶才需要正文解码。
fn next_terminator(
    source: &impl PieceByteSource,
    root: Option<&MarkdownPieceTreeNode>,
    at: usize,
    base: usize,
) -> Result<Option<Terminator>, MarkdownDocumentBufferError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metrics = root.metrics();
    if metrics.text_checking_terminators == 0 || at >= base + metrics.utf16_len {
        // EOF及无终止符子树没有候选；不访问原mmap或编辑段的任何正文页。
        return Ok(None);
    }
    let first = base
        + metrics
            .first_text_checking_terminator
            .expect("a nonempty terminator aggregate has a first offset")
            .get()
        - 1;
    let last = base
        + metrics
            .last_text_checking_terminator
            .expect("a nonempty terminator aggregate has a last offset")
            .get()
        - 1;
    if first >= at {
        return Ok(Some(Terminator {
            offset: first,
            unit: None,
        })); // 精确位置来自同一冻结root，字符只在末端CRLF判定真正需要时读取。
    }
    if last < at {
        return Ok(None); // 终止符虽存在却全部在目标之前，剪枝后不读失败叶。
    }
    match root {
        MarkdownPieceTreeNode::Branch { left, right, .. } => {
            // 正向先查左侧，只有其真实聚合排除后才进入右侧；不顺序扫描中间正文。
            match next_terminator(source, Some(left), at, base)? {
                Some(terminator) => Ok(Some(terminator)),
                None => next_terminator(source, Some(right), at, base + left.metrics().utf16_len),
            }
        }
        MarkdownPieceTreeNode::OriginalSpan(span) => {
            // 正向寻找第一个真实终止符；目录只提供已验证计量，不复制整个段落。
            let mut offset = base;
            for ordinal in span.record_range() {
                let piece = span.piece(ordinal)?;
                let next = offset + piece.metrics.utf16_len;
                if let Some(found) = next_terminator(
                    source,
                    Some(&MarkdownPieceTreeNode::Leaf(piece)),
                    at,
                    offset,
                )? {
                    return Ok(Some(found));
                }
                offset = next;
            }
            Ok(None)
        }
        MarkdownPieceTreeNode::Leaf(piece) => source.with_piece_bytes(piece, |bytes| {
            // first<at<=last保证此叶确实命中，不会再解码相邻叶寻找同一个端点。
            let text = std::str::from_utf8(bytes).expect("piece bytes remain valid UTF-8");
            let mut offset = base;
            for character in text.chars() {
                if offset >= at && is_terminator(character) {
                    return Ok(Some(Terminator {
                        offset,
                        unit: Some(character as u16),
                    })); // 三种终止符均在BMP，顺手保留其真实单元省去重复的字符查询。
                }
                offset += character.len_utf16();
            }
            unreachable!("the immutable last terminator follows the request")
        }),
    }
}

fn is_terminator(character: char) -> bool {
    matches!(character, '\r' | '\n' | '\u{2029}') // NEL和LS不属于文本检查段落终止集合。
}

#[cfg(test)]
#[path = "text_checking_range/tests.rs"]
mod tests;
