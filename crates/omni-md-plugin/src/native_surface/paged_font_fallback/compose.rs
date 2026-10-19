//! 把原NeedStyle的封闭字素票接入同一字体级联；不另建或接收GCB/Script索引。

use omni_markdown_core::{
    MarkdownLineComposeError, MarkdownLineComposeStyle, MarkdownLineFontProbe,
};

use super::*;

/// Native只给原样式和字体资源预算；方向、脚本、原范围及BOT/EOT全部由Core原票生成。
pub struct NativeMarkdownProbedFontFallbackRequest {
    pub style: MarkdownLineComposeStyle,
    pub languages: &'static [&'static str],
    pub cascade_budget: NativeMarkdownFontCascadeBudget,
    pub maximum_font_bytes: u64,
}

pub(super) fn error(cause: MarkdownLineComposeError) -> NativeMarkdownFontFallbackError {
    match cause {
        MarkdownLineComposeError::Source(cause) => NativeMarkdownFontFallbackError::Source(cause),
        other => NativeMarkdownFontFallbackError::Compose(other),
    }
}

impl NativeMarkdownFontFallback {
    /// 消费当前原请求签出的一个真实GCB；构造不会分配第二套文字依赖或读取整个段落。
    ///
    /// 此字体结果仍只授权原GCB，不能把首字的无缺字结论推广到后继文字或完整run。
    ///
    /// # Errors
    /// 原scope、Source或样式无效时拒绝；所有Native字体来源继续经过原捕获/往返校验。
    pub fn new_with_probe(
        probe: MarkdownLineFontProbe,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        request: NativeMarkdownProbedFontFallbackRequest,
        mtm: MainThreadMarker,
    ) -> Result<Self> {
        let options = probe.shaping_options(&request.style).map_err(error)?;
        let source = probe.source().map_err(error)?.clone();
        let source_authority = source.authority();
        let mut owner = Self::new(
            source,
            &source_authority,
            base,
            authority,
            NativeMarkdownFontFallbackRequest {
                source_utf16: probe.source_range(),
                options,
                languages: request.languages,
                cascade_budget: request.cascade_budget,
                maximum_font_bytes: request.maximum_font_bytes,
            },
            mtm,
        )?;
        probe.validate().map_err(error)?;
        owner.probe = Some(probe);
        Ok(owner)
    }

    /// 同一原票推进一个Native候选；纯Core试次仍需在真实worker内执行并完整归还。
    pub fn next_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let result = self.issue(base, authority, None, current);
        self.finish_probed(result)
    }

    /// 只有原试次的真实完整结果才能消费；此入口没有第二份Script/GCB可供补签。
    pub fn complete_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackCompletion> {
        let result = self.complete(base, authority, None, result, current);
        self.finish_probed(result)
    }

    fn finish_probed<T>(&mut self, result: Result<T>) -> Result<T> {
        // 同一失败仍撤原试次/级联/Source，但已有Source与shaper双错链必须跨此wrapper保留。
        result.map_err(|cause| self.stop_preserving_error(cause))
    }
}
