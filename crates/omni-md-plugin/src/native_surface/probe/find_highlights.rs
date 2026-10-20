//! 原长文窗口的有限像素回归；只保存弱权限和颜色样本，不保存图像、正文副本或另一份布局。

use super::*;
use crate::native_surface::paged_body::NativeMarkdownPagedBody;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedSceneKey, NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate,
};
use objc2::rc::Weak as NativeWeak;
use objc2_app_kit::{NSBitmapFormat, NSClipView, NSColor, NSColorSpace, NSColorSpaceModel};
#[cfg(feature = "native-block-fragment-diagnostic")]
use objc2_core_graphics::{CGColorSpace, CGImage};
use objc2_foundation::NSAlignmentOptions;
use std::ops::Range;
use std::os::unix::fs::MetadataExt;

const HEAD_UNITS: usize = 1_024;
const SAMPLE_COLUMNS: usize = 16;
const SAMPLE_ROWS: usize = 8;
const PIXEL_SAMPLES: usize = SAMPLE_COLUMNS * SAMPLE_ROWS;

/// 原窗口跨事件只拥有固定颜色和小范围；batch也只保Weak，不能使旧装饰继续存活。
pub(super) struct FindHighlightsProbe {
    facts: MemoryWorkloadFacts,
    selection: MarkdownTextSelection,
    input: crate::native_surface::paged_input::NativeMarkdownPagedInputState,
    epoch: MemoryActionEpoch,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    query_units: usize,
    chinese: Vec<Range<usize>>,
    non_bmp: char,
    non_bmp_range: Range<usize>,
    chinese_baseline: Option<PixelSample>,
    non_bmp_baseline: Option<PixelSample>,
    scrolled_baseline: Option<PixelSample>,
    preview_baseline: Option<PixelSample>,
    // Hide只允许归还原Find栏实测占高；不把任意滚动或resize当作同一glyph的合法平移。
    hidden_bar_height: Option<f64>,
    // 有限Paged正文随滚动移动frame，本地visibleRect可不变；原滚动票必须记录真正clip的bounds。
    scrolled_from: Option<(NativeWeak<NSClipView>, NSRect, NativeMarkdownPagedSceneKey)>,
}

/// 一个真实glyph及右侧未命中对照共用同一小bitmap；只复制128个RGB标量。
pub(super) struct PixelSample {
    pub(super) key: NativeMarkdownPagedSceneKey,
    source: Range<usize>,
    frame: NSRect,
    rect: NSRect,
    width: isize,
    height: isize,
    pub(super) colors: [[f64; 3]; PIXEL_SAMPLES],
    pub(super) in_match: [bool; PIXEL_SAMPLES],
    // 仅原代码背景验收保存颜色解释的纯值；bitmap、CGImage和NSColor都不能跨事件保活。
    #[cfg(feature = "native-block-fragment-diagnostic")]
    pub(super) color_diagnostic: String,
}

impl NativeMarkdownMemoryWorkload {
    /// 每次取走原观察状态再进入Native；无RefMut跨字段输入、绘制、滚动或模式回调。
    pub(crate) fn exercise_find_highlights(&self, stage: i16) -> Result<(), String> {
        if stage == 425 {
            if self.find_highlights.borrow().is_some() {
                return Err("高亮像素回归不能重复初始化".into());
            }
            let facts = self.facts()?;
            let probe = with_active_native_markdown_surface(|surface| {
                let mut probe = FindHighlightsProbe::capture(surface, facts)?;
                let editor = surface.editor.as_ref().ok_or("高亮源码模式缺少原正文")?;
                if editor.presentation_mode() != MarkdownPresentationMode::Source {
                    // 正式 Paged 模式转换会撤销原 Find 意图；动作前声明唯一后继，不能事后重采 epoch。
                    probe.epoch = probe.next_find_epoch()?;
                }
                Ok(probe)
            })?;
            *self.find_highlights.borrow_mut() = Some(probe);
            return self.change_mode(true); // 沿原产品模式收据推进，不手动补跑executor。
        }
        if stage == 426 || stage == 436 {
            self.verify_mode(stage == 426)?; // 原模式真实完成后，才读取相应正文的像素。
        } else {
            self.verify_memory_frame()?; // 原动作的真实完整 CPU 终点由统一 driver 交付，采样不补跑排版。
        }
        let mut probe = self
            .find_highlights
            .borrow_mut()
            .take()
            .ok_or("高亮像素回归缺少原观察状态")?;
        let result = (|| {
            if stage == 431 {
                // 重复打开沿真实产品入口执行；原查询正在组合时不能再次全选或提交候选。
                probe.verify_reopen_marked_query()?;
            }
            with_active_native_markdown_surface(|surface| {
                probe.exercise(surface, stage)?;
                probe.verify_document(surface, &self.path)
            })?;
            if stage == 435 {
                with_active_native_markdown_surface(|surface| {
                    if !probe.current(surface) {
                        return Err("高亮 Preview 转换前原输入已撤权".into());
                    }
                    let editor = surface
                        .editor
                        .as_ref()
                        .ok_or("高亮 Preview 转换缺少原正文")?;
                    if editor.presentation_mode() != MarkdownPresentationMode::InlinePreview {
                        probe.epoch = probe.next_find_epoch()?;
                    }
                    Ok(())
                })?;
                self.change_mode(false)?; // 仅真实Preview转换；下一事件仍核原mode收据。
            } else if stage == 437 {
                // 首Native前声明Hide将推进的原Find意图；返回后不重采新epoch替换预期。
                let expected = probe.next_find_epoch()?;
                perform_native_markdown_find_action(MarkdownFindAction::Hide)?;
                with_active_native_markdown_surface(|surface| {
                    if !probe.current_at(surface, expected) {
                        return Err("隐藏查找栏改变了原正文或输入权限".into());
                    }
                    probe.epoch = expected;
                    let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
                    if bar.is_visible() {
                        return Err("真实Hide未隐藏原查找栏".into());
                    }
                    probe.verify_document(surface, &self.path)
                })?;
            } else if stage == 438 {
                with_active_native_markdown_surface(|surface| {
                    let baseline = probe.preview_baseline.as_ref().ok_or("缺少Preview基线")?;
                    let height = probe.hidden_bar_height.ok_or("Hide缺少原查找栏实测高度")?;
                    baseline.require_hidden_restored(
                        &probe.sample(surface, baseline.source.clone())?,
                        height,
                    )?;
                    probe.verify_document(surface, &self.path)
                })?;
                println!(
                    "native_find_highlight verified=true source_pixels=true preview_pixels=true unicode_non_bmp=true marked_query_protocol_no_provisional=true real_system_ime_verified=false old_geometry_revoked=true selection_unchanged=true empty_history_unchanged=true nonempty_undo_redo_verified=false screen_present_verified=false"
                );
            }
            Ok(())
        })();
        if stage != 438 || result.is_err() {
            *self.find_highlights.borrow_mut() = Some(probe); // 错误也交还原弱状态，由原Return一并清理。
        }
        result
    }
}

