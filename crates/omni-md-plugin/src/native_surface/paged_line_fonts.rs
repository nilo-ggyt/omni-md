//! 一个真实Native字体的行注册值；会话palette持有它，输出目录只保弱注册。

use omni_markdown_core::{
    MarkdownLineComposeError, MarkdownLineFontCatalog, MarkdownLineFontRegistration,
    MarkdownLineRegisteredFont,
};

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
    #[error("无法注册原Core字体：{0}")]
    Registration(#[from] MarkdownLineComposeError),
    #[error("无法读取原Native字体度量：{0}")]
    Metrics(#[from] NativeMarkdownPagedFontGeometryError),
}
type Result<T> = std::result::Result<T, NativeMarkdownLineFontError>;

/// 强边仅为一个实际Font、其真实度量和同payload注册；没有Source、视图或全局缓存。
pub struct NativeMarkdownLineFont {
    native: NativeMarkdownPagedFont,
    registered: MarkdownLineRegisteredFont,
    metrics: NativeMarkdownPagedFontMetrics,
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
        let registered = catalog.register(native.core_font())?;
        if !current() {
            return Err(NativeMarkdownLineFontError::Revoked);
        } // 原票撤销时本候选的Font/注册值直接析构，不占据后继会话的字体槽。
        Ok(Self {
            native,
            registered,
            metrics,
        })
    }

    #[must_use]
    pub fn native(&self) -> &NativeMarkdownPagedFont {
        &self.native
    }

    #[must_use]
    pub fn registered(&self) -> &MarkdownLineRegisteredFont {
        &self.registered
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
