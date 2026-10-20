//! 只读快照的字素覆盖独立于glyph布局范围；未迁入的尾部保留Pending而非伪EOF。
use super::*;

/// Known(None)只代表已覆盖记录域内不是 GCB；隐区 typed 拒绝，Pending 表示原记录尚未迁入。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedGraphemeOrdinalAnswer {
    Pending,
    Known(Option<usize>),
}

impl MarkdownComposedLines {
    /// 序号只属于本次明确的记录域；FoldedVisible 不含隐区内部边界，不能当全源字素计数。
    pub fn grapheme_ordinal_domain(&self) -> Result<crate::MarkdownGraphemeOrdinalDomain> {
        self.require(&self.authority)?;
        Ok(self.graphemes.domain)
    }

    fn grapheme_dimensions(&self, offset: usize) -> Result<(usize, usize)> {
        self.require(&self.authority)?;
        let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
        if offset > source.utf16_len() {
            return Err(Error::InvalidRequest);
        }
        if let Some(fold) = self
            .payload
            .as_ref()
            .and_then(|payload| payload.folds.as_ref())
            .and_then(|plan| {
                plan.spans()
                    .iter()
                    .find(|fold| fold.start.utf16 < offset && offset < fold.end.utf16)
            })
        {
            return Err(Error::FoldedRange(fold.start.utf16..fold.end.utf16));
        } // 这里只使用同 Source 的有限锚点，绝不为一次查询遍历或解析隐藏正文。
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        if let Some(tables) = &self.payload.as_ref().ok_or(Error::Poisoned)?.tables
            && tables.contains(offset, true)?
        {
            return Err(Error::TableRange);
        }
        Ok((source.byte_len(), source.utf16_len()))
    }

    /// ordinal 属于显式 Source/FoldedVisible 域；非 BMP 与多 scalar GCB 沿原 BreakJob 记录。
    pub fn grapheme_ordinal_answer(
        &mut self,
        offset: usize,
    ) -> Result<MarkdownComposedGraphemeOrdinalAnswer> {
        let result = (|| {
            let (bytes, units) = self.grapheme_dimensions(offset)?;
            if offset > self.graphemes.covered.utf16 {
                return Ok(MarkdownComposedGraphemeOrdinalAnswer::Pending);
            }
            let (ordinal, value) =
                graphemes::lower_bound(self.graphemes, offset, bytes, units, |record| {
                    self.row(record)
                })?;
            Ok(MarkdownComposedGraphemeOrdinalAnswer::Known(
                value.filter(|value| value.utf16 == offset).map(|_| ordinal),
            ))
        })();
        self.finish(result)
    }

    /// 兼容已准备位置的旧入口。缺记录是可恢复背压，故此特定typed错误不经finish
    /// 永久撤权；任何真实Source/FD/取消失败仍由新入口原门处理并归还reader。
    pub fn grapheme_ordinal(&mut self, offset: usize) -> Result<Option<usize>> {
        match self.grapheme_ordinal_answer(offset)? {
            MarkdownComposedGraphemeOrdinalAnswer::Known(value) => Ok(value),
            MarkdownComposedGraphemeOrdinalAnswer::Pending => Err(Error::GraphemePrefixPending),
        }
    }

    pub fn grapheme_query(
        &mut self,
        offset: usize,
        query: crate::MarkdownGraphemeQuery,
    ) -> Result<crate::MarkdownGraphemeAnswer> {
        use crate::{MarkdownGraphemeAnswer as Answer, MarkdownGraphemeQuery as Query};
        let result = (|| {
            let (bytes, units) = self.grapheme_dimensions(offset)?;
            if offset > self.graphemes.covered.utf16 {
                return Ok(Answer::Pending);
            }
            let (ordinal, value) =
                graphemes::lower_bound(self.graphemes, offset, bytes, units, |record| {
                    self.row(record)
                })?;
            let index = match query {
                Query::IsBoundary => {
                    return Ok(Answer::Known(value.filter(|value| value.utf16 == offset)));
                }
                Query::Previous => ordinal.checked_sub(1),
                Query::Next => {
                    let next = if value.is_some_and(|value| value.utf16 == offset) {
                        ordinal.checked_add(1).ok_or(Error::Overflow)?
                    } else {
                        ordinal
                    };
                    if next == self.graphemes.count && !self.graphemes.complete {
                        return Ok(Answer::Pending);
                    } // 已覆盖末边界不是源EOF；不能把没有复制的后继答为Known(None)。
                    Some(next)
                }
            };
            let value = match index {
                None => None, // 真实SOT之前没有边界。
                Some(index) => {
                    graphemes::ordinal_boundary(self.graphemes, index, bytes, units, |record| {
                        self.row(record)
                    })?
                }
            };
            Ok(Answer::Known(value))
        })();
        self.finish(result)
    }
}
