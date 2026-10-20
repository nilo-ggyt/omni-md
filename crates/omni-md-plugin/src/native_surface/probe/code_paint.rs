//! 原 Find 动作的长代码背景验收；取当前已发布分页视图的有限像素，不另画一张候选 Scene。

use super::*;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedSceneKey, NativeMarkdownPagedTextView,
};

/// 本次动作只保原视图弱身份、输入前 CPU 序号与一处固定源标记；没有正文、图像或 Reader 强边。
pub(super) struct ExpectedCodePaint {
    view: Rc<Weak<NativeMarkdownPagedTextView>>,
    facts: MemoryWorkloadFacts,
    epoch: MemoryActionEpoch,
    target: std::ops::Range<usize>,
    after: usize,
    bound: RefCell<Option<(NativeMarkdownPagedSceneKey, usize)>>,
    binding_started: Cell<bool>,
    failure: RefCell<Option<String>>,
}

impl ExpectedCodePaint {
    /// 原三秒期限已经失败后，仅报告此前同步采样留下的有限事实；不重新取图或推进原请求。
    pub(super) fn diagnose_deadline(&self) {
        let failure = self.failure.borrow().clone();
        let bound = self.bound.borrow().is_some();
        eprintln!(
            "native_memory_code_background_deadline binding_started={} original_frame_bound={bound} original_failure={failure:?}",
            self.binding_started.get(),
        );
    }

    /// 首个 Native 动作前固定原视图和实际 CPU 基线；旧帧不能冒充后一次 Find 的背景发布。
    pub(super) fn capture(
        view: &NativeMarkdownPagedTextView,
        facts: &MemoryWorkloadFacts,
        epoch: MemoryActionEpoch,
        target: &std::ops::Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let start = target.end.checked_sub(1).ok_or("代码背景目标范围溢出")?;
        if start < target.start {
            return Err("代码背景目标缺少原固定中文末字符".into());
        }
        let (_, after) = view
            .probe_cpu_published_frame()
            .ok_or("代码背景缺少原动作前 CPU 帧")?;
        let view = native_markdown_table_intent_step(current, || Weak::from(view))
            .map_err(|_| "代码背景原视图弱身份构造时已撤权")?;
        Ok(Self {
            view: Rc::new(view),
            facts: facts.clone(),
            epoch,
            // 固定夹具每次命中是十一字BMP中文，后面恰有一个不命中的等宽空格。
            // 首字右侧仍在同一命中内，不能拿它要求代码底色；末字右侧才是真正未高亮的对照。
            target: start..target.end,
            after,
            bound: RefCell::new(None),
            binding_started: Cell::new(false),
            failure: RefCell::new(None),
        })
    }

    /// 原 Find 终点观察已包括同 Input 的完整 CPU 帧；此处只采这一帧，不等没有生产者的下一帧。
    pub(super) fn after_find_terminal(
        self: Rc<Self>,
        request: Rc<MemoryFindObservation>,
        observer: Box<dyn FnOnce()>,
    ) {
        let result: Result<(), String> = (|| {
            if self.binding_started.replace(true) {
                return Err("原代码背景终点不能重复绑定".into());
            }
            request.completed()?;
            let bound = with_active_native_markdown_surface(|surface| {
                request.completed_at(surface)?;
                let current = || {
                    memory_workload_facts_current(surface, &self.facts)
                        && self.epoch.permits(surface)
                        && surface.editor.as_ref().is_some_and(|paged| {
                            paged.body.presentation_mode() == request.mode
                                && paged.text_view.input_state().ok() == Some(request.input)
                        })
                };
                let result = self.snapshot(surface, &current)?;
                // 完整帧的 Native/Source 核验放在有限采样前后；逐像素 current 只核原纯值权限，避免递归取景。
                request.completed_at(surface)?;
                Ok(result)
            })?;
            *self.bound.borrow_mut() = Some(bound);
            Ok(())
        })();
        if let Err(cause) = result {
            let mut failure = self.failure.borrow_mut();
            if failure.is_none() {
                *failure = Some(cause);
            }
        }
        observer(); // 只回交同一个原终点；Host 的既有调度决定后继检查，不在这里推进事件队列。
    }

