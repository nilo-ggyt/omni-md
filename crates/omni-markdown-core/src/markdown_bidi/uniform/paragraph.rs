//! 多段level0证书只保正在消费的一段；真实B端点仍由同一冻结树的有界游标取得。

use super::*;
use crate::markdown_document_buffer::FrozenBidiParagraphCursor;

#[derive(Default)]
pub(in crate::markdown_bidi) struct PreparedParagraph {
    known: Option<(usize, Range<usize>, usize)>,
    cursor: Option<FrozenBidiParagraphCursor>,
}

pub(crate) struct ParagraphPrepareStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) ready: bool,
    pub(crate) maximum_borrow: usize,
}

impl ParagraphPrepareStep {
    pub(in crate::markdown_bidi) fn ready() -> Self {
        Self {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            ready: true,
            maximum_borrow: 0,
        }
    }
}

impl PreparedParagraph {
    pub(in crate::markdown_bidi) fn range_at(&self, position: usize) -> Option<Range<usize>> {
        self.known
            .as_ref()
            .filter(|known| known.0 == position)
            .map(|known| known.1.clone())
    }

    /// 真实同一段的范围事实可覆盖其它位置；调用方仍须独立证明目标是scalar/GCB边界。
    pub(in crate::markdown_bidi) fn range_covering(&self, position: usize) -> Option<Range<usize>> {
        self.known
            .as_ref()
            .filter(|(_, range, _)| range.contains(&position))
            .map(|(_, range, _)| range.clone())
    }

    pub(in crate::markdown_bidi) fn control_free(
        &self,
        range: &Range<usize>,
    ) -> Option<(Range<usize>, usize)> {
        self.known
            .as_ref()
            .filter(|(_, paragraph, content_end)| {
                paragraph.start <= range.start && range.end <= *content_end
            })
            .map(|(_, paragraph, content_end)| (paragraph.clone(), *content_end))
    }

    pub(in crate::markdown_bidi) fn step(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<ParagraphPrepareStep, BidiError> {
        if self.range_at(position).is_some() {
            return Ok(ParagraphPrepareStep::ready());
        }
        if self.range_covering(position).is_some() && snapshot.utf16_boundaries_are_scalar()? {
            self.known.as_mut().expect("已知真实段").0 = position;
            return Ok(ParagraphPrepareStep::ready()); // 无代理对时新端点也是已证明整数边界，不重复读取B叶。
        }
        let retained = if let Some(cursor) = &mut self.cursor {
            cursor.position() == position || cursor.advance_position(snapshot, position)?
        } else {
            false
        };
        if !retained {
            self.cursor = Some(snapshot.begin_bidi_paragraph(position)?);
        } // 只有同 Frozen、同已知 B 段的真实前进点可复用；其余请求仍支付原冷路径。
        let step = snapshot.advance_bidi_paragraph(
            self.cursor.as_mut().expect("原Frozen游标已安装"),
            budget.bytes,
            budget.scalars,
            budget.metadata_ops,
        )?;
        let ready = step.range.is_some();
        if let Some(range) = step.range {
            self.known = Some((position, range, step.content_end));
            self.cursor = self
                .cursor
                .take()
                .and_then(FrozenBidiParagraphCursor::into_forward_range);
            // 唯一游标只留下同次查询的 B 范围和 scalar 点；下一 GCB 不重找段的两端。
            // 投影仍立即释放；没有正文 lease、历史段或额外 Source owner，取消/drop 原路归还。
        }
        Ok(ParagraphPrepareStep {
            bytes: step.bytes,
            scalars: step.scalars,
            metadata: step.metadata,
            ready,
            maximum_borrow: step.maximum_borrow,
        })
    }
}