impl FindHighlightsProbe {
    /// 首Native前固定完整选择及原栏；文首只读1024UTF16，末端不能截断surrogate。
    fn capture(
        surface: &NativeMarkdownWindowSurface,
        facts: MemoryWorkloadFacts,
    ) -> Result<Self, String> {
        if !memory_workload_facts_current(surface, &facts) {
            return Err("高亮验收原窗口已失效".into());
        }
        let editor = surface.editor.as_ref().ok_or("高亮验收缺少原正文")?;
        let paged = editor;
        let input = paged
            .text_view
            .input_state()
            .map_err(|cause| cause.to_string())?;
        let bar = editor
            .find_bar()
            .borrow()
            .clone()
            .ok_or("高亮验收缺少原栏")?;
        let (selection, chinese, non_bmp, non_bmp_range) = {
            let document = editor.document().borrow();
            if document.can_undo() || document.can_redo() {
                return Err("只读高亮夹具开始时已存在历史，不能冒认空历史验收".into());
            }
            let mut units = [0_u16; HEAD_UNITS];
            let mut length = HEAD_UNITS.min(document.utf16_len());
            document
                .copy_utf16_code_units(0, &mut units[..length])
                .map_err(|_| "无法读取有限文首")?;
            if length != 0 && (0xd800..=0xdbff).contains(&units[length - 1]) {
                length -= 1; // 只舍弃被有限上界截开的高surrogate，不把它替换成另一字符。
            }
            let head = String::from_utf16(&units[..length]).map_err(|_| "有限文首不是合法UTF16")?;
            let chinese: Vec<_> = head
                .match_indices("中文")
                .map(|(byte, _)| {
                    let start = head[..byte].encode_utf16().count();
                    start..start + 2
                })
                .collect();
            let (byte, non_bmp) = head
                .char_indices()
                .find(|(_, scalar)| scalar.len_utf16() == 2)
                .ok_or("原有限文首缺少nonBMP字符，不能回退别的夹具")?;
            let start = head[..byte].encode_utf16().count();
            if chinese.is_empty() {
                return Err("原有限文首缺少中文目标，不能修改样本使回归通过".into());
            }
            (
                document.selection(),
                chinese,
                non_bmp,
                start..start + non_bmp.len_utf16(),
            )
        };
        let mut probe = Self {
            facts,
            selection,
            input,
            epoch: MemoryActionEpoch::capture(surface),
            bar: Rc::downgrade(&bar),
            query_units: 0,
            chinese,
            non_bmp,
            non_bmp_range,
            chinese_baseline: None,
            non_bmp_baseline: None,
            scrolled_baseline: None,
            preview_baseline: None,
            hidden_bar_height: None,
            scrolled_from: None,
        };
        // 查询长度只在尚未开始组合输入的原字段读取；后继长度均由本用例明确输入决定。
        let current = || probe.current(surface);
        let fields = step(&current, || bar.fields())?;
        let value = step(&current, || fields[0].stringValue())?;
        let query_units = step(&current, || value.length())?;
        drop((value, fields));
        if !current() || query_units > 256 {
            return Err("高亮验收的原查询变化或超出有限预算".into());
        }
        probe.query_units = query_units;
        println!(
            "native_find_highlight_fixture non_bmp=U+{:X} source_start={} source_units=2 head_units={HEAD_UNITS}",
            u32::from(probe.non_bmp),
            probe.non_bmp_range.start
        );
        Ok(probe)
    }

