//! 同一准备回合只消费原请求；Native 重入后检查原票，不捕后继请求替旧 Source 签 Scene。

use std::rc::Weak;

use omni_markdown_core::{MarkdownLayoutTextAuthority, MarkdownLayoutTextSource};

use super::{
    NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error,
    NativeMarkdownPagedSceneRequest, Result, ViewError,
};

pub(super) struct PreparationTicket {
    pub(super) request: NativeMarkdownPagedSceneRequest,
    source: MarkdownLayoutTextAuthority,
    serial: u64,
    width: i64,
    padding: f64,
}

impl PreparationTicket {
    pub(super) fn new(
        request: &NativeMarkdownPagedSceneRequest,
        source: &MarkdownLayoutTextSource,
        serial: u64,
        width: i64,
        padding: f64,
    ) -> Self {
        Self {
            request: request.clone(),
            source: source.authority(),
            serial,
            width,
            padding,
        }
    }

    fn pure_current(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && source.authority() == self.source
            && source.generation() == self.request.input.generation
            && source.utf16_len() == self.request.input.utf16_len
            && owner
                .input
                .target_domain()
                .is_ok_and(|target| target.same(&self.request.target))
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && super::producer::width(&self.request, self.padding)
                .is_ok_and(|width| width == self.width)
    }

    pub(super) fn current(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> bool {
        if !self.pure_current(owner, source) {
            return false; // 纯前台事实已变时不再进入 Native，也不读取较新的输入修复旧票。
        }
        let Ok(view) = owner.view() else {
            return false;
        };
        let same = view
            .capture_scene_request()
            .is_ok_and(|actual| actual.same(&self.request));
        drop(view); // 临时 Native claim 归还后再核纯票，析构回调也没有重签旧请求的权利。
        same && self.pure_current(owner, source)
    }

    pub(super) fn require(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> Result<()> {
        // 端点0不需要全文读取；原冻结映射的真实首错始终先于普通前台撤权。
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        let current = self.current(owner, source);
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        if !current {
            return Err(Error::PreparationRevoked);
        }
        Ok(())
    }

    pub(super) fn require_pure(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
    ) -> Result<()> {
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        if !self.pure_current(owner, source) {
            return Err(Error::PreparationRevoked);
        }
        Ok(()) // 纯Core量子没有Native重入点，不为每256个标量再额外执行整组Native getter。
    }
}

impl Error {
    pub(super) fn is_foreground_revocation(&self) -> bool {
        use crate::native_surface::{
            paged_font_cascade::NativeMarkdownFontCascadeError as Cascade,
            paged_font_fallback::NativeMarkdownFontFallbackError as Fallback,
            paged_font_geometry::NativeMarkdownPagedFontGeometryError as Metrics,
            paged_fonts::NativeMarkdownPagedFontError as Capture,
            paged_line_fonts::NativeMarkdownLineFontError as Font,
            paged_viewport::NativeMarkdownPagedViewportError as Viewport,
        };
        let cascade_revoked =
            |cause: &Cascade| matches!(cause, Cascade::Revoked | Cascade::Font(Capture::Revoked));
        matches!(
            self,
            Self::PreparationRevoked
                | Self::View(ViewError::Revoked)
                | Self::Font(Capture::Revoked)
                | Self::Fallback(Fallback::Revoked)
                | Self::Registration(Font::Revoked | Font::Metrics(Metrics::Revoked))
                | Self::Viewport(Viewport::Revoked | Viewport::Busy)
        ) || matches!(self, Self::Fallback(Fallback::Cascade(cause) | Fallback::CandidateCapture { error: cause, .. }) if cascade_revoked(cause))
        // EIO、真实字体失败和普通 InvalidRange 等均不在可交后继的白名单中。
    }
}
