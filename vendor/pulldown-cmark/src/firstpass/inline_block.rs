// Copyright 2015 Google Inc. All rights reserved.
// 真 scoped Paragraph/Setext 共用原 firstpass；完整 MIT 条款见 LICENSE。

//! 完整 raw 短块的原续行状态已经分量子闭合；这里不建立祖先 AST 或再次分类 Root 块。

use super::*;
use crate::global_frontier::{GlobalFrontierError, InlineBlockKind, InlineParagraphContext};

pub(crate) fn run<'a>(
    text: &'a str,
    context: &InlineParagraphContext,
) -> Result<(Tree<Item>, Allocations<'a>), GlobalFrontierError> {
    let seed = context.seed();
    if !context.is_complete() || text.len() != seed.physical_range().len() || text.len() > 2048 {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    let lookup = create_lut(&seed.options());
    let mut pass = first_pass(text, seed.options(), &lookup, &[]);
    pass.paragraph_context = Some(context);
    let start = seed.source_range().start - seed.physical_range().start;
    if !text.is_char_boundary(start) {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    let end = pass.parse_paragraph(start, None);
    if let Some(error) = pass.paragraph_seed_error {
        return Err(error);
    }
    let node = pass.tree.cur().ok_or(GlobalFrontierError::InvalidFrame)?;
    let kind_matches = match (seed.kind(), pass.tree[node].item.body) {
        (InlineBlockKind::Paragraph, ItemBody::Paragraph) => true,
        (InlineBlockKind::SetextHeading(expected), ItemBody::Heading(actual, _)) => {
            expected == actual
        }
        _ => false,
    };
    if end != text.len() || !kind_matches {
        return Err(GlobalFrontierError::InvalidFrame);
    } // 原最终 kind 与真实闭合末端都必须保持，不能拿回退 Paragraph 掩盖 Setext 消费失败。
    Ok((pass.tree, pass.allocs))
}
