//! The sealed draft guard shared by all frozen reads. Only original capture can
//! issue it, so internal snapshot clones such as Bidi cannot bypass revocation.

use super::*;

#[derive(Clone)]
/// Weak identity and fixed content version for the cell-local text domain, not a
/// canonical document range or a separate edit permission.
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

/// Issue only through this same draft's controlled capture boundary. Coordinates,
/// document generation or another frozen-read receipt cannot derive this identity.
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
    /// Observe only the captured numeric version. Continued readability still
    /// requires the original Source's actual health and revocation checks.
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
        // Internal snapshots return Core errors. Check only fixed scope atomics,
        // without capturing the document, Source or candidate root.
        _ => MarkdownDocumentBufferError::PreparedTextInputStale,
    })
}

/// Retain fixed scope atomics and the same original anonymous backing, without
/// extending the lifetime of the document, history, or native objects.
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
        // Frozen already checked actual backing/edit segments in order. Check
        // this same scope here without repeating the storage-health traversal.
        scope_health(&self.scope, self.content_serial)
    }

    /// Frozen checks actual text resources before revocation. Preserve the original
    /// backing's first failure even when no current root leaf refers to it.
    pub(in crate::markdown_document_buffer) fn backing_health(
        &self,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(backing) = &self.health_backing {
            // Even a delta-only root preserves original Source failure ahead of
            // content revocation or a previously matching derived-cache entry.
            backing
                .health()
                .map_err(MarkdownDocumentBufferError::from)
                .inspect_err(|&error| self.scope.record_source(error))?;
        }
        Ok(())
    }

    /// Actual sealed-edit-segment failures enter this original scope's first cause;
    /// no second Source-authority object is introduced for error reporting.
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