    /// 仅原owner/Doc/选择/空历史和预声明代次有权限；无Native getter或完整正文hash。
    fn current_at(&self, surface: &NativeMarkdownWindowSurface, epoch: MemoryActionEpoch) -> bool {
        memory_workload_facts_current(surface, &self.facts)
            && epoch.permits(surface)
            && surface.editor.as_ref().is_some_and(|editor| {
                let same_bar = editor
                    .find_bar()
                    .borrow()
                    .as_ref()
                    .is_some_and(|bar| std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(bar)));
                let document = editor.document().borrow();
                same_bar
                    && editor.text_view.input_state().ok() == Some(self.input)
                    && document.selection() == self.selection
                    && !document.can_undo()
                    && !document.can_redo()
            })
    }

    /// 本阶段固定原动作epoch，不能靠相同内容或后继合法窗口继续采样。
    fn current(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        self.current_at(surface, self.epoch)
    }

    /// 一次明确输入或Hide只授权原Find意图的下一值，其他业务代次保持原值。
    fn next_find_epoch(&self) -> Result<MemoryActionEpoch, String> {
        let mut expected = self.epoch;
        expected.find = expected.find.checked_add(1).ok_or("高亮输入代次溢出")?;
        Ok(expected)
    }

    /// 每个事件只做声明动作；后继事件消费正常产品队列的结果，不补executor或等待重试。
    fn exercise(
        &mut self,
        surface: &NativeMarkdownWindowSurface,
        stage: i16,
    ) -> Result<(), String> {
        if !self.current(surface) {
            return Err("高亮验收的原正文、选择或意图已失效".into());
        }
        let editor = surface.editor.as_ref().ok_or("高亮正文缺失")?;
        let paged = editor;
        let view = &**paged.text_view;
        match stage {
            426 => {
                let current = || self.current(surface);
                let scroll = editor.scroll_view();
                let clip = step(&current, || scroll.contentView())?;
                step(&current, || clip.scrollToPoint(NSPoint::new(0.0, 0.0)))?;
                step(&current, || scroll.reflectScrolledClipView(&clip))?;
                drop(clip);
                self.input(surface, "", false)?; // 空查询让下个事件建立真实无高亮基线。
            }
            427 => {
                let source = self.visible_chinese(surface)?;
                self.chinese_baseline = Some(self.sample(surface, source)?);
                self.input(surface, "中文", false)?;
            }
            428 => {
                let baseline = self.chinese_baseline.as_ref().ok_or("缺少中文基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
                self.input(surface, "", false)?; // 后继事件独立证明旧亮区真正擦除。
            }
            429 => {
                let baseline = self.chinese_baseline.as_ref().ok_or("缺少中文基线")?;
                baseline.require_restored(&self.sample(surface, baseline.source.clone())?)?;
                self.non_bmp_baseline = Some(self.sample(surface, self.non_bmp_range.clone())?);
                let query = self.non_bmp.to_string();
                self.input(surface, &query, false)?;
            }
            430 => {
                let baseline = self.non_bmp_baseline.as_ref().ok_or("缺少nonBMP基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
                self.input(surface, "中", true)?; // 真实marked候选撤旧意图，不把候选提交为查询。
            }
            431 => {
                let current = || self.current(surface);
                let field = self.field_editor(surface, &current)?;
                if !step(&current, || field.hasMarkedText())? {
                    return Err("查询字段没有保留实际marked候选".into());
                }
                drop(field); // 不读取candidate string，实际像素和hasMarkedText分别验证。
                for baseline in [&self.chinese_baseline, &self.non_bmp_baseline] {
                    let baseline = baseline.as_ref().ok_or("缺少组合输入前的原基线")?;
                    baseline.require_restored(&self.sample(surface, baseline.source.clone())?)?;
                }
                self.input(surface, "中文", false)?; // 公开insertText提交同一字段组合输入。
            }
            432 => {
                let baseline = self.chinese_baseline.as_ref().ok_or("缺少中文基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
                let current = || self.current(surface);
                let visible = step(&current, || view.visibleRect())?;
                let old = view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?;
                let scroll = editor.scroll_view();
                let clip = step(&current, || scroll.contentView())?;
                let before = step(&current, || clip.bounds())?;
                let original = step(&current, || NativeWeak::from_retained(&clip))?;
                step(&current, || {
                    clip.scrollToPoint(NSPoint::new(
                        visible.origin.x,
                        visible.origin.y + visible.size.height * 0.5,
                    ))
                })?;
                step(&current, || scroll.reflectScrolledClipView(&clip))?;
                drop(clip);
                self.scrolled_from = Some((original, before, old));
                // 新clip先走真实空查询帧；不能拿尚未刷新的旧高亮时序伪造新位置的无亮基线。
                self.input(surface, "", false)?;
            }
            433 => {
                let current = || self.current(surface);
                let (original, old_bounds, old) =
                    self.scrolled_from.as_ref().ok_or("缺少原滚动票")?;
                let original = step(&current, || original.load())?.ok_or("原滚动clip已释放")?;
                let scroll = editor.scroll_view();
                let clip = step(&current, || scroll.contentView())?;
                if !std::ptr::eq(&*clip, &*original) {
                    return Err("滚动后的clip不再属于原实际挂载对象".into());
                }
                let bounds = step(&current, || clip.bounds())?;
                drop((clip, original)); // 原弱身份只在本事件同步升级，不保活后继图或补采替代clip。
                let key = view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?;
                // 正式正文只占当前视口；update把frame跟随document原点、bounds保持零，
                // 因而不能用它的本地visibleRect冒充滚动量。真实clip必须已动，旧Scene仍必须撤权。
                if bounds == *old_bounds
                    || key.same(old)
                    || NativeMarkdownPagedTextViewDelegate::scene_current(&*paged.body, old)
                {
                    return Err("真实滚动没有撤销旧高亮几何或发布新clip".into());
                }
                let source = self.visible_chinese(surface)?;
                self.scrolled_baseline = Some(self.sample(surface, source)?);
                self.input(surface, "中文", false)?;
            }
            434 => {
                let baseline = self
                    .scrolled_baseline
                    .as_ref()
                    .ok_or("缺少新clip的无亮基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
                self.input(surface, "", false)?;
            }
            435 => {
                let baseline = self
                    .scrolled_baseline
                    .as_ref()
                    .ok_or("缺少新clip的原基线")?;
                baseline.require_restored(&self.sample(surface, baseline.source.clone())?)?;
            }
            436 => {
                let source = self.visible_chinese(surface)?;
                self.preview_baseline = Some(self.sample(surface, source)?);
                self.input(surface, "中文", false)?;
            }
            437 => {
                let baseline = self.preview_baseline.as_ref().ok_or("缺少Preview基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
                let current = || self.current(surface);
                let bar = self.bar.upgrade().ok_or("Hide前原查找栏已释放")?;
                let frame = step(&current, || bar.visible_frame_for_probe())?
                    .ok_or("Hide前原查找栏没有实际可见frame")?;
                if !frame.size.height.is_finite() || frame.size.height <= 0.0 {
                    return Err("Hide前原查找栏没有有限正高布局".into());
                }
                self.hidden_bar_height = Some(frame.size.height);
            }
            438 => {} // 原Hide后的完整CPU帧由外层原等待交付，再核同一有限像素已恢复。
            _ => return Err("未声明的高亮像素阶段".into()), // 原driver才能决定后继，不在这里发明另一状态机。
        }
        Ok(())
    }

    /// 用真实重复打开检查焦点、组合范围与本地选择；不读取候选正文，也不伪造字段通知。
    fn verify_reopen_marked_query(&self) -> Result<(), String> {
        let (original, selection, marked, revision) =
            with_active_native_markdown_surface(|surface| {
                let current = || self.current(surface);
                let field = self.field_editor(surface, &current)?;
                if !step(&current, || field.hasMarkedText())? {
                    return Err("重复打开前原查询没有实际候选".into());
                }
                let selection = step(&current, || field.selectedRange())?;
                let marked = step(&current, || field.markedRange())?;
                let revision = field
                    .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                    .ok_or("原查询不是会话编辑器")?
                    .input_client_revision();
                Ok((field, selection, marked, revision))
            })?;
        // 重复原Find模式只核焦点与组合是否保持；打开Replace会改变栏高和正文几何，
        // 不能把这种额外布局动作混入后继同一像素基线的恢复检查。
        show_native_markdown_find_bar(false)?;
        let result = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface);
            let field = self.field_editor(surface, &current)?;
            let responder = step(&current, || surface.panel.firstResponder())?;
            let same_responder = responder.as_ref().is_some_and(|responder| {
                std::ptr::from_ref(&**responder).addr() == std::ptr::from_ref(&*original).addr()
            });
            drop(responder);
            if !std::ptr::eq(&*field, &*original)
                || !same_responder
                || field
                    .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                    .is_none_or(|editor| editor.input_client_revision() != revision)
                || !step(&current, || field.hasMarkedText())?
                || step(&current, || field.selectedRange())? != selection
                || step(&current, || field.markedRange())? != marked
            {
                return Err("重复打开查找改变了原焦点、候选或本地选区".into());
            }
            Ok(())
        });
        drop(original); // 临时原生 claim 只活一次同步动作，不进入后继事件或关闭收据。
        result
    }

    /// 只取得原SessionFieldEditor；无编辑器或替代类均明确失败，不selectText偷偷抢焦点。
    fn field_editor(
        &self,
        surface: &NativeMarkdownWindowSurface,
        current: &impl Fn() -> bool,
    ) -> Result<Retained<NSTextView>, String> {
        let bar = self.bar.upgrade().ok_or("原查找栏已释放")?;
        let fields = step(current, || bar.fields())?;
        let field = step(current, || fields[0].currentEditor())?
            .ok_or("原查询字段失去实际输入编辑器")?
            .downcast::<NSTextView>()
            .map_err(|_| "查询输入不是NSTextView")?;
        if field
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_none()
            || !self.current(surface)
        {
            return Err("查询输入不属于原会话".into());
        }
        Ok(field)
    }

    /// 使用真实输入协议替换明确的有限查询；预声明一次通知，不伪造通知或直接改字段值。
    fn input(
        &mut self,
        surface: &NativeMarkdownWindowSurface,
        text: &str,
        marked: bool,
    ) -> Result<(), String> {
        let expected = self.next_find_epoch()?;
        {
            let current = || self.current(surface);
            let field = self.field_editor(surface, &current)?;
            let value = step(&current, || NSString::from_str(text))?;
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*field);
            let replacement = NSRange::new(0, self.query_units);
            // SAFETY: 原自有field editor及有限NSString存活；明确范围只替换本用例上次输入的查询。
            unsafe {
                if marked {
                    input.setMarkedText_selectedRange_replacementRange(
                        &value,
                        NSRange::new(text.encode_utf16().count(), 0),
                        replacement,
                    );
                } else {
                    input.insertText_replacementRange(&value, replacement);
                }
            }
            let after = || self.current_at(surface, expected);
            step(&after, || {
                surface.controller.observe_native_field_editor(&field)
            })
            .map_err(|cause| {
                // 失败后只记录原窗口现有标量，不能用实际新值替换预声明的输入权限。
                let actual = MemoryActionEpoch::capture(surface);
                format!(
                    "{cause}：marked={marked}，预期/实际Find={}/{}, workspace={}/{}, header={}/{}",
                    expected.find,
                    actual.find,
                    expected.workspace,
                    actual.workspace,
                    expected.header,
                    actual.header,
                )
            })?;
            if step(&after, || field.hasMarkedText())? != marked {
                return Err("实际查询组合输入状态与本次动作不符".into());
            }
            drop((value, field)); // 最后Native claim也必须在原预声明后态下归还。
            if !after() {
                return Err("查询输入最终释放改变了原权限".into());
            }
        }
        self.epoch = expected;
        self.query_units = text.encode_utf16().count();
        Ok(())
    }

    /// 只检查有限文首已知中文范围；实际clip中的完整glyph才可作为像素ROI。
    fn visible_chinese(
        &self,
        surface: &NativeMarkdownWindowSurface,
    ) -> Result<Range<usize>, String> {
        let current = || self.current(surface);
        let editor = surface.editor.as_ref().ok_or("高亮正文缺失")?;
        let paged = editor;
        let view = &**paged.text_view;
        let visible = step(&current, || view.visibleRect())?;
        for source in &self.chinese {
            // 只读已声明的有限源位置；不ensure离屏段落或把未知结构默认成普通正文。
            let frame = PixelSample::source_frame(view, &paged.body, source, &current);
            if let Ok(frame) = frame
                && sample_rect(frame, visible).is_some()
            {
                return Ok(source.clone());
            }
            if !current() {
                return Err("中文glyph定位期间原票已撤销".into());
            }
        }
        Err("实际可见窗口没有有限文首的完整中文glyph".into())
    }

    /// 有限glyph几何来自已知canonical范围；不以生产batch.frames作为绘制通过证据。
    fn sample(
        &self,
        surface: &NativeMarkdownWindowSurface,
        source: Range<usize>,
    ) -> Result<PixelSample, String> {
        let current = || self.current(surface);
        let editor = surface.editor.as_ref().ok_or("像素正文缺失")?;
        let paged = editor;
        PixelSample::capture(&paged.text_view, &paged.body, source, &current)
    }

    /// 每个事件结束核原保存摘要和磁盘身份；只读回归不能借dirty=false掩盖历史或数据改变。
    fn verify_document(
        &self,
        surface: &NativeMarkdownWindowSurface,
        path: &Path,
    ) -> Result<(), String> {
        if !self.current(surface) {
            return Err("高亮阶段改变了原选择、正文或空历史".into());
        }
        let document = surface
            .editor
            .as_ref()
            .ok_or("原正文缺失")?
            .document()
            .borrow();
        if document
            .current_revision()
            .map_err(|_| "无法读取高亮阶段正文revision")?
            != self.facts.revision
            || !document.original_file_identity_is_unchanged()
        {
            return Err("高亮阶段改变了原保存内容或文件身份".into());
        }
        drop(document);
        let metadata = std::fs::metadata(path).map_err(|_| "无法核高亮夹具磁盘身份")?;
        if (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ) != self.facts.disk
        {
            return Err("只读高亮阶段写入或替换了原夹具".into());
        }
        Ok(())
    }
}

