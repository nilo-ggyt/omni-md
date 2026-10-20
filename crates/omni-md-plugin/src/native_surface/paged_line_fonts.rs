//! 一个真实Native字体的行注册值；会话palette持有它，输出目录只保弱注册。

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

/// 同一真实 Font 的系统行槽与基线；只存两个 Copy 标量，不保 TextKit、正文或 Native manager。
#[derive(Clone, Copy, Debug)]
pub(in crate::native_surface) struct NativeMarkdownPreviewFontMetrics {
    pub line_height: f64,
    pub baseline: f64,
}

/// 强边仅为一个实际Font、其真实度量和同payload注册；没有Source、视图或全局缓存。
pub struct NativeMarkdownLineFont {
    native: NativeMarkdownPagedFont,
    registered: MarkdownLineRegisteredFont,
    metrics: NativeMarkdownPagedFontMetrics,
    traits: CTFontSymbolicTraits,
    preview: Option<NativeMarkdownPreviewFontMetrics>,
}

impl NativeMarkdownLineFont {
    /// 已捕获的Native字体只注册一次；读取度量前后始终核原调用票。
    ///
    /// # Errors
    /// 失权、实际度量失败或有限注册槽耗尽均拒绝，不用同名字体替换旧输出。
    pub fn register(
        native: NativeMarkdownPagedFont,
        catalog: &MarkdownLineFontCatalog,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        let metrics = capture_paged_font_metrics(&native, current)?;
        // SAFETY: 原主线程CTFont由本候选强持有，公开getter仅返回有限trait位，不借正文或调用布局。
        let traits = unsafe { native.native_font().symbolic_traits() };
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        } // 实际face特征在原注册边捕获；绘制和验收不在Reader借用中再次进入Native。
        let registered = catalog.register(native.core_font())?;
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        } // 原票撤销时本候选的Font/注册值直接析构，不占据后继会话的字体槽。
        Ok(Self {
            native,
            registered,
            metrics,
            traits,
            preview: None, // Source 注册只捕原 CT 度量；预览装配另从同一 Font 捕系统行标量。
        })
    }

    /// Preview 的有限字体槽只捕一次实际系统行度量；每次 Native 调用前后仍核原输入/Source 票。
    ///
    /// # Errors
    /// 原票撤销或同一实际 Font 的系统度量无效时归还本候选，不保留 manager 或另找字体。
    pub fn capture_preview_metrics(
        mut self,
        manager: &NSLayoutManager,
        current: &dyn Fn() -> bool,
    ) -> Result<Self> {
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        }
        // SAFETY: 本注册值强持同一 CTFont；Apple 定义 CTFont/NSFont toll-free bridge，+1只活于本次捕获。
        let retained = unsafe { CFRetained::retain(NonNull::from(self.native.native_font())) };
        let retained: Retained<CTFont> = retained.into();
        // SAFETY: 只转换同一真实对象的 Rust 视图，不按名称或字号重新创建另一份 Font。
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
        } // 原隐藏 0.1pt face 的系统 baseline 确实可为0；不以正数限制撑高隐藏围栏。
        self.preview = Some(NativeMarkdownPreviewFontMetrics {
            line_height,
            baseline,
        });
        Ok(self)
    }

    /// 由原 Body 的不可变模式选择：Preview 必须已经捕同 Font 系统标量，Source 不借用此值。
    pub(in crate::native_surface) fn preview_metrics(&self) -> NativeMarkdownPreviewFontMetrics {
        self.preview
            .expect("InlinePreview 的每个真实注册 Font 都持自己的系统行标量")
    }

    #[must_use]
    pub fn native(&self) -> &NativeMarkdownPagedFont {
        &self.native
    }

    #[must_use]
    pub fn registered(&self) -> &MarkdownLineRegisteredFont {
        &self.registered
    }

    /// 返回同一个实际CTFont在注册时的字形特征，不把语义样式或slot编号冒充真实face。
    #[must_use]
    pub fn traits(&self) -> CTFontSymbolicTraits {
        self.traits
    }

    /// 用实际注册弱票核同一个Core payload；slot整数或相同PSName均不够。
    ///
    /// # Errors
    /// 原catalog、槽代次或注册值已失效，或它属于另一个实际Font时拒绝。
    pub fn require(&self, registration: &MarkdownLineFontRegistration) -> Result<()> {
        let leased = registration.lease()?;
        if !self.native.core_font().permits(&leased.font().authority()) {
            return Err(NativeMarkdownLineFontError::FontMismatch);
        } // lease只有本次栈强边；不把目录里的弱注册升级成常驻字体列表。
        Ok(())
    }

    /// 行高合并使用原字号与轴位的真实ascent/descent/leading，不调用Native排版。
    pub fn metrics(&self) -> Result<NativeMarkdownPagedFontMetricValues> {
        Ok(self.metrics.values_for(&self.native)?)
    }
}
