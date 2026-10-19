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
        if self
            .cursor
            .as_ref()
            .is_none_or(|cursor| cursor.position() != position)
        {
            self.cursor = Some(snapshot.begin_bidi_paragraph(position)?);
        } // 新请求只替换自己的端点游标，不把旧范围签给另一个目标位置。
        let step = snapshot.advance_bidi_paragraph(
            self.cursor.as_mut().expect("原Frozen游标已安装"),
            budget.bytes,
            budget.scalars,
            budget.metadata_ops,
        )?;
        let ready = step.range.is_some();
        if let Some(range) = step.range {
            self.known = Some((position, range, step.content_end));
            drop(self.cursor.take()); // 真实端点交付后立即归还树路径，不积累历史段或正文资源。
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
