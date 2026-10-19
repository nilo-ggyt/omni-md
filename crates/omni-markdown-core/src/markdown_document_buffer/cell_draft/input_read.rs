//! 原输入胶囊的同步有限读取；不冻结新root、不占布局的两个实际读取名额。

use super::edit::DraftSource;
use super::*;

impl MarkdownDocumentBuffer {
    /// 核对原serial并返回局部状态与正文弱身份；选区版本不冒充内容版本。
    ///
    /// # Errors
    /// 原Source失败、草稿取消、nonce或serial失配时拒绝；不会重新捕获后继权限。
    pub fn cell_draft_edit_facts(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
    ) -> Result<(MarkdownCellDraftFacts, MarkdownCellDraftSourceAuthority), MarkdownCellDraftError>
    {
        let pending = self.require_cell_draft_edit(edit)?;
        let facts = pending.facts();
        let authority = guard::source_authority(&pending.scope, facts.content_serial);
        self.require_cell_draft_edit(edit)?; // 返回前再过原健康门，缓存事实不能掩盖后台首错。
        Ok((facts, authority))
    }

    /// 单次至多4096 UTF16单元，可从低代理项开始；只复用Doc原有的一个8KiB派生窗口。
    ///
    /// # Errors
    /// 超限、越界、原票失效或Source失败时返回错误；调用方须丢弃已写入的整次候选。
    pub fn copy_cell_draft_utf16_code_units(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(edit)?;
        if destination.len() > read_cache::READ_WINDOW_UTF16_UNITS {
            return Err(MarkdownCellDraftError::ReadLimit); // 先核原Source，再拒绝无界Native请求。
        }
        let source = DraftSource {
            document: self,
            edits: &pending.edits,
            scope: &pending.scope,
            serial: pending.serial,
        };
        let result = self
            .utf16_read_cache
            .lock()
            .map_err(|_| MarkdownDocumentBufferError::Utf16ReadCacheUnavailable)
            .and_then(|mut cache| {
                // 真root弱身份与content_serial共同隔离canonical和其他草稿；命中也核Source健康。
                cache.copy(
                    &source,
                    &pending.root,
                    pending.content_serial,
                    offset,
                    destination,
                )
            });
        self.finish_cell_draft_input_read(edit, pending, result)
    }

    /// 验证草稿局部范围的真实标量端点；不导出canonical坐标或另一个编辑许可。
    ///
    /// # Errors
    /// 范围倒置、溢出、越界、切入代理对或原Source/票失效时拒绝。
    pub fn validate_cell_draft_utf16_range(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        range: Range<usize>,
    ) -> Result<(), MarkdownCellDraftError> {
        let pending = self.require_cell_draft_edit(edit)?;
        let source = DraftSource {
            document: self,
            edits: &pending.edits,
            scope: &pending.scope,
            serial: pending.serial,
        };
        let result = (|| {
            if range.start > range.end {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            for at in [range.start, range.end] {
                byte_offset_for_utf16_in_tree(&source, pending.root.as_deref(), at, 0)?;
            } // 只核两条真实端点路径；不扫描区间、不把UTF16差值当字素或glyph数量。
            Ok(())
        })();
        self.finish_cell_draft_input_read(edit, pending, result)
    }

    fn finish_cell_draft_input_read<T>(
        &self,
        edit: &MarkdownCellDraftEditCapsule,
        pending: &PendingCellDraft,
        result: Result<T, MarkdownDocumentBufferError>,
    ) -> Result<T, MarkdownCellDraftError> {
        if let Err(error) = &result {
            pending.scope.record_source(*error); // 实际errno保持sticky；过期或后继成功不能覆盖首错。
        }
        self.require_cell_draft_edit(edit)?; // cache锁已归还；空范围与EOF同样必须通过原健康与serial门。
        result.map_err(Into::into)
    }
}