    /// 同原 Doc、Body、视图与 Preview 输入下读取实际画面；临时 Native claim 在同步栈归还。
    fn snapshot(
        &self,
        surface: &NativeMarkdownWindowSurface,
        current: &impl Fn() -> bool,
    ) -> Result<(NativeMarkdownPagedSceneKey, usize), String> {
        if !current() {
            return Err("代码背景原 Find 权限已失效".into());
        }
        let editor = surface.editor.as_ref().ok_or("代码背景缺少原编辑器")?;
        let paged = editor;
        let original = native_markdown_table_intent_step(current, || self.view.load())
            .map_err(|_| "代码背景原视图弱引用读取时已撤权")?
            .ok_or("代码背景原视图已经释放")?;
        let same = std::ptr::eq(&*original, &**paged.text_view)
            && std::rc::Weak::ptr_eq(&self.facts.body, &Rc::downgrade(&paged.body));
        drop(original);
        if !same || !current() {
            return Err("代码背景当前正文不属于原动作".into());
        }
        let sample = find_highlights::PixelSample::capture(
            &paged.text_view,
            &paged.body,
            self.target.clone(),
            current,
        )?;
        let (key, paints) = paged
            .text_view
            .probe_cpu_published_frame()
            .ok_or("代码背景缺少已发布帧")?;
        if !key.same(&sample.key)
            || key.presentation_mode != MarkdownPresentationMode::InlinePreview
            || key.request.input.generation != self.facts.generation
            || paints <= self.after
        {
            return Err("代码背景未来自本次原 Preview Find 后的真实完整 CPU 帧".into());
        }
        // 调色板是实际产品外观契约；只有目标外侧真有底色且目标内有不同的字形墨迹才通过。
        // 这不是检查 Rust 的 styles/cache：丢画背景、全透明 bitmap 或只显示单色都不能通过。
        let expected = crate::native_surface::text_attributes::NATIVE_MARKDOWN_CODE_BACKGROUND;
        let mut background = 0;
        let mut ink_low = [f64::INFINITY; 3];
        let mut ink_high = [f64::NEG_INFINITY; 3];
        let mut ink_samples = 0;
        for (color, inside) in sample.colors.iter().zip(sample.in_match) {
            let near_background = color
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (*actual - expected).abs() <= 0.025);
            if !inside && near_background {
                background += 1;
            }
            if inside {
                for channel in 0..3 {
                    ink_low[channel] = ink_low[channel].min(color[channel]);
                    ink_high[channel] = ink_high[channel].max(color[channel]);
                }
                ink_samples += 1;
            }
        }
        let ink = ink_samples >= 4
            && ink_low
                .into_iter()
                .zip(ink_high)
                .any(|(low, high)| high - low > 0.12);
        if background < 4 || !ink {
            // 失败时只附原128点中的两端颜色与最后对照点的真实格式/颜色空间/原分量；
            // 不再截屏、重画或改tag，也不以这些诊断替换原背景与墨迹判定。
            let controls = || {
                sample
                    .colors
                    .iter()
                    .zip(sample.in_match)
                    .filter(|(_, inside)| !inside)
            };
            let first_control = controls().next().map(|(color, _)| color);
            let last_control = controls().next_back().map(|(color, _)| color);
            return Err(format!(
                "原代码目标没有真实底色或字形墨迹：background_samples={background} ink_samples={ink_samples} glyph_contrast={ink} first_control={first_control:?} last_control={last_control:?} expected={expected:?} color_diagnostic=({})",
                sample.color_diagnostic
            ));
        }
        if !current() {
            return Err("代码背景实际像素归还后原动作已撤权".into());
        }
        Ok((key, paints))
    }

    /// 绑定后仍须是同一原发布帧；同文本、后继查询或新视口都不能替换本次实际画面。
    pub(super) fn verify(
        &self,
        surface: &NativeMarkdownWindowSurface,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if let Some(cause) = self.failure.borrow().clone() {
            return Err(cause);
        }
        let bound = self
            .bound
            .borrow()
            .clone()
            .ok_or("代码背景缺少原 Find 终点绑定")?;
        let (key, paints) = self.snapshot(surface, current)?;
        if !key.same(&bound.0) || paints != bound.1 {
            return Err("代码背景当前 CPU 帧已不属于原终点".into());
        }
        println!(
            "native_memory_code_background verified=true actual_paged_cpu_frame=true actual_view_pixels=true code_background=true glyph_ink=true finite_pixel_samples=128 same_native_owner=true same_source=true screen_present_verified=false"
        );
        Ok(())
    }
}
