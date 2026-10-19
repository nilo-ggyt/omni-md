//! 草稿只暴露封闭的局部读取胶囊；没有转成canonical Source或取出普通snapshot的接口。

use super::*;

struct ReadPayload {
    // 实际读取名额在Frozen末字段；内部snapshot clone不能因这个外层payload退场而提前归还。
    snapshot: MarkdownTextSnapshot,
    facts: MarkdownCellDraftFacts,
    scope: Arc<Scope>,
}

#[derive(Clone)]
/// clone共享同一个实际读取payload；原编辑或取消撤权不改变已持字节，但拒绝继续读取。
pub struct MarkdownCellDraftReadCapsule {
    payload: Arc<ReadPayload>,
}

#[derive(Clone)]
/// 仅当前capture的弱身份；坐标、相同长度或普通Source票都不能构造它。
pub struct MarkdownCellDraftReadAuthority {
    payload: Weak<ReadPayload>,
}

impl MarkdownDocumentBuffer {
    /// 为原草稿当前content_serial固定只读根；状态修改不撤正文，真实编辑会撤销旧读取。
    pub fn capture_cell_draft(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftReadCapsule, MarkdownCellDraftError> {
        let pending = self.active_cell_draft(token)?;
        let claim = pending.scope.reserve_reader()?;
        let guard = FrozenDraftGuard::new(
            Arc::clone(&pending.scope),
            pending.content_serial,
            self.original_snapshot.clone(),
        );
        let snapshot = MarkdownTextSnapshot::capture_draft_root(
            self,
            &pending.root,
            &pending.edits,
            pending.content_serial,
            guard,
            claim,
        );
        // 即使candidate已不引用Original，原Source的sticky I/O仍必须先于cache hit或Stale。
        let payload = Arc::new(ReadPayload {
            snapshot,
            facts: pending.facts(),
            scope: Arc::clone(&pending.scope),
        });
        pending.require(self)?;
        Ok(MarkdownCellDraftReadCapsule { payload })
    }
}

impl MarkdownCellDraftReadCapsule {
    #[must_use]
    /// 票据不保活胶囊；不同capture即使同serial也不能互换。
    pub fn authority(&self) -> MarkdownCellDraftReadAuthority {
        MarkdownCellDraftReadAuthority {
            payload: Arc::downgrade(&self.payload),
        }
    }

    fn require(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<(), MarkdownCellDraftError> {
        self.payload
            .snapshot
            .original_backing_health()
            .inspect_err(|&error| self.payload.scope.record_source(error))
            .map_err(|error| match error {
                MarkdownDocumentBufferError::PreparedTextInputStale => {
                    MarkdownCellDraftError::Stale
                }
                other => other.into(),
            })?;
        self.payload
            .scope
            .require_content(self.payload.facts.content_serial)?;
        if !Weak::ptr_eq(&authority.payload, &Arc::downgrade(&self.payload)) {
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    fn finish<T>(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        result: Result<T, MarkdownDocumentBufferError>,
    ) -> Result<T, MarkdownCellDraftError> {
        if let Err(error) = &result {
            self.payload.scope.record_source(*error);
        }
        // 先保原I/O错误；回调期间的撤权仍阻止部分候选返回成功。
        self.require(authority)?;
        result.map_err(Into::into)
    }

    /// 只返回捕获时的局部事实；selection可能早于当前输入状态，不授予canonical或编辑权限。
    pub fn facts(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require(authority)?;
        Ok(self.payload.facts.clone())
    }

    /// 原cell局部正文身份；相同长度、普通Source或裸范围都不能创建这张弱票。
    pub fn source_authority(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownCellDraftSourceAuthority, MarkdownCellDraftError> {
        self.require(authority)?;
        self.payload
            .snapshot
            .cell_draft_source_authority()
            .ok_or(MarkdownCellDraftError::Stale)
    }

    /// 只给布局Source受控factory共享同一Frozen；真实ReadClaim仍在最后snapshot归还时结算。
    #[cfg(any(
        feature = "paged-graphemes",
        feature = "paged-line-breaks",
        feature = "paged-shaping"
    ))]
    pub(crate) fn layout_snapshot(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownTextSnapshot, MarkdownCellDraftError> {
        self.require(authority)?;
        Ok(self.payload.snapshot.clone())
    }

    /// 单次最多4096UTF16单元；代理对内的半个单元范围仍是合法NSString读取。
    /// 出错时destination可能已有前缀，调用方必须丢弃整次候选。
    pub fn copy_utf16_code_units(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownCellDraftError> {
        self.require(authority)?;
        if destination.len() > read_cache::READ_WINDOW_UTF16_UNITS {
            return Err(MarkdownCellDraftError::ReadLimit);
        }
        self.finish(
            authority,
            self.payload
                .snapshot
                .copy_utf16_code_units(offset, destination),
        )
    }

    /// 单次最多4KiB，回调仍按原小片执行；Err禁止调用方提交已观察的部分字节。
    pub fn visit_byte_range(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        range: Range<usize>,
        visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownCellDraftError> {
        self.require(authority)?;
        if range.end.saturating_sub(range.start) > ORIGINAL_PIECE_BYTES {
            return Err(MarkdownCellDraftError::ReadLimit);
        }
        self.finish(
            authority,
            self.payload.snapshot.visit_byte_range(range, visit),
        )
    }

    /// 标量坐标映射仅查实际有限叶，拒绝surrogate或UTF-8标量内部边界。
    pub fn byte_offset_for_utf16(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
    ) -> Result<usize, MarkdownCellDraftError> {
        self.require(authority)?;
        self.finish(
            authority,
            self.payload.snapshot.byte_offset_for_utf16(offset),
        )
    }

    /// 从真实字节边界映射局部UTF16；不补签任何文档或表格全局范围。
    pub fn utf16_offset_for_byte(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
    ) -> Result<usize, MarkdownCellDraftError> {
        self.require(authority)?;
        self.finish(
            authority,
            self.payload.snapshot.utf16_offset_for_byte(offset),
        )
    }
}
