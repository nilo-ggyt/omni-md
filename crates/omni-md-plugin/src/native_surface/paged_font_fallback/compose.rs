//! 把原NeedStyle的封闭字素票接入同一字体级联；不另建或接收GCB/Script索引。

use objc2_core_foundation::{CFRange, CFString};
use omni_markdown_core::{
    MarkdownLineComposeBudget, MarkdownLineComposeError, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer,
    MarkdownLineFontProbeSegmentStep,
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
        // 原 probe 只转交同一个 Shaping Job；保留原取消与存储错误类型，
        // 不能让新增的坐标认证入口把真实资源退休错误包成另一种普通 Compose 失败。
        MarkdownLineComposeError::Shaping(cause) => NativeMarkdownFontFallbackError::Shaping(cause),
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
        if !base.core_font().permits(authority) {
            return Err(NativeMarkdownFontFallbackError::FontMismatch);
        }
        if !base.accepts_shaping_size(options.points) {
            return Err(NativeMarkdownFontFallbackError::InvalidCluster);
        }
        // 原 probe 已经核准同一个完整 GCB 的 UTF16 与字节端点；不能再经过普通 new 的远端坐标转换。
        // 样式预算由上面的原 shaping_options 校验，原 backing 与取消仍由同一个 owner/guard 负责。
        let owner = Self {
            authority: source.authority(),
            backing: source.original_backing_observer(),
            source: Some(source),
            base: authority.clone(),
            request: NativeMarkdownFontFallbackRequest {
                source_utf16: probe.source_range(),
                options,
                languages: request.languages,
                cascade_budget: request.cascade_budget,
                maximum_font_bytes: request.maximum_font_bytes,
            },
            cascade: None,
            active: None,
            tried: 0,
            probe_source_bytes: 0,
            stopped: false,
            mtm,
            probe: Some(probe),
        };
        owner
            .probe
            .as_ref()
            .expect("原票已移交此 owner")
            .validate()
            .map_err(error)?;
        Ok(owner)
    }

    /// 原 Base 真缺字后才读取首 scalar；它仅选择候选，完整覆盖仍由同一 worker 实排原 GCB。
    pub(super) fn for_string_font(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        self.guard(base, authority, current)?;
        let (read, scalar) = self
            .probe
            .as_ref()
            .expect("首 scalar 候选只来自原协调器真实 probe")
            .first_scalar();
        self.probe_source_bytes = self
            .probe_source_bytes
            .checked_add(read)
            .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
        let scalar = scalar.map_err(error)?; // 即使原票在读取后撤销，已发生的字节量也留待 Producer 领取。
        self.guard(base, authority, current)?;
        let mut bytes = [0_u8; 4];
        let string = CFString::from_str(scalar.encode_utf8(&mut bytes));
        self.guard(base, authority, current)?;
        // SAFETY: string 只含一个完整原 scalar，公开范围至多两个 UTF16；原 base 与本次票贯穿调用。
        let native = unsafe {
            base.native_font()
                .for_string(&string, CFRange::new(0, scalar.len_utf16() as _))
        };
        self.guard(base, authority, current)?;
        drop(string);
        self.guard(base, authority, current)?;
        let valid = || self.guard(base, authority, current).is_ok();
        let captured = NativeMarkdownPagedFont::capture_reusing(
            &native,
            palette,
            self.request.maximum_font_bytes,
            self.mtm,
            &valid,
        );
        self.guard(base, authority, current)?; // 真实原 Source errno 优先于字体捕获或回调失权。
        drop(native);
        self.guard(base, authority, current)?;
        captured.map_err(NativeMarkdownFontFallbackError::ForStringCapture)
    }

    /// 只领取这个原 owner 已发生的有限读取；调用方必须先记账，再消费成功、不可捕获或失败结果。
    pub(in crate::native_surface) fn take_probe_source_bytes(&mut self) -> usize {
        std::mem::take(&mut self.probe_source_bytes)
    }

    /// 同一原票推进一个 Native 候选；palette 只供精确同候选复用，纯 Core 试次仍由原 worker 完成。
    pub fn next_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        palette: &[Rc<NativeMarkdownLineFont>],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        let result = self.issue(base, authority, None, palette, current);
        self.finish_probed(result)
    }

    /// 只消费已真实归还的本段结果；None 表示同候选仍须推进后继段，绝不是 Selected。
    pub fn complete_trial_probed(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        result: NativeMarkdownFontTrialResult,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownFontFallbackCompletion>> {
        let result = self.complete(base, authority, None, result, current);
        self.finish_probed(result)
    }

    /// 缺字候选的真实 Job 与捕获已归还后，才把原协调器恢复到同一个外 GCB 起点。
    /// 下一字体必须重试此前成功的段，不能从失败段续接而漏掉候选的完整覆盖。
    pub(in crate::native_surface) fn restart_probe(
        &mut self,
        composer: &mut MarkdownLineComposer,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<()> {
        let result = (|| {
            self.guard(base, authority, current)?;
            if self.active.is_some() {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            composer
                .restart_font_probe(self.probe.as_mut().expect("原 probed owner 持同一 probe"))
                .map_err(error)?;
            self.guard(base, authority, current)
        })();
        self.finish_probed(result)
    }

    /// 每次只推进原协调器的一个元数据量子；返回真实 Step 后再交成功/失败，后验撤权也不漏记工作。
    /// 中间 Ready 只授权同候选的下一 Job；Complete 才允许消费此前全部段的真实无缺字事实。
    pub(in crate::native_surface) fn step_probe_segment(
        &mut self,
        composer: &mut MarkdownLineComposer,
        style: &MarkdownLineComposeStyle,
        budget: MarkdownLineComposeBudget,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> (
        Option<MarkdownLineFontProbeSegmentStep>,
        Result<Option<NativeMarkdownFontFallbackCompletion>>,
    ) {
        let mut step = None;
        let bytes = self.active.as_ref().and_then(|active| {
            active
                .font
                .as_ref()
                .and_then(NativeMarkdownFontCapture::fresh_bytes_observer)
        });
        let result = (|| {
            self.guard(base, authority, current)?;
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.phase == ProbeTrialPhase::Worker)
            {
                return Err(NativeMarkdownFontFallbackError::InvalidState);
            }
            let probe = self.probe.as_mut().expect("原 probed owner 持同一 probe");
            let advance = self
                .active
                .as_ref()
                .is_some_and(|active| active.phase == ProbeTrialPhase::NextSegment)
                || !probe.shaping_segment_ready().map_err(error)?;
            if !advance {
                return Ok(None); // 首个普通段或已经交付的下一段保原快路径，不重复构造 options 或探下一段。
            }
            let actual = composer
                .step_font_probe_segment(probe, budget)
                .map_err(error)?;
            let answer = actual.answer;
            step = Some(actual); // Native/current 的后验可能失败；已经发生的原工作仍交调用者计费。
            self.guard(base, authority, current)?;
            match answer {
                MarkdownLineFontProbeSegmentAnswer::Pending => Ok(None),
                MarkdownLineFontProbeSegmentAnswer::Ready => {
                    self.request.options = self
                        .probe
                        .as_ref()
                        .expect("同一 probe")
                        .shaping_options(style)
                        .map_err(error)?;
                    if let Some(active) = &mut self.active {
                        active.phase = ProbeTrialPhase::SegmentReady;
                    }
                    Ok(None)
                }
                MarkdownLineFontProbeSegmentAnswer::Complete => {
                    let active = self
                        .active
                        .take()
                        .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
                    if active.phase != ProbeTrialPhase::NextSegment {
                        return Err(NativeMarkdownFontFallbackError::InvalidState);
                    }
                    let facts = active
                        .facts
                        .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
                    self.select_choice(base, active, facts, current).map(Some)
                }
            }
        })();
        // final Choice 会转走捕获；只有失败才核原 capture 的实际释放，不把中间成功误当退休。
        let result = self.finish_probed(result);
        let result = match (result, bytes) {
            (Err(cause), Some(bytes))
                if !bytes.mapping_drop_completed()
                    || !bytes.unmap_succeeded()
                    || bytes.mapped_bytes() != 0
                    || !bytes.source_descriptor_closed()
                    || !bytes.backing_descriptor_closed()
                    || bytes.failed() =>
            {
                Err(NativeMarkdownFontFallbackError::TrialCleanup {
                    cause: Box::new(cause),
                    cleanup: Box::new(NativeMarkdownFontFallbackError::FontBytesNotReleased),
                })
            }
            (result, _) => result,
        };
        (step, result)
    }

    /// 只读取原协调器的当前事实；Pending 不构造 packet，也不切换候选。
    pub(in crate::native_surface) fn probe_segment_ready(&self) -> Result<bool> {
        self.probe
            .as_ref()
            .expect("原 probed owner 持同一 probe")
            .shaping_segment_ready()
            .map_err(error)
    }

    /// 同一候选的前一段已真实归还；这里只给下一段换唯一 packet 身份，不重捕字体或增加候选计数。
    pub(super) fn continue_trial(
        &mut self,
        base: &NativeMarkdownPagedFont,
        authority: &MarkdownLayoutFontAuthority,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontFallbackStep> {
        self.guard(base, authority, current)?;
        if !self.probe_segment_ready()? {
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        let active = self.active.as_mut().expect("续段持唯一原候选");
        if active.phase != ProbeTrialPhase::SegmentReady {
            return Err(NativeMarkdownFontFallbackError::InvalidState);
        }
        let selected = active
            .font
            .as_ref()
            .map(NativeMarkdownFontCapture::font)
            .unwrap_or(base);
        let identity = Arc::new(());
        let trial = NativeMarkdownFontTrial {
            probe: self.probe.clone(),
            source: self.source.as_ref().expect("原候选仍持 Source").clone(),
            source_authority: self.authority.clone(),
            font: selected.core_font().clone(),
            font_authority: active.authority.clone(),
            range: self.request.source_utf16.clone(),
            options: self.request.options.clone(),
            cancellation: active.cancellation.clone(),
            identity: identity.clone(),
        };
        active.identity = identity;
        active.phase = ProbeTrialPhase::Worker;
        self.guard(base, authority, current)?;
        Ok(NativeMarkdownFontFallbackStep::Trial(Box::new(trial)))
    }

    fn finish_probed<T>(&mut self, result: Result<T>) -> Result<T> {
        // 同一失败仍撤原试次/级联/Source，但已有Source与shaper双错链必须跨此wrapper保留。
        result.map_err(|cause| self.stop_preserving_error(cause))
    }
}
