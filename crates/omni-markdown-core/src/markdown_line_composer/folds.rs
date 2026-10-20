//! 原 composer 只持同 Source 折叠段落证明；各 Unicode job 共享端点，不保存第二正文。

use std::ops::Range;

use super::{MarkdownLineComposeError as Error, item::Result};
use crate::{
    MarkdownLayoutTextSource, markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS,
    markdown_layout_source::folds::FoldPlan,
};

#[derive(Default)]
pub(super) struct Plan {
    shared: Option<FoldPlan>,
}

impl Plan {
    /// 显式配置最多保原256个锚点；真实 Source 首尾 CRLF/EOF 证明由唯一共享计划准备。
    pub(super) fn new(source: &MarkdownLayoutTextSource, ranges: &[Range<usize>]) -> Result<Self> {
        source.health()?;
        if ranges.len() > MAX_MARKDOWN_FOLD_ANCHORS {
            return Err(Error::InvalidRequest);
        }
        let shared = if ranges.is_empty() {
            None
        } else {
            Some(FoldPlan::new(source, ranges)?)
        };
        Ok(Self { shared })
    }

    /// 原 Script/GCB/LB/Bidi 各保同一个弱 Source 证明与有限游标，不复制目录或另开 Source。
    pub(super) fn shared(&self) -> Option<FoldPlan> {
        self.shared.clone()
    }

    /// 只有原 Item 游标命中精确隐区起点才签控制，不从 viewport 边缘推测隐藏权限。
    pub(super) fn at(&self, position: usize) -> Option<Range<usize>> {
        let spans = self.shared.as_ref()?.spans();
        let index = spans
            .binary_search_by_key(&position, |span| span.start.utf16)
            .ok()?;
        Some(spans[index].start.utf16..spans[index].end.utf16)
    }
}
