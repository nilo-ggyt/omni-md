//! 只读快照的字素覆盖独立于glyph布局范围；未迁入的尾部保留Pending而非伪EOF。
use super::*;

/// Known(None)只代表已覆盖位置不是原GCB；Pending代表此快照没有足够原记录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedGraphemeOrdinalAnswer {
    Pending,
    Known(Option<usize>),
}

impl MarkdownComposedLines {
    fn grapheme_dimensions(&self, offset: usize) -> Result<(usize, usize)> {
        self.require(&self.authority)?;
        let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
        if offset > source.utf16_len() {
            return Err(Error::InvalidRequest);
        }
        Ok((source.byte_len(), source.utf16_len()))
    }

    /// 原全局ordinal不由UTF16差推算；非BMP和多scalar GCB都沿原BreakJob记录。
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