impl PixelSample {
    /// 几何只取当前已封存Scene的完整GCB；中文查询可含两字，不能要求IME首框冒充整个词框。
    /// 只聚合同一软行带的实际字形，不跨行拉大ROI，也不以隐藏语法占位框替代真实内容。
    pub(super) fn source_frame(
        view: &NativeMarkdownPagedTextView,
        body: &NativeMarkdownPagedBody,
        source: &Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<NSRect, String> {
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request)
            .map_err(|cause| cause.to_string())?;
        scene
            .validate_ready(current)
            .map_err(|cause| cause.to_string())?;
        if !scene.key().same(&key) {
            return Err("有限像素几何没有原 Scene 权限".into());
        }
        let mut cursor = source.start;
        let mut frame: Option<NSRect> = None;
        while cursor < source.end {
            // first_rect的公开合同是首个完整GCB，而非请求范围内所有字符。原固定中文两字
            // 与单个nonBMP分别逐GCB取景，始终借同一Scene；不另求布局或读取正文。
            let actual = scene
                .first_rect(NSRange::new(cursor, source.end - cursor))
                .map_err(|cause| cause.to_string())?;
            let end = actual
                .actual_range
                .location
                .checked_add(actual.actual_range.length)
                .ok_or("有限像素实际字形范围溢出")?;
            if actual.actual_range.location != cursor || end <= cursor || end > source.end {
                return Err("有限像素目标未按完整GCB连续覆盖原范围".into());
            }
            frame = Some(if let Some(previous) = frame {
                if previous.origin.y != actual.rect.origin.y
                    || previous.size.height != actual.rect.size.height
                {
                    return Err("有限像素目标跨越实际软行或不同字形带".into());
                }
                // 左右对照仍包围完整查询。只截首字或末字会把查询另一字误当成未命中对照，
                // 因而这里仅合并真实同带字形的横向包络，原ROI尺寸上限继续在sample_rect核验。
                let left = previous.origin.x.min(actual.rect.origin.x);
                let right = (previous.origin.x + previous.size.width)
                    .max(actual.rect.origin.x + actual.rect.size.width);
                NSRect::new(
                    NSPoint::new(left, previous.origin.y),
                    NSSize::new(right - left, previous.size.height),
                )
            } else {
                actual.rect
            });
            cursor = end;
        }
        scene
            .validate_ready(current)
            .map_err(|cause| cause.to_string())?;
        frame.ok_or("有限像素目标没有真实完整字形".into())
    }

