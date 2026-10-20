//! 原worker持续追加时，只发布完整LineEnd之前的同inode只读前缀。

use super::{output::OutputSeal, *};
use crate::markdown_layout_index::LayoutIndexSnapshotError;

impl From<LayoutIndexSnapshotError> for Error {
    fn from(error: LayoutIndexSnapshotError) -> Self {
        match error {
            LayoutIndexSnapshotError::ReadersBusy => Self::SnapshotBusy,
            LayoutIndexSnapshotError::Index(error) => Self::from(error),
        } // 名额背压只属于此发布入口；其他索引消费者的typed IO分类保持原合同。
    }
}

impl GlyphStore {
    /// 没有完整行或仍有半写入capture时不交付候选；旧快照不限制后续同源追加。
    pub(in crate::markdown_line_composer) fn publish_ready_prefix(
        &mut self,
    ) -> Result<Option<MarkdownComposedLines>> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some() || self.control_capture.is_some() || self.lines == 0 {
                return Ok(None);
            } // 半item不能被截成已提交行；调用者保留worker，下一回合继续原阶段。
            let last = self.line_end.ok_or(Error::InconsistentCoverage)?;
            let records = last.checked_add(1).ok_or(Error::Overflow)?;
            let graphemes = self.published_grapheme_directory()?;
            let reader = match self.index.as_mut().ok_or(Error::Poisoned)? {
                Rows::Writing(index) => index.snapshot_reader(records)?,
                #[cfg(test)]
                Rows::Reading(_) => return Err(Error::InvalidRequest),
            };
            self.guard()?; // 真实checkpoint/dup之后仍核原源与writer，错误候选的FD随值归还。
            Ok(Some(MarkdownComposedLines::from_sealed(OutputSeal {
                source: self.source.clone(),
                authority: self.authority.clone(),
                fonts: self.fonts.clone(),
                cancellation: self.cancellation.clone(),
                reader,
                // 每次交付独立输出身份；相同inode、行号或record不能让旧Part跨快照补签。
                identity: Arc::new(()),
                #[cfg(feature = "paged-shaping-frontier")]
                lineage: Arc::downgrade(&self.identity),
                last: Some(last),
                count: self.lines,
                range: 0..self.line_source_end,
                graphemes,
                folds: self.folds.clone(),
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: self.tables.clone(),
            })?))
        })();
        if matches!(result, Err(Error::SnapshotBusy)) {
            // 两份实际Reader未还只是发布背压；健康首错仍优先，不能借此绕过源/IO撤权。
            if let Err(error) = self.guard() {
                return self.finish(Err(error));
            }
            return result;
        }
        self.finish(result)
    }
}
