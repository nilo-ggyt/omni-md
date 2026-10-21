//! One actual Native font registration is held by the session palette; the output directory retains
//! weak registration only.

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSLayoutManager};
use objc2_core_foundation::CFRetained;
use objc2_core_text::{CTFont, CTFontSymbolicTraits};
use omni_markdown_core::{
    MarkdownLineComposeError, MarkdownLineFontCatalog, MarkdownLineFontRegistration,
    MarkdownLineRegisteredFont,
};
use std::ptr::NonNull;

use super::{
    paged_font_geometry::{
        NativeMarkdownPagedFontGeometryError, NativeMarkdownPagedFontMetricValues,
        NativeMarkdownPagedFontMetrics, capture_paged_font_metrics,
    },
    paged_fonts::NativeMarkdownPagedFont,
};

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownLineFontError {
    #[error("原行字体注册请求已经失权")]
    Revoked,
    #[error("行输出的原字体与这个Native字体不一致")]
    FontMismatch,
    #[error("原生字体没有提供所需的粗体、斜体或等宽特征")]
    StyleUnavailable,
    #[error("无法保留原系统字体的实际文稿实例：{0}")]
    DocumentInstance(#[from] super::paged_fonts::NativeMarkdownPagedFontError),
    #[error("无法注册原Core字体：{0}")]
    Registration(#[from] MarkdownLineComposeError),
    #[error("无法读取原Native字体度量：{0}")]
    Metrics(#[from] NativeMarkdownPagedFontGeometryError),
}
type Result<T> = std::result::Result<T, NativeMarkdownLineFontError>;

/// System line slot/baseline for the same actual font, stored as two Copy scalars without TextKit,
/// text, or a Native manager.
#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) struct NativeMarkdownPreviewFontMetrics {
    pub line_height: f64,
    pub baseline: f64,
}

/// Strong ownership contains one actual font, its metrics, and same-payload registration, without
/// Source, View, or global cache.
pub struct NativeMarkdownLineFont {
    native: NativeMarkdownPagedFont,
    registered: MarkdownLineRegisteredFont,
    metrics: NativeMarkdownPagedFontMetrics,
    traits: CTFontSymbolicTraits,
    preview: Option<NativeMarkdownPreviewFontMetrics>,
}

impl NativeMarkdownLineFont {
    /// Register the captured Native font once, validating the original caller around metric reads.
    ///
    /// # Errors
    /// Reject revocation, actual metric failure, or exhausted registration slots without substituting a
    /// same-name font for old output.
    pub fn register(
        native: NativeMarkdownPagedFont,
        catalog: &MarkdownLineFontCatalog,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        let metrics = capture_paged_font_metrics(&native, current)?;
        // SAFETY: This candidate strongly holds the main-thread CTFont; its getter returns finite trait
        // bits without text borrowing or layout.
        let traits = unsafe { native.native_font().symbolic_traits() };
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        } // Capture actual face traits at registration; drawing/verification need not reenter Native while a reader is borrowed.
        let registered = catalog.register(native.core_font())?;
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        } // Revocation drops this candidate's font/registration without occupying a successor session's font slot.
        Ok(Self {
            native,
            registered,
            metrics,
            traits,
            preview: None, // Source captures original Core Text metrics only; preview separately captures system-line scalars for that same font.
        })
    }

    /// The finite preview font slot captures actual system-line metrics once, validating original
    /// Input/Source authority around every Native call.
    ///
    /// # Errors
    /// Revocation or invalid system metrics returns this candidate without retaining the manager or
    /// finding another font.
    pub fn capture_preview_metrics(
        mut self,
        manager: &NSLayoutManager,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        // SAFETY: Registration holds this CTFont; the public CTFont/NSFont toll-free bridge lends a +1
        // claim only for this capture.
        let retained = unsafe { CFRetained::retain(NonNull::from(self.native.native_font())) };
        let retained: Retained<CTFont> = retained.into();
        // SAFETY: Cast only the Rust view of the same actual object, without reconstructing a font by
        // name or size.
        let font: Retained<NSFont> = unsafe { Retained::cast_unchecked(retained) };
        let line_height = manager.defaultLineHeightForFont(&font);
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        let baseline = manager.defaultBaselineOffsetForFont(&font);
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        if !line_height.is_finite() || line_height <= 0.0 || !baseline.is_finite() {
            return Err(NativeMarkdownPagedFontGeometryError::InvalidMetrics.into());
        } // A hidden 0.1-point face may have an actual zero baseline; requiring positivity would incorrectly expand hidden fences.
        self.preview = Some(NativeMarkdownPreviewFontMetrics {
            line_height,
            baseline,
        });
        Ok(self)
    }

    /// The Body's actual mode selects usage: preview must already hold same-font system scalars, while
    /// Source does not borrow them.
    pub(in crate::native_surface) fn preview_metrics(&self) -> NativeMarkdownPreviewFontMetrics {
        self.preview
            .expect("InlinePreview 的每个真实注册 Font 都持自己的系统行标量")
    }

    #[must_use]
    /// Borrow the already-owned Native font without another capture.
    pub fn native(&self) -> &NativeMarkdownPagedFont {
        &self.native
    }

    #[must_use]
    /// Borrow this same registration value; callers receive no additional catalog slot.
    pub fn registered(&self) -> &MarkdownLineRegisteredFont {
        &self.registered
    }

    /// Return traits captured from the actual CTFont at registration, never semantic style or a slot
    /// number pretending to be a face.
    #[must_use]
    pub fn traits(&self) -> CTFontSymbolicTraits {
        self.traits
    }

    /// Validate the same Core payload with its actual weak registration; a slot integer or equal
    /// PostScript name is insufficient.
    ///
    /// # Errors
    /// Reject an expired catalog, slot generation, or registration, and any registration belonging to
    /// another actual font.
    pub fn require(&self, registration: &MarkdownLineFontRegistration) -> Result<()> {
        let leased = registration.lease()?;
        if !self.native.core_font().permits(&leased.font().authority()) {
            return Err(NativeMarkdownLineFontError::FontMismatch);
        } // The lease holds a strong claim only on this stack; weak directory registrations never become a resident font inventory.
        Ok(())
    }

    /// Combine actual ascent/descent/leading at the original size/axes without invoking Native layout.
    pub fn metrics(&self) -> Result<NativeMarkdownPagedFontMetricValues> {
        Ok(self.metrics.values_for(&self.native)?)
    }
}
