//! 所有冻结读取共享的封闭草稿门；只有原capture能签发，不让Bidi等内部snapshot克隆绕过撤权。

use super::*;

#[derive(Clone)]
/// cell局部正文域的弱身份与固定内容版本；不是canonical文档范围或另一个编辑许可。
pub struct MarkdownCellDraftSourceAuthority {
    scope: Weak<Scope>,
    content_serial: u64,
}

impl PartialEq for MarkdownCellDraftSourceAuthority {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.scope, &other.scope) && self.content_serial == other.content_serial
    }
}
impl Eq for MarkdownCellDraftSourceAuthority {}

/// 只由同草稿的受控入口签发；不从坐标、Doc代次或另一个冻结读取票推导身份。
pub(super) fn source_authority(
    scope: &Arc<Scope>,
    content_serial: u64,
) -> MarkdownCellDraftSourceAuthority {
    MarkdownCellDraftSourceAuthority {
        scope: Arc::downgrade(scope),
        content_serial,
    }
}

impl MarkdownCellDraftSourceAuthority {
    #[must_use]
    /// 只观察捕获的标量版本；是否仍可读取须经原Source的真实健康门核验。
    pub fn content_serial(&self) -> u64 {
        self.content_serial
    }

    pub(crate) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        let scope = self
            .scope
            .upgrade()
            .ok_or(MarkdownDocumentBufferError::PreparedTextInputStale)?;
        scope_health(&scope, self.content_serial)
    }
}

fn scope_health(scope: &Scope, serial: u64) -> Result<(), MarkdownDocumentBufferError> {
    scope.require_content(serial).map_err(|error| match error {
        MarkdownCellDraftError::Document(error) => error,
        // 内部Snapshot统一保Core错误；只检查固定原子scope，不捕获Doc、Source或候选root。
        _ => MarkdownDocumentBufferError::PreparedTextInputStale,
    })
}

/// 只有固定原子scope和同匿名backing；不保活Doc、历史或Native对象。
pub(in crate::markdown_document_buffer) struct FrozenDraftGuard {
    scope: Arc<Scope>,
    content_serial: u64,
    health_backing: Option<Arc<OriginalBacking>>,
}

impl FrozenDraftGuard {
    pub(super) fn new(
        scope: Arc<Scope>,
        content_serial: u64,
        health_backing: Option<Arc<OriginalBacking>>,
    ) -> Self {
        Self {
            scope,
            content_serial,
            health_backing,
        }
    }

    pub(in crate::markdown_document_buffer) fn require_content(
        &self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        // Frozen已经按顺序核过真实backing及编辑段；这里只检查同一scope，避免重复健康遍历。
        scope_health(&self.scope, self.content_serial)
    }

    /// Frozen先核所有实际正文资源再核撤权；原始backing即使不在当前root中也保首因。
    pub(in crate::markdown_document_buffer) fn backing_health(
        &self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(backing) = &self.health_backing {
            // 即使root只剩编辑段，原始Source首错也压过内容撤权或已命中的派生cache。
            backing
                .health()
                .map_err(MarkdownDocumentBufferError::from)
                .inspect_err(|&error| self.scope.record_source(error))?;
        }
        Ok(())
    }

    /// 同一封存编辑段的真实错误仍进入原scope首因，不增加第二个Source权限对象。
    pub(in crate::markdown_document_buffer) fn record_source(
        &self,
        error: MarkdownDocumentBufferError,
    ) {
        self.scope.record_source(error);
    }

    pub(in crate::markdown_document_buffer) fn authority(
        &self,
    ) -> MarkdownCellDraftSourceAuthority {
        MarkdownCellDraftSourceAuthority {
            scope: Arc::downgrade(&self.scope),
            content_serial: self.content_serial,
        }
    }

    pub(in crate::markdown_document_buffer) fn original_backing_observer(
        &self,
    ) -> Option<crate::MarkdownOriginalBackingObserver> {
        self.health_backing
            .as_ref()
            .map(|backing| backing.observer())
    }
}
