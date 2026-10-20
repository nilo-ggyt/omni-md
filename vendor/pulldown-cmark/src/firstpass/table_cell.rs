// Copyright 2015 Google Inc. All rights reserved.
// 原 firstpass::parse_line Active 入口适配；MIT 条款见 LICENSE。
//! 固定短格从真实 seed 直接进入原 inline lexer，不把格文本重新解释成 Root Markdown。

use super::*;
use crate::global_frontier::{GlobalFrontierError, InlineBlockKind, VerifiedInlineSemanticSeed};

/// 输入必须是同一 seed 的完整≤2KiB trim 格；真实源读取与归还仍由 Core owner 负责。
pub(crate) fn run<'a>(
    text: &'a str,
    seed: &VerifiedInlineSemanticSeed,
) -> Result<(Tree<Item>, Allocations<'a>), GlobalFrontierError> {
    if seed.kind() != InlineBlockKind::TableCell
        || text.len() != seed.source_range().len()
        || text.len() > 2048
    {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    let options = seed.options();
    let lookup = &create_lut(&options);
    let mut pass = first_pass(text, options, lookup, &[]);
    pass.math_context = seed
        .table_cell_math_context()
        .expect("TableCell seed 保同源数学入口");
    // 原 inline 两遍算法需要实际 Cell parent 作为 block_end；不以假 Paragraph 替代原节点。
    pass.tree.append(Item {
        start: 0,
        end: text.len(),
        body: ItemBody::TableCell,
    });
    pass.tree.push();
    let (end, line_break) = pass.parse_line(0, Some(text.len()), TableParseMode::Active);
    if end != text.len() || line_break.is_some() {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    pass.tree.pop();
    Ok((pass.tree, pass.allocs))
}
