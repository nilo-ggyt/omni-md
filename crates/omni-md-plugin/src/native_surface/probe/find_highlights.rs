//! 原长文窗口的有限像素回归；只保存弱权限和颜色样本，不保存图像、正文副本或另一份布局。

use super::*;
use crate::native_surface::find_bar::NativeMarkdownFindHighlightBatch;
use crate::native_surface::source_decorations::NativeMarkdownDecorationGeometry;
use objc2_app_kit::NSColorSpace;
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
    scrolled_from: Option<(NSRect, std::rc::Weak<NativeMarkdownFindHighlightBatch>)>,
}

/// 一个真实glyph及右侧未命中对照共用同一小bitmap；只复制128个RGB标量。
struct PixelSample {
    source: Range<usize>,
    frame: NSRect,
    rect: NSRect,
    width: isize,
    height: isize,
    colors: [[f64; 3]; PIXEL_SAMPLES],
    in_match: [bool; PIXEL_SAMPLES],
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
                FindHighlightsProbe::capture(surface, facts)
            })?;
            *self.find_highlights.borrow_mut() = Some(probe);
            return self.change_mode(true); // 沿原产品模式收据推进，不手动补跑executor。
        }
        if stage == 426 || stage == 434 {
            self.verify_mode(stage == 426)?; // 原模式真实完成后，才读取相应正文的像素。
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
            if stage == 433 {
                self.change_mode(false)?; // 仅真实Preview转换；下一事件仍核原mode收据。
            } else if stage == 435 {
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
                    let baseline = probe.preview_baseline.as_ref().ok_or("缺少Preview基线")?;
                    let clear = probe.sample(surface, baseline.source.clone())?;
                    baseline.require_restored(&clear)?;
                    probe.verify_document(surface, &self.path)
                })?;
                println!(
                    "native_find_highlight verified=true source_pixels=true preview_pixels=true unicode_non_bmp=true ime_query_no_provisional=true old_geometry_revoked=true selection_unchanged=true empty_history_unchanged=true nonempty_undo_redo_verified=false screen_present_verified=false"
                );
            }
            Ok(())
        })();
        if stage != 435 || result.is_err() {
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
        let bar = editor.find_bar.borrow().clone().ok_or("高亮验收缺少原栏")?;
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
                    .find_bar
                    .borrow()
                    .as_ref()
                    .is_some_and(|bar| std::rc::Weak::ptr_eq(&self.bar, &Rc::downgrade(bar)));
                let document = editor.document().borrow();
                same_bar
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
        let view = &editor.backend.text_view;
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        match stage {
            426 => {
                let current = || self.current(surface);
                let scroll = &editor.backend.scroll_view;
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
                    .ivars()
                    .source_decorations
                    .borrow()
                    .iter()
                    .find_map(|geometry| match geometry {
                        NativeMarkdownDecorationGeometry::Find(batch) => Some(Rc::downgrade(batch)),
                        NativeMarkdownDecorationGeometry::Source { .. }
                        | NativeMarkdownDecorationGeometry::PreviewVisibility(_) => None,
                    })
                    .ok_or("滚动前没有原高亮批次")?;
                let scroll = &editor.backend.scroll_view;
                let clip = step(&current, || scroll.contentView())?;
                step(&current, || {
                    clip.scrollToPoint(NSPoint::new(
                        visible.origin.x,
                        visible.origin.y + visible.size.height * 0.5,
                    ));
                })?;
                step(&current, || scroll.reflectScrolledClipView(&clip))?;
                drop(clip);
                self.scrolled_from = Some((visible, old));
                // 原clip已改变，旧batch此刻必须拒绝绘制；有限真实paint本身也会沿产品路径排settled刷新。
                let source = self.visible_chinese(surface)?;
                self.scrolled_baseline = Some(self.sample(surface, source)?);
            }
            433 => {
                let current = || self.current(surface);
                let visible = step(&current, || view.visibleRect())?;
                let width = step(&current, || view.bounds().size.width)?;
                let (old_visible, old) = self.scrolled_from.as_ref().ok_or("缺少原滚动票")?;
                if visible == *old_visible {
                    return Err("真实滚动没有改变原clip".into());
                }
                if let Some(old) = old.upgrade()
                    && old.current(
                        view,
                        &editor.backend.text_storage,
                        &surface.controller,
                        visible,
                        width,
                    )
                {
                    return Err("滚动后的旧高亮几何仍有权限".into());
                }
                let baseline = self
                    .scrolled_baseline
                    .as_ref()
                    .ok_or("缺少新clip的真实无亮区基线")?;
                let source = baseline.source.clone();
                let highlighted = self.sample(surface, source.clone())?;
                baseline.require_highlight(&highlighted)?;
                self.input(surface, "", false)?;
                let clear = self.sample(surface, source)?;
                self.scrolled_baseline
                    .as_ref()
                    .ok_or("新clip基线在明确查询输入后丢失")?
                    .require_restored(&clear)?;
            }
            434 => {
                let source = self.visible_chinese(surface)?;
                self.preview_baseline = Some(self.sample(surface, source)?);
                self.input(surface, "中文", false)?;
            }
            435 => {
                let baseline = self.preview_baseline.as_ref().ok_or("缺少Preview基线")?;
                baseline.require_highlight(&self.sample(surface, baseline.source.clone())?)?;
            }
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
        show_native_markdown_find_bar(true)?;
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
                    "{cause}：marked={marked}，预期/实际Find={}/{}, workspace={}/{}, header={}/{}, table={}/{}",
                    expected.find,
                    actual.find,
                    expected.workspace,
                    actual.workspace,
                    expected.header,
                    actual.header,
                    expected.table,
                    actual.table,
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
        let view = &surface
            .editor
            .as_ref()
            .ok_or("高亮正文缺失")?
            .backend
            .text_view;
        let visible = step(&current, || view.visibleRect())?;
        for source in &self.chinese {
            // 只读已声明的有限源位置；不ensure离屏段落或把未知结构默认成普通正文。
            let frame = view.source_probe_segment(source, &current);
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
        let view = &surface
            .editor
            .as_ref()
            .ok_or("像素正文缺失")?
            .backend
            .text_view;
        let frame = view
            .source_probe_segment(&source, &current)
            .map_err(|_| "实际glyph几何不可用")?;
        let visible = step(&current, || view.visibleRect())?;
        let rect = sample_rect(frame, visible).ok_or("实际glyph和对照条不在有限可见区域")?;
        let sample = autoreleasepool(|_| {
            let bitmap = step(&current, || {
                view.bitmapImageRepForCachingDisplayInRect(rect)
            })?
            .ok_or("AppKit未创建有限高亮bitmap")?;
            step(&current, || {
                view.cacheDisplayInRect_toBitmapImageRep(rect, &bitmap)
            })?;
            let width = step(&current, || bitmap.pixelsWide())?;
            let height = step(&current, || bitmap.pixelsHigh())?;
            if width <= 0 || height <= 0 || width > 128 || height > 64 {
                return Err("高亮bitmap超出128x64实际像素预算".into());
            }
            let space = step(&current, NSColorSpace::sRGBColorSpace)?;
            let mut colors = [[0.0; 3]; PIXEL_SAMPLES];
            let mut in_match = [false; PIXEL_SAMPLES];
            for row in 0..SAMPLE_ROWS {
                for column in 0..SAMPLE_COLUMNS {
                    // 固定网格覆盖真实glyph和右侧非命中对照；每个颜色的Native claim局部归还。
                    let index = row * SAMPLE_COLUMNS + column;
                    let x = ((2 * column + 1) as isize * width / (2 * SAMPLE_COLUMNS) as isize)
                        .min(width - 1);
                    let y = ((2 * row + 1) as isize * height / (2 * SAMPLE_ROWS) as isize)
                        .min(height - 1);
                    colors[index] = autoreleasepool(|_| {
                        let color = step(&current, || bitmap.colorAtX_y(x, y))?
                            .ok_or("bitmap像素不可读")?;
                        let rgb = step(&current, || color.colorUsingColorSpace(&space))?
                            .ok_or("像素无法转换为sRGB")?;
                        let channels = step(&current, || {
                            [
                                rgb.redComponent(),
                                rgb.greenComponent(),
                                rgb.blueComponent(),
                            ]
                        })?;
                        drop((rgb, color));
                        Ok::<_, String>(channels)
                    })?;
                    let view_x = rect.origin.x + (x as f64 + 0.5) * rect.size.width / width as f64;
                    in_match[index] = view_x < frame.origin.x + frame.size.width;
                }
            }
            drop((space, bitmap)); // 图像和颜色空间不得跨事件；只返回有限颜色标量。
            Ok::<_, String>(PixelSample {
                source: source.clone(),
                frame,
                rect,
                width,
                height,
                colors,
                in_match,
            })
        })?;
        if !current()
            || step(&current, || view.visibleRect())? != visible
            || view
                .source_probe_segment(&source, &current)
                .map_err(|_| "像素结束后glyph失效")?
                != frame
        {
            return Err("实际paint或局部pool归还后原clip/glyph已变化".into());
        }
        Ok(sample)
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
    /// 同一个真实glyph和采样网格才可比较颜色，不能将模式/滚动后的不同位置拼成差分。
    fn same_region(&self, actual: &Self) -> bool {
        self.source == actual.source
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
                    return Err("高亮误染了相邻非命中区域".into());
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
}

/// ROI包括原glyph与8pt非命中对照，始终完整位于真实clip且不超过64x32pt。
fn sample_rect(frame: NSRect, visible: NSRect) -> Option<NSRect> {
    let rect = NSRect::new(
        frame.origin,
        NSSize::new(frame.size.width + 8.0, frame.size.height),
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
