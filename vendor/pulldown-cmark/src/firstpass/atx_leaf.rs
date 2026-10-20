// Copyright 2015 Google Inc. All rights reserved.
// 真 ATX Leaf 共用原 firstpass 标题入口；完整 MIT 条款见 LICENSE。
//! 只从原闭合 Leaf 的 opening 进入原算法；不裁掉容器前缀后重新执行 Root 块分类。

use super::*;
use crate::global_frontier::{GlobalFrontierError, LeafBlockKind, VerifiedGlobalLeafBlock};

pub(crate) fn run<'a>(
    text: &'a str,
    leaf: &VerifiedGlobalLeafBlock,
) -> Result<(Tree<Item>, Allocations<'a>), GlobalFrontierError> {
    let LeafBlockKind::AtxHeading(level) = leaf.kind() else {
        return Err(GlobalFrontierError::InvalidFrame);
    };
    let source = leaf.source_range();
    if text.len() > 2048 || text.len() != source.end - leaf.physical_start() {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    let start = source.start - leaf.physical_start();
    // 这里的原字节只用于核实际输入对应 opening；标题级别、闭合与原 scope 仍来自真实 Leaf。
    // Core 持同源读取与 generation 权限，vendor 不据相同长度或相似前缀制造另一份来源票。
    if !text.is_char_boundary(start)
        || text.as_bytes().get(start..start + level as usize) != Some(&b"######"[..level as usize])
    {
        return Err(GlobalFrontierError::InvalidFrame);
    }
    let lookup = create_lut(&leaf.options());
    let mut pass = first_pass(text, leaf.options(), &lookup, &[]);
    let end = pass.parse_atx_heading(start, level);
    if end != text.len() {
        return Err(GlobalFrontierError::InvalidFrame);
    } // 单个真实 Leaf 必须完整消费；不把剩余 prefix 或邻块悄悄解释为第二个 Root 节点。
    Ok((pass.tree, pass.allocs))
}