    /// 只截取原已安装分页视图的≤128×64像素；不能另调用 Scene.paint 生成一张替代通过图。
    /// AppKit bitmap、颜色与 Scene 强边全在同步栈归还，跨事件只留下128个RGB与原弱 key。
    pub(super) fn capture(
        view: &NativeMarkdownPagedTextView,
        body: &NativeMarkdownPagedBody,
        source: Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let (published, paints) = view
            .probe_cpu_published_frame()
            .ok_or("有限像素目标缺少实际 CPU 帧")?;
        if !published.same(&key)
            || paints == 0
            || view.cpu_paints() != Some(paints)
            || view.cpu_paint_pending() != Some(false)
            || !current()
        {
            return Err("有限像素目标不是原完整 CPU 发布帧".into());
        }
        let frame = Self::source_frame(view, body, &source, current)?;
        let visible = step(current, || view.visibleRect())?;
        let proposed = sample_rect(frame, visible).ok_or("实际字形和对照条不在有限可见区域")?;
        // 缓存区域必须先由同一个真实 View 对齐完整 backing 像素。此前把小数 ROI 的宽度
        // 平摊到整数 bitmap，会把 emoji 左缘的部分覆盖像素误签成相邻未命中点：其中心
        // 虽在字框外，原像素仍与字框相交。这里保留原左右各 8pt 对照，再向外像素对齐，
        // 不挪字形、不改颜色阈值，也不让 AppKit 隐含取整决定我们未记录的采样原点。
        let rect = step(current, || {
            view.backingAlignedRect_options(proposed, NSAlignmentOptions::AlignAllEdgesOutward)
        })?;
        let backing = step(current, || view.convertRectToBacking(rect))?;
        let glyph_backing = step(current, || view.convertRectToBacking(frame))?;
        // 向外对齐最多补足边缘像素，但仍须支付原 ROI/可见区上限，不能借对齐扩大观察预算。
        // backing 原点和边长必须确为整数；后续每个 getPixel 才对应一个完整单位像素，
        // 而不是按显示器倍率或 bitmap 与小数 rect 的宽度比推测坐标。
        if ![
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
            backing.origin.x,
            backing.origin.y,
            backing.size.width,
            backing.size.height,
            glyph_backing.origin.x,
            glyph_backing.origin.y,
            glyph_backing.size.width,
            glyph_backing.size.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || rect.size.width <= 0.0
            || rect.size.height <= 0.0
            || rect.size.width > 64.0
            || rect.size.height > 32.0
            || rect.origin.x < visible.origin.x
            || rect.origin.y < visible.origin.y
            || rect.origin.x + rect.size.width > visible.origin.x + visible.size.width
            || rect.origin.y + rect.size.height > visible.origin.y + visible.size.height
            || backing.size.width <= 0.0
            || backing.size.height <= 0.0
            || [
                backing.origin.x,
                backing.origin.y,
                backing.size.width,
                backing.size.height,
            ]
            .into_iter()
            .any(|value| value.fract() != 0.0)
        {
            return Err("像素对齐后的原字形采样区不在有限可见 backing 范围".into());
        }
        let sample = autoreleasepool(|_| {
            let bitmap = step(current, || view.bitmapImageRepForCachingDisplayInRect(rect))?
                .ok_or("AppKit 未创建原视图有限 bitmap")?;
            step(current, || {
                view.cacheDisplayInRect_toBitmapImageRep(rect, &bitmap)
            })?;
            let width = step(current, || bitmap.pixelsWide())?;
            let height = step(current, || bitmap.pixelsHigh())?;
            // 只接受同 View 已声明的逐 backing 像素缓存。其它实际分辨率应明确失败，
            // 不能再次线性缩放坐标把格式不符掩盖成合法未命中点。
            if width <= 0
                || height <= 0
                || width > 128
                || height > 64
                || width as f64 != backing.size.width
                || height as f64 != backing.size.height
            {
                return Err("实际 bitmap 未逐像素覆盖原 backing 范围或超出128×64预算".into());
            }
            // 实测colorAtX把显示器ICC的原分量包装成GenericRGB；不能据这个错误源空间再转换。
            // getPixel与原colorAtX共用左上角原点，仍只读同128个整数坐标，不引入图像绘制或Y翻转。
            let bitmap_space = step(current, || bitmap.colorSpace())?;
            let color_components = step(current, || bitmap_space.numberOfColorComponents())?;
            let model = step(current, || bitmap_space.colorSpaceModel())?;
            let (bps, spp, format, alpha) = step(current, || {
                (
                    bitmap.bitsPerSample(),
                    bitmap.samplesPerPixel(),
                    bitmap.bitmapFormat(),
                    bitmap.hasAlpha(),
                )
            })?;
            // getPixel对浮点样本会截成整数，无法保原颜色；只接受可以无损读出的整数布局。
            // 单色/RGB/CMYK至多四个色分量，真实alpha另占一项；不按显示器名字猜RGB或字节次序。
            if !(1..=32).contains(&bps)
                || !(1..=4).contains(&color_components)
                || spp != color_components + isize::from(alpha)
                || format.contains(NSBitmapFormat::FloatingPointSamples)
                || !matches!(
                    model,
                    NSColorSpaceModel::Gray | NSColorSpaceModel::RGB | NSColorSpaceModel::CMYK
                )
            {
                return Err("实际缓存像素格式不能按原颜色空间无损读取".into());
            }
            let color_components = color_components as usize;
            let maximum = ((1u64 << bps as u32) - 1) as usize;
            let alpha_first = alpha && format.contains(NSBitmapFormat::AlphaFirst);
            let premultiplied = alpha && !format.contains(NSBitmapFormat::AlphaNonpremultiplied);
            let space = step(current, NSColorSpace::sRGBColorSpace)?;
            let mut colors = [[0.0; 3]; PIXEL_SAMPLES];
            let mut in_match = [false; PIXEL_SAMPLES];
            for row in 0..SAMPLE_ROWS {
                for column in 0..SAMPLE_COLUMNS {
                    let index = row * SAMPLE_COLUMNS + column;
                    let x = ((2 * column + 1) as isize * width / (2 * SAMPLE_COLUMNS) as isize)
                        .min(width - 1);
                    let y = ((2 * row + 1) as isize * height / (2 * SAMPLE_ROWS) as isize)
                        .min(height - 1);
                    let mut raw = [0usize; 5];
                    step(current, || {
                        // SAFETY: spp已经证明为真实色分量数加可选alpha且≤5，原坐标仍完整位于bitmap。
                        // getPixel负责实际位深/planar/端序解码，数组顺序继续按bitmapFormat解释。
                        unsafe { bitmap.getPixel_atX_y(std::ptr::NonNull::from(&mut raw[0]), x, y) }
                    })?;
                    if raw[..spp as usize].iter().any(|value| *value > maximum) {
                        return Err("实际缓存像素分量超出原整数位深".into());
                    }
                    let alpha_sample = if alpha {
                        raw[if alpha_first { 0 } else { color_components }]
                    } else {
                        maximum
                    };
                    let mut components = [0.0; 5];
                    for (channel, component) in
                        components[..color_components].iter_mut().enumerate()
                    {
                        let sample = raw[channel + usize::from(alpha_first)];
                        // NSColor需要非预乘、alpha末置的真实ICC分量；仅原格式要求时解除预乘。
                        // 透明像素没有可恢复色值，按零色交付，不能除零或假设本图恒opaque。
                        *component = if premultiplied {
                            if alpha_sample == 0 {
                                0.0
                            } else {
                                sample as f64 / alpha_sample as f64
                            }
                        } else {
                            sample as f64 / maximum as f64
                        };
                    }
                    components[color_components] = alpha_sample as f64 / maximum as f64;
                    colors[index] = autoreleasepool(|_| {
                        let color = step(current, || {
                            // SAFETY: 完整原ICC分量及alpha末项都在固定数组内，count严格等于该空间要求。
                            unsafe {
                                NSColor::colorWithColorSpace_components_count(
                                    &bitmap_space,
                                    std::ptr::NonNull::from(&components[0]),
                                    (color_components + 1) as isize,
                                )
                            }
                        })?;
                        let rgb = step(current, || color.colorUsingColorSpace(&space))?
                            .ok_or("实际缓存ICC像素不能转换为 sRGB")?;
                        step(current, || {
                            [
                                rgb.redComponent(),
                                rgb.greenComponent(),
                                rgb.blueComponent(),
                            ]
                        })
                    })?;
                    // NSBitmap 的整数行从左上开始；本 NSView 使用原未翻转坐标。只换算
                    // 该已读像素在同一 backing 中的完整单位面积，不翻转或重采颜色数据。
                    // 任何正面积交集都属于命中边缘的合法覆盖；只有完全不相交的像素才是
                    // 严格未命中对照。恢复检查仍比较全部 128 点，包括这些部分覆盖像素。
                    let pixel_left = backing.origin.x + x as f64;
                    let pixel_bottom = backing.origin.y + backing.size.height - y as f64 - 1.0;
                    in_match[index] = pixel_left
                        < glyph_backing.origin.x + glyph_backing.size.width
                        && glyph_backing.origin.x < pixel_left + 1.0
                        && pixel_bottom < glyph_backing.origin.y + glyph_backing.size.height
                        && glyph_backing.origin.y < pixel_bottom + 1.0;
                }
            }
            #[cfg(feature = "native-block-fragment-diagnostic")]
            let color_diagnostic = {
                // 描述实际源空间而不改tag；名称缺席也保留真实model，不把未命名ICC空间当sRGB。
                let describe_cg = |value: Option<&CGColorSpace>| {
                    step(current, || {
                        let model = CGColorSpace::model(value).0;
                        let name = CGColorSpace::name(value).map(|name| name.to_string());
                        format!("present={} model={model} name={name:?}", value.is_some())
                    })
                };
                let describe = |value: &NSColorSpace| -> Result<String, String> {
                    let (model, name) = step(current, || {
                        (
                            value.colorSpaceModel().0,
                            value.localizedName().map(|name| name.to_string()),
                        )
                    })?;
                    let cg = step(current, || value.CGColorSpace())?;
                    Ok(format!(
                        "model={model} name={name:?} cg=({})",
                        describe_cg(cg.as_deref())?
                    ))
                };
                let bitmap_description = describe(&bitmap_space)?;
                let image = step(current, || bitmap.CGImage())?;
                let image_space = image
                    .as_deref()
                    .map(|image| step(current, || CGImage::color_space(Some(image))))
                    .transpose()?
                    .flatten();
                let image_description = describe_cg(image_space.as_deref())?;
                let bpp = step(current, || bitmap.bitsPerPixel())?;
                let last = in_match.iter().rposition(|inside| !inside);
                let control = if let Some(index) = last {
                    // 仍读原128点中的最后对照；不另取ROI、不改变字形命中或颜色通过标准。
                    let column = index % SAMPLE_COLUMNS;
                    let row = index / SAMPLE_COLUMNS;
                    let x = ((2 * column + 1) as isize * width / (2 * SAMPLE_COLUMNS) as isize)
                        .min(width - 1);
                    let y = ((2 * row + 1) as isize * height / (2 * SAMPLE_ROWS) as isize)
                        .min(height - 1);
                    let mut raw = [0usize; 5];
                    let raw = if (1..=raw.len() as isize).contains(&spp) {
                        step(current, || {
                            // SAFETY: spp已核为1..=5，固定数组覆盖全部实际分量；x/y仍在原bitmap内。
                            // getPixel只读原像素，不自行假定RGBA字节次序或解除预乘。
                            unsafe {
                                bitmap.getPixel_atX_y(std::ptr::NonNull::from(&mut raw[0]), x, y)
                            }
                        })?;
                        Some(raw)
                    } else {
                        None // 未支持的分量数明确记缺席，不能向小数组写入，也不改原像素判定。
                    };
                    let color = step(current, || bitmap.colorAtX_y(x, y))?
                        .ok_or("原最后对照点的颜色诊断不可读")?;
                    let color_space = step(current, || color.colorSpace())?;
                    let color_description = describe(&color_space)?;
                    let (count, color_alpha) = step(current, || {
                        (color.numberOfComponents(), color.alphaComponent())
                    })?;
                    let mut components = [0.0; 5];
                    let components = if (1..=components.len() as isize).contains(&count) {
                        step(current, || {
                            // SAFETY: 原NSColor的完整分量数已在固定数组内；包含其原alpha，不先转sRGB。
                            unsafe {
                                color.getComponents(std::ptr::NonNull::from(&mut components[0]))
                            }
                        })?;
                        Some(components)
                    } else {
                        None // 仅诊断保留未知格式，不插值、截断或伪造原色分量。
                    };
                    format!(
                        "index={index} x={x} y={y} raw_count={spp} raw_pixel={raw:?} color_space=({color_description}) color_count={count} color_components={components:?} color_alpha={color_alpha} source_icc_to_srgb={:?}",
                        colors[index]
                    )
                } else {
                    "present=false".into()
                };
                // 所有Native强边都在当前pool归还；跨事件只带格式、空间名和同点分量的有限文本。
                format!(
                    "bitmap_space=({bitmap_description}) image_space=({image_description}) bps={bps} spp={spp} bpp={bpp} format={} alpha={alpha} last_control=({control})",
                    format.0
                )
            };
            drop((space, bitmap_space, bitmap));
            Ok::<_, String>(Self {
                key: key.clone(),
                source: source.clone(),
                frame,
                rect,
                width,
                height,
                colors,
                in_match,
                #[cfg(feature = "native-block-fragment-diagnostic")]
                color_diagnostic,
            })
        })?;
        let after = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let stable_cpu = view
            .probe_cpu_published_frame()
            .is_some_and(|(actual, count)| actual.same(&key) && count == paints);
        if !current()
            || !after.same(&key)
            || !stable_cpu
            || view.cpu_paint_pending() != Some(false)
            || step(current, || view.visibleRect())? != visible
            || Self::source_frame(view, body, &source, current)? != frame
        {
            return Err("原视图采样或临时对象归还后，原 CPU 帧、clip 或字形已变化".into());
        }
        Ok(sample)
    }

    /// 同一个真实glyph和采样网格才可比较颜色，不能将模式/滚动后的不同位置拼成差分。
    fn same_region(&self, actual: &Self) -> bool {
        self.key.source == actual.key.source
            && self.key.presentation_mode == actual.key.presentation_mode
            && WeakMarkdownDocument::ptr_eq(
                &self.key.request.document,
                &actual.key.request.document,
            )
            && self.source == actual.source
            && self.frame == actual.frame
            && self.rect == actual.rect
            && self.width == actual.width
            && self.height == actual.height
            && self.in_match == actual.in_match
    }

    /// 命中区必须出现实际金色混合变化，旁边非命中像素仍保持原值；单纯有geometry不能通过。
    fn require_highlight(&self, actual: &Self) -> Result<(), String> {
        if !self.same_region(actual) {
            return Err("高亮前后不是同一个真实有限glyph区域".into());
        }
        let mut tinted = 0;
        let mut controls = 0;
        for index in 0..PIXEL_SAMPLES {
            let delta = std::array::from_fn::<_, 3, _>(|channel| {
                actual.colors[index][channel] - self.colors[index][channel]
            });
            if self.in_match[index] {
                if delta[0] > 0.02 && delta[0] > delta[2] + 0.025 && delta[1] > delta[2] + 0.02 {
                    tinted += 1; // 真实gold tint相对同点基线的色差，不把普通文字或彩色emoji本身记作高亮。
                }
            } else {
                controls += 1;
                if delta.into_iter().any(|value| value.abs() > 0.025) {
                    // 只在原首个失败点保留已有几何和同点颜色；不另取图、不读查询或正文。
                    // 完整范围与像素阈值仍原样拒绝，供区分真正越界绘制和边缘采样前提。
                    return Err(format!(
                        "高亮误染了相邻非命中区域：source={:?} frame={:?} rect={:?} bitmap={}x{} index={index} before={:?} after={:?} delta={delta:?}",
                        self.source,
                        self.frame,
                        self.rect,
                        self.width,
                        self.height,
                        self.colors[index],
                        actual.colors[index],
                    ));
                }
            }
        }
        if tinted < 4 || controls < 8 {
            return Err(format!(
                "实际像素没有有限命中高亮或对照不足：tinted={tinted} controls={controls}"
            ));
        }
        Ok(())
    }

    /// 清空/marked/Hide后必须真正回到原像素，不能仅删除Rust几何或重签当前状态。
    fn require_restored(&self, actual: &Self) -> Result<(), String> {
        if !self.same_region(actual)
            || self
                .colors
                .iter()
                .zip(&actual.colors)
                .any(|(before, after)| {
                    before
                        .iter()
                        .zip(after)
                        .any(|(before, after)| (before - after).abs() > 0.025)
                })
        {
            return Err("撤旧后有限正文像素未恢复原基线".into());
        }
        Ok(())
    }

    /// Hide真实增大有限正文的局部高度，旧TextKit绝对Y不再是Paged相同glyph的坐标合同。
    /// 只认可原栏占高引起的精确平移；文档原点、原Source/Input和相对ROI均须不变。
    fn require_hidden_restored(&self, actual: &Self, bar_height: f64) -> Result<(), String> {
        let before = &self.key.request;
        let after = &actual.key.request;
        let same_source = self.key.source == actual.key.source
            && self.key.presentation_mode == actual.key.presentation_mode
            && WeakMarkdownDocument::ptr_eq(&before.document, &after.document)
            && before.target.same(&after.target)
            && self.source == actual.source;
        let same_input = before.input == after.input;
        // 关闭栏只能归还它自身的实际高度，不能掩盖页底夹滚、横向滚动、换宽或后继用户操作。
        // virtual document_origin是原有限正文的真实文档坐标，不用本地visibleRect的零点代替。
        let translated = bar_height.is_finite()
            && bar_height > 0.0
            && before.bounds_width == after.bounds_width
            && before.virtual_viewport.document_origin == after.virtual_viewport.document_origin
            && before.virtual_viewport.layout_origin_x == after.virtual_viewport.layout_origin_x
            && before.virtual_viewport.layout_width == after.virtual_viewport.layout_width
            && before.viewport.origin == after.viewport.origin
            && before.viewport.size.width == after.viewport.size.width
            && after.viewport.size.height - before.viewport.size.height == bar_height
            && before.frame_in_parent.origin == after.frame_in_parent.origin
            && before.frame_in_parent.size.width == after.frame_in_parent.size.width
            && after.frame_in_parent.size.height - before.frame_in_parent.size.height == bar_height;
        let mut frame = self.frame;
        frame.origin.y += bar_height;
        let mut rect = self.rect;
        rect.origin.y += bar_height;
        // 同完整字形及其左右原8pt对照只平移，像素尺寸和128个命中标记仍逐项一致。
        // 不缩成首字、不换采样点，也不重新绘制一幅没有真实发布的替代图。
        let same_grid = actual.frame == frame
            && actual.rect == rect
            && actual.width == self.width
            && actual.height == self.height
            && actual.in_match == self.in_match;
        let colors_restored = !self
            .colors
            .iter()
            .zip(&actual.colors)
            .any(|(before, after)| {
                before
                    .iter()
                    .zip(after)
                    .any(|(before, after)| (before - after).abs() > 0.025)
            });
        if same_source && same_input && translated && same_grid && colors_restored {
            return Ok(());
        }
        // 失败仅消费原128点已有纯值；区分错几何与残色，不补采样、不改原.025颜色门槛。
        let maximum_delta = self
            .colors
            .iter()
            .zip(&actual.colors)
            .flat_map(|(before, after)| before.iter().zip(after))
            .map(|(before, after)| (before - after).abs())
            .fold(0.0_f64, f64::max);
        Err(format!(
            "Hide后有限正文像素未恢复原基线：same_source={same_source} same_input={same_input} hide_translation={translated} same_grid={same_grid} colors_restored={colors_restored} original_same_region={} bar_height={bar_height} old_viewport={:?} new_viewport={:?} old_document_origin={:?} new_document_origin={:?} old_frame={:?} new_frame={:?} old_rect={:?} new_rect={:?} old_pixels={}x{} new_pixels={}x{} max_rgb_delta={maximum_delta}",
            self.same_region(actual),
            before.viewport,
            after.viewport,
            before.virtual_viewport.document_origin,
            after.virtual_viewport.document_origin,
            self.frame,
            actual.frame,
            self.rect,
            actual.rect,
            self.width,
            self.height,
            actual.width,
            actual.height,
        ))
    }
}

/// ROI包括原glyph与左右各8pt非命中对照，始终完整位于真实clip且不超过64x32pt。
fn sample_rect(frame: NSRect, visible: NSRect) -> Option<NSRect> {
    let rect = NSRect::new(
        NSPoint::new(frame.origin.x - 8.0, frame.origin.y),
        NSSize::new(frame.size.width + 16.0, frame.size.height),
    );
    (rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
        && rect.size.width.is_finite()
        && rect.size.height.is_finite()
        && frame.size.width > 0.0
        && frame.size.height > 0.0
        && rect.size.width <= 64.0
        && rect.size.height <= 32.0
        && rect.origin.x >= visible.origin.x
        && rect.origin.y >= visible.origin.y
        && rect.origin.x + rect.size.width <= visible.origin.x + visible.size.width
        && rect.origin.y + rect.size.height <= visible.origin.y + visible.size.height)
        .then_some(rect)
}

/// 复用原Native操作边界；本用例没有新的通用Validate层或另一套调度协议。
fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work).map_err(|_| "高亮像素原动作已撤权".into())
}
