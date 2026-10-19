//! 同一真实TextKit图上的列表标记属性回归；只读有限属性与有限源unit，不建立替代正文或编辑器。

use super::*;
use objc2_core_foundation::{CFAttributedString, CGSize};
use objc2_core_text::{CTFontOrientation, CTGlyphInfo, CTLine, CTRun};

/// 纯测试权限只借原surface/controller；root只有Weak，允许的模式/选区转换须在首Native前声明。
struct Permission<'a> {
    surface: &'a NativeMarkdownEditorSurface,
    controller: &'a Retained<NativeMarkdownController>,
    document: WeakMarkdownDocument,
    generation: u64,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    length: usize,
    epochs: [u64; 4],
    fold_revision: Cell<u64>,
    next_fold_revision: Cell<Option<u64>>,
    mode: Cell<MarkdownPresentationMode>,
    next_mode: Cell<Option<MarkdownPresentationMode>>,
    selection: Cell<MarkdownTextSelection>,
    next_selection: Cell<Option<MarkdownTextSelection>>,
    // 诊断仅记录固定阶段、原坐标与角色；不成为权限、重试或后继输入的基线。
    phase: Cell<&'static str>,
    step_sequence: Cell<u64>,
    attribute_index: Cell<usize>,
    attribute_role: Cell<MarkdownListGlyphRole>,
}

impl Permission<'_> {
    fn current(&self) -> bool {
        let view = &self.surface.backend.text_view;
        if !native_markdown_controller_has_foreground_authority(self.controller)
            || view.ivars().textkit_graph_teardown.get()
            || self.controller.find_input_intent() != self.epochs[0]
            || self.controller.workspace_query_intent() != self.epochs[1]
            || self.controller.ivars().header_layout_revision.get() != self.epochs[2]
            || view.ivars().table_input_intent_epoch.get() != self.epochs[3]
            || !WeakMarkdownDocument::ptr_eq(
                &Rc::downgrade(self.surface.document()),
                &self.document,
            )
        {
            return false; // 新输入、owner替换或accepted close都不能继续本次属性测试。
        }
        let fold_revision = view.ivars().folds.revision();
        if !view.ivars().folds.owner_is_current()
            || !view.ivars().folds.borrow().is_empty()
            || !(fold_revision == self.fold_revision.get()
                || self.next_fold_revision.get() == Some(fold_revision))
        {
            return false; // 本回归不接受新增折叠；模式切换只允许预声明的一次revision前进。
        }
        let mode = self.surface.backend.text_storage.presentation_mode();
        let document = self.surface.document().borrow();
        let selection = document.selection();
        let selection_and_fold = match self.next_selection.get() {
            Some(next) => {
                // 选择通知在同一纯Rust栈内成对发布选择与revision；禁止把两种许可交叉拼接。
                (selection == self.selection.get() && fold_revision == self.fold_revision.get())
                    || (selection == next && self.next_fold_revision.get() == Some(fold_revision))
            }
            None => {
                // 没有选择动作时不接受新端点；模式动作的唯一revision后继仍由上方原声明约束。
                selection == self.selection.get()
            }
        };
        document.generation() == self.generation
            && document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision)
            && document.utf16_len() == self.length
            && (mode == self.mode.get() || self.next_mode.get() == Some(mode))
            && selection_and_fold
    }

    fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        self.step_sequence
            .set(self.step_sequence.get().saturating_add(1));
        native_markdown_table_intent_step(&|| self.current(), work).map_err(|_| {
            format!(
                "列表标记属性回归的原窗口、正文或输入权限已变更：{}",
                self.permission_difference()
            )
        })
    }

    fn require_current(&self) -> Result<(), String> {
        self.current()
            .then_some(())
            .ok_or_else(|| format!("列表标记属性回归已撤权：{}", self.permission_difference()))
    }

    /// pool销毁也必须重核；已有首因不能被随后相同的撤权信息覆盖。
    fn finish_pool<T>(&self, result: Result<T, String>, phase: &'static str) -> Result<T, String> {
        self.phase.set(phase);
        match (result, self.require_current()) {
            (Err(first), Err(after)) => Err(format!("{first}；pool结束后：{after}")),
            (Err(first), Ok(())) => Err(first),
            (Ok(_), Err(after)) => Err(after),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    /// 失败后只读取Rust身份和标量；禁止为诊断再调用Native getter或用这些值重新签票。
    fn permission_difference(&self) -> String {
        let view = &self.surface.backend.text_view;
        let identity =
            WeakMarkdownDocument::ptr_eq(&Rc::downgrade(self.surface.document()), &self.document);
        let actual_epochs = [
            self.controller.find_input_intent(),
            self.controller.workspace_query_intent(),
            self.controller.ivars().header_layout_revision.get(),
            view.ivars().table_input_intent_epoch.get(),
        ];
        let document = self.surface.document().borrow();
        format!(
            "phase={} step={} index={} role={:?} foreground={} teardown={} weak_match={} generation={}/{} revision_match={} length={}/{} epochs={actual_epochs:?}/{:?} fold_owner={} fold_empty={} fold_revision={}/{} next_fold={:?} mode={:?}/{:?} next_mode={:?} selection={:?} before={:?} next_selection={:?}",
            self.phase.get(),
            self.step_sequence.get(),
            self.attribute_index.get(),
            self.attribute_role.get(),
            native_markdown_controller_has_foreground_authority(self.controller),
            view.ivars().textkit_graph_teardown.get(),
            identity,
            document.generation(),
            self.generation,
            document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision),
            document.utf16_len(),
            self.length,
            self.epochs,
            view.ivars().folds.owner_is_current(),
            view.ivars().folds.borrow().is_empty(),
            view.ivars().folds.revision(),
            self.fold_revision.get(),
            self.next_fold_revision.get(),
            self.surface.backend.text_storage.presentation_mode(),
            self.mode.get(),
            self.next_mode.get(),
            document.selection(),
            self.selection.get(),
            self.next_selection.get()
        )
    }

    fn select(&self, selection: MarkdownTextSelection) -> Result<(), String> {
        self.phase.set("selection_prepare");
        let after_fold = self.prepare_selection_transition(selection)?;
        self.phase.set("selection_native_setter");
        self.step(|| {
            self.surface
                .backend
                .text_view
                .set_semantic_native_selection(selection)
        })?;
        self.settle_selection_transition(selection, after_fold)?;
        self.phase.set("selection_primary_getter");
        // 原生方向读取也走生产TK2 helper，不能只凭升序selectedRange声称恢复了anchor/head。
        let captured = self
            .step(|| self.surface.backend.text_view.primary_text_selection())?
            .map_err(|_| "列表标记回归无法读取真实主选区")?;
        if captured.selection != selection {
            return Err("列表标记回归的原生主选区方向不符".into());
        }
        self.phase.set("selection_complete");
        self.require_current()
    }

    /// 原fold为空时，publish_head只对实际完整选区变化advance一次；相同选区不授权revision变化。
    fn prepare_selection_transition(
        &self,
        selection: MarkdownTextSelection,
    ) -> Result<u64, String> {
        self.require_current()?;
        let changed = self.selection.get() != selection;
        let after_fold = self
            .fold_revision
            .get()
            .checked_add(u64::from(changed))
            .ok_or("列表标记选择转换的fold revision耗尽")?;
        // 首Native前签好唯一after；即使getter回调产生其他选择或多次advance，也不能冒领此票。
        self.next_selection.set(Some(selection));
        self.next_fold_revision.set(Some(after_fold));
        Ok(after_fold)
    }

    /// 只能消费调用前签出的after，不用Native返回后的实际revision给旧选择重新签权。
    fn settle_selection_transition(
        &self,
        selection: MarkdownTextSelection,
        after_fold: u64,
    ) -> Result<(), String> {
        self.require_current()?;
        if self.surface.document().borrow().selection() != selection
            || self.surface.backend.text_view.ivars().folds.revision() != after_fold
            || self.next_selection.get() != Some(selection)
            || self.next_fold_revision.get() != Some(after_fold)
        {
            return Err(format!(
                "列表标记选择转换未取得原预声明的完整选区与fold后继：{}",
                self.permission_difference()
            ));
        }
        // 确切before→after结算只有纯Rust字段发布，不在中途调用Native或接受另一个输入动作。
        self.selection.set(selection);
        self.fold_revision.set(after_fold);
        self.next_selection.set(None);
        self.next_fold_revision.set(None);
        self.require_current()
    }

    fn change_mode(&self, mode: MarkdownPresentationMode) -> Result<(), String> {
        self.require_current()?;
        if self.surface.backend.text_storage.presentation_mode() == mode {
            return Ok(()); // 原模式已恢复，不制造额外模式动作或布局阶段。
        }
        let receipt = self.step(|| {
            capture_native_markdown_probe_mode_receipt(self.surface, self.controller, mode)
        })??;
        let next_fold = self
            .fold_revision
            .get()
            .checked_add(1)
            .ok_or("列表标记fold revision耗尽")?;
        self.next_fold_revision.set(Some(next_fold));
        self.next_mode.set(Some(mode)); // 原request先捕获，随后只允许它声明的before→after。
        self.step(|| {
            self.surface
                .backend
                .set_presentation_mode(mode, self.controller)
        })??;
        if self.surface.backend.text_storage.presentation_mode() != mode {
            return Err("列表标记回归未实际切换展示模式".into());
        }
        if self.surface.backend.text_view.ivars().folds.revision() != next_fold {
            return Err("列表标记模式切换没有取得预声明fold revision".into());
        }
        self.fold_revision.set(next_fold);
        self.next_fold_revision.set(None);
        self.mode.set(mode);
        self.next_mode.set(None);
        // 只调用一次现有有界executor；不使用probe旧helper的ensureLayoutForBounds(view.bounds)。
        self.step(|| {
            self.surface
                .backend
                .text_view
                .run_scheduled_table_layout_update()
        })?;
        if receipt.completed() != Some(true) {
            return Err("列表标记模式往返未取得原真实定位完成收据".into());
        }
        drop(receipt); // 收据仅Weak/标量；局部context已在工厂返回前析构，结束后仍核原权限。
        self.require_current()
    }

    fn graph(&self) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let manager = self
                .step(|| self.surface.backend.text_view.textLayoutManager())?
                .ok_or("列表标记回归丢失TK2 manager")?;
            let content = self
                .step(|| manager.textContentManager())?
                .ok_or("列表标记回归丢失content manager")?;
            // 同时核view与manager指向的原container，再核反向边；仅manager身份不能证明图未被拆接。
            let view_container = self
                .step(|| {
                    // SAFETY: 原surface强持有装配时的container；step在主线程核对未拆图权限。
                    unsafe { self.surface.backend.text_view.textContainer() }
                })?
                .ok_or("列表标记回归丢失view container")?;
            let manager_container = self
                .step(|| manager.textContainer())?
                .ok_or("列表标记回归丢失manager container")?;
            let container_manager = self
                .step(|| view_container.textLayoutManager())?
                .ok_or("列表标记回归丢失container反向manager")?;
            let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(
                &*self.surface.backend.text_content_storage,
            );
            let storage = self
                .step(|| observer.textStorage())?
                .ok_or("列表标记回归丢失piece storage")?;
            if !std::ptr::eq(&*manager, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq(&*view_container, &*self.surface.backend.text_container)
                || !std::ptr::eq(&*manager_container, &*self.surface.backend.text_container)
                || !std::ptr::eq(
                    &*container_manager,
                    &*self.surface.backend.text_layout_manager,
                )
                || !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                    &*content,
                    self.surface.backend.text_content_storage.as_super(),
                )
                || !std::ptr::eq::<NSTextStorage>(&*storage, self.surface.backend.text_storage())
            {
                return Err("列表标记模式往返更换了真实TextKit图".into());
            }
            Ok(())
        });
        self.require_current()?; // 真实图claim不跨下一次模式切换或关闭。
        result
    }

    /// 从真实storage读属性与字形，所有对象只活在局部pool，返回后只保留断言结果。
    fn attribute(&self, index: usize, role: MarkdownListGlyphRole) -> Result<(), String> {
        self.attribute_index.set(index);
        self.attribute_role.set(role);
        self.phase.set("attribute_native_getter");
        if index >= self.length {
            return Err("列表标记属性请求超出原g0长度".into());
        }
        let result = autoreleasepool(|_| {
            let mut range = NSRange::new(0, 0);
            let attrs = self.step(|| {
                // SAFETY: 首Native前确认index在原g0长度内；range为本栈有效可写输出。
                unsafe {
                    NSAttributedString::attributesAtIndex_effectiveRange(
                        self.surface.backend.text_storage(),
                        index,
                        &mut range,
                    )
                }
            })?;
            if index >= self.length
                || range.length == 0
                || range.length > NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
                || range.location > index
                || range
                    .location
                    .checked_add(range.length)
                    .is_none_or(|end| end <= index || end > self.length)
            {
                return Err("列表标记实际属性范围没有有限覆盖原unit".into());
            }
            // SAFETY: AppKit属性键为进程期不可变NSString；实际值下转前逐项核对类型。
            let (font_key, glyph_key, color_key) = unsafe {
                (
                    NSFontAttributeName,
                    NSGlyphInfoAttributeName,
                    NSForegroundColorAttributeName,
                )
            };
            self.phase.set("attribute_font_and_glyph");
            let font = self
                .step(|| attrs.objectForKey(font_key))?
                .ok_or("列表标记没有真实字体")?;
            let font = self
                .step(|| font.downcast::<NSFont>())?
                .map_err(|_| "列表标记字体类型不符")?;
            let size = self.step(|| font.pointSize())?;
            let info = self.step(|| attrs.objectForKey(glyph_key))?;
            let expected = match role {
                MarkdownListGlyphRole::Dash => Some((u16::from(b'-'), 0x2022_u16)),
                MarkdownListGlyphRole::Plus => Some((u16::from(b'+'), 0x2022)),
                MarkdownListGlyphRole::Star => Some((u16::from(b'*'), 0x2022)),
                MarkdownListGlyphRole::Unchecked => Some((u16::from(b'['), 0x2610)),
                MarkdownListGlyphRole::Checked => Some((u16::from(b'['), 0x2611)),
                MarkdownListGlyphRole::Literal | MarkdownListGlyphRole::Hidden => None,
            };
            if let Some((source, symbol)) = expected {
                if range != NSRange::new(index, 1) || (size - 14.0).abs() > 0.001 {
                    return Err("列表字形不是完整单unit的14pt属性".into());
                }
                let info = info.ok_or("非活动列表缺少实际NSGlyphInfo")?;
                let info = self
                    .step(|| info.downcast::<NSGlyphInfo>())?
                    .map_err(|_| "列表字形类型不符")?;
                let base = self.step(|| info.baseString())?;
                if self.step(|| base.length())? != 1
                    || self.step(|| {
                        // 上一原票步骤确认base仅有一个unit，索引0有效；公开getter本身是安全API。
                        base.characterAtIndex(0)
                    })? != source
                {
                    return Err("列表字形的baseString不是精确的一个源字符".into());
                }
                let mut symbol = symbol;
                let mut glyph = 0_u16;
                let core_font: &CTFont = font.as_ref();
                let covered = self.step(|| {
                    // SAFETY: 单个BMP符号与单个输出glyph均在本栈，指针非空且本调用内有效。
                    unsafe {
                        core_font.glyphs_for_characters(
                            NonNull::from(&mut symbol),
                            NonNull::from(&mut glyph),
                            1,
                        )
                    }
                })?;
                let legacy_getter = self.step(|| info.glyphID())?;
                let core_info: &CTGlyphInfo = info.as_ref();
                let core_getter = self.step(|| {
                    // SAFETY: NSGlyphInfo与CTGlyphInfo公开toll-free桥接；info仍在本pool强持有。
                    unsafe { core_info.glyph() }
                })?;
                let font_name = self.step(|| font.fontName())?;
                let font_name = self.step(|| font_name.to_string())?;
                let diagnostic = format!(
                    "role={role:?} index={index} font={font_name} covered={covered} expected={glyph} ns_info={legacy_getter} ct_info={core_getter}"
                );
                if !covered || glyph == 0 {
                    return Err(format!("实际字体不覆盖应显示的bullet/任务框：{diagnostic}"));
                }
                println!("native_list_marker_glyph_info {diagnostic}");
                // 属性getter值只用于定位API差异；成功必须由同一实际字典排版出的run字形与宽度证明。
                let (shaped, advance) = self
                    .shaped_single_unit(index, &attrs, &font, source, glyph)
                    .map_err(|error| format!("{error}；{diagnostic}"))?;
                println!(
                    "native_list_marker_shaped role={role:?} expected={glyph} shaped={shaped} advance={advance:.6}"
                );
            } else {
                if info.is_some() {
                    return Err("Source、活动标记或隐藏尾部残留旧glyphInfo".into());
                }
                if role == MarkdownListGlyphRole::Hidden {
                    self.phase.set("attribute_hidden_color");
                    let color = self
                        .step(|| attrs.objectForKey(color_key))?
                        .ok_or("任务框尾部缺少颜色")?;
                    let color = self
                        .step(|| color.downcast::<NSColor>())?
                        .map_err(|_| "任务框尾部颜色类型不符")?;
                    if (size - 0.1).abs() > 0.001 || self.step(|| color.alphaComponent())? != 0.0 {
                        return Err("任务框尾部没有按0.1pt透明属性收起".into());
                    }
                } else if !size.is_finite() || size < 1.0 {
                    return Err("活动或Source标记没有恢复正常字面字体".into());
                }
            }
            Ok(())
        });
        self.finish_pool(result, "attribute_pool_ended")
    }

    /// 只用实际storage的一个源unit和本次实际属性排版；不建立文档镜像或第二个TextKit图。
    fn shaped_single_unit(
        &self,
        index: usize,
        attributes: &MarkdownTextAttributes,
        font: &NSFont,
        expected_source: u16,
        expected_glyph: u16,
    ) -> Result<(u16, f64), String> {
        self.require_current()?;
        if index >= self.length {
            return Err("单unit字形排版范围超出原正文".into());
        }
        let result = autoreleasepool(|_| {
            let value = self.step(|| self.surface.backend.text_storage().string())?;
            let source = self.step(|| value.substringWithRange(NSRange::new(index, 1)))?;
            if self.step(|| source.length())? != 1
                || self.step(|| source.characterAtIndex(0))? != expected_source
            {
                return Err("单unit排版未读取到预声明的原列表源码".into());
            }
            let allocated = self.step(NSAttributedString::alloc)?;
            let attributed = self.step(|| {
                // SAFETY: 字典来自实际storage，font/glyphInfo等已逐项核型；源字符串精确为一个完整ASCII unit。
                unsafe {
                    NSAttributedString::initWithString_attributes(
                        allocated,
                        &source,
                        Some(attributes),
                    )
                }
            })?;
            let cf_attributed: &CFAttributedString = attributed.as_ref();
            let line = self.step(|| {
                // SAFETY: Foundation与CFAttributedString公开桥接；attributed保持不可变且在整个排版读取期存活。
                unsafe { CTLine::with_attributed_string(cf_attributed) }
            })?;
            let count = self.step(|| {
                // SAFETY: line是本栈持有的有效不可变CTLine，无并发修改。
                unsafe { line.glyph_count() }
            })?;
            let source_range = self.step(|| {
                // SAFETY: 同一不可变line仍存活，仅读其实际源关联范围。
                unsafe { line.string_range() }
            })?;
            let runs = self.step(|| {
                // SAFETY: 返回数组由本栈持有，元素类型按CTLine公开契约为CTRun。
                unsafe { line.glyph_runs() }
            })?;
            if count != 1 || source_range != CFRange::new(0, 1) || self.step(|| runs.count())? != 1
            {
                return Err("原单unit没有排版为精确一个源关联glyph/run".into());
            }
            let run = self.step(|| {
                // SAFETY: 上一原票步骤确认数组恰有一个CTRun，索引0有效；runs保持它的生命周期。
                unsafe { runs.value_at_index(0) }
            })?;
            let run = NonNull::new(run.cast_mut().cast::<CTRun>()).ok_or("单unit排版返回空run")?;
            // SAFETY: 指针来自CTLine公开CTRun数组；数组及line在本函数读取结束前持续持有该不可变对象。
            let run = unsafe { run.as_ref() };
            let run_count = self.step(|| {
                // SAFETY: run由原line/runs保持有效，不跨原权限进行排版或修改。
                unsafe { run.glyph_count() }
            })?;
            let run_range = self.step(|| {
                // SAFETY: 同一有效不可变run的源范围读取。
                unsafe { run.string_range() }
            })?;
            if run_count != 1 || run_range != CFRange::new(0, 1) {
                return Err("单unit实际run的glyph数或源范围不符".into());
            }
            let run_attributes = self.step(|| {
                // SAFETY: run由原不可变line保持，返回的属性字典在本栈独立持有。
                unsafe { run.attributes() }
            })?;
            // SAFETY: CFDictionary与NSDictionary公开toll-free桥接；值来自Core Text实际run属性，未制造字典或更换字体。
            let run_attributes: &MarkdownTextAttributes =
                unsafe { &*std::ptr::from_ref(&*run_attributes).cast::<MarkdownTextAttributes>() };
            // SAFETY: AppKit导出的字体key进程期不可变；返回值仍逐项下转核型。
            let font_key = unsafe { NSFontAttributeName };
            let run_font = self
                .step(|| run_attributes.objectForKey(font_key))?
                .ok_or("实际单unit run没有字体属性")?;
            let run_font = self
                .step(|| run_font.downcast::<NSFont>())?
                .map_err(|_| "实际单unit run字体类型不符")?;
            let run_name = self.step(|| run_font.fontName())?;
            let original_name = self.step(|| font.fontName())?;
            let same_name = self.step(|| run_name.isEqualToString(&original_name))?;
            if !same_name
                || (self.step(|| run_font.pointSize())? - self.step(|| font.pointSize())?).abs()
                    > 0.001
            {
                return Err("实际单unit排版发生了字体face或字号替换".into());
            }
            let mut shaped = 0_u16;
            let mut source_index = -1;
            let mut advance = CGSize::new(0.0, 0.0);
            self.step(|| {
                // SAFETY: run恰有一个glyph，范围0..1和三个单元素可写输出均经边界核对。
                unsafe { run.glyphs(CFRange::new(0, 1), NonNull::from(&mut shaped)) };
            })?;
            self.step(|| {
                // SAFETY: 仅复制同一个glyph的原文索引，单元素buffer完整可写。
                unsafe { run.string_indices(CFRange::new(0, 1), NonNull::from(&mut source_index)) };
            })?;
            self.step(|| {
                // SAFETY: 仅复制同一个glyph的advance，单元素CGSize输出完整可写。
                unsafe { run.advances(CFRange::new(0, 1), NonNull::from(&mut advance)) };
            })?;
            let line_width = self.step(|| {
                // SAFETY: 三个可选度量输出均传null；只读取原有效不可变line的总宽度。
                unsafe {
                    line.typographic_bounds(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                }
            })?;
            let mut glyph = expected_glyph;
            let mut expected_advance = CGSize::new(0.0, 0.0);
            let core_font: &CTFont = font.as_ref();
            let width = self.step(|| {
                // SAFETY: 原实际字体已验证目标glyph非0；单个glyph及单个CGSize输出均在本栈有效。
                unsafe {
                    core_font.advances_for_glyphs(
                        CTFontOrientation::Horizontal,
                        NonNull::from(&mut glyph),
                        &mut expected_advance,
                        1,
                    )
                }
            })?;
            if shaped != expected_glyph
                || source_index != 0
                || !advance.width.is_finite()
                || !advance.height.is_finite()
                || !expected_advance.width.is_finite()
                || !width.is_finite()
                || !line_width.is_finite()
                || width <= 0.0
                || advance.width <= 0.0
                || (advance.width - expected_advance.width).abs() > 0.01
                || (advance.height - expected_advance.height).abs() > 0.01
                || (line_width - width).abs() > 0.01
            {
                return Err(format!(
                    "实际单unit排版字形/源索引/宽度不符：expected={expected_glyph} shaped={shaped} source_index={source_index} advance={} expected_advance={} line_width={line_width} width={width}",
                    advance.width, expected_advance.width
                ));
            }
            Ok((shaped, advance.width))
        });
        // 原CF/Native claims与局部pool全部结束后才发布标量排版收据，同时保留最初失败阶段。
        self.finish_pool(result, "shape_pool_ended")
    }

    fn source_units(&self, start: usize, expected: &str) -> Result<(), String> {
        let count = expected.encode_utf16().count();
        if count > 256 || start.checked_add(count).is_none_or(|end| end > self.length) {
            return Err("列表标记只允许读取固定栈预算内的原夹具".into());
        }
        let result = autoreleasepool(|_| {
            let string = self.step(|| self.surface.backend.text_storage().string())?;
            let mut units = [0_u16; 256];
            self.step(|| {
                // SAFETY: range已核在原正文内，固定栈buffer至少有count个可写unit。
                unsafe {
                    string.getCharacters_range(
                        NonNull::new(units.as_mut_ptr()).unwrap(),
                        NSRange::new(start, count),
                    )
                };
            })?;
            if !units[..count].iter().copied().eq(expected.encode_utf16()) {
                return Err("原虚拟NSString的列表源码已改变".into());
            }
            Ok(())
        });
        self.require_current()?;
        result
    }

    fn exact_selection(&self, expected: MarkdownTextSelection) -> Result<(), String> {
        let actual = self
            .step(|| self.surface.backend.text_view.primary_text_selection())?
            .map_err(|_| "列表标记无法读取实际完整选区")?;
        if actual.selection != expected || self.surface.document().borrow().selection() != expected
        {
            return Err("列表标记内部插入点或正反方向被原生字形cluster吸附".into());
        }
        self.require_current()
    }

    fn find_internal_x(&self, before: usize, inside_x: usize) -> Result<(), String> {
        self.select(MarkdownTextSelection::caret(before))?;
        let expected = MarkdownTextSelection {
            anchor_utf16: inside_x,
            head_utf16: inside_x + 1,
        };
        let after_fold = self.prepare_selection_transition(expected)?;
        self.phase.set("find_actual_next"); // 真Find的完整选择与唯一fold后继均在首查询getter前声明。
        let result = autoreleasepool(|_| {
            let request = begin_list_marker_find_probe(
                self.surface,
                self.controller,
                self.epochs[0],
                inside_x..inside_x + 1,
                &|| self.current(),
            )?;
            self.settle_selection_transition(expected, after_fold)?;
            self.exact_selection(expected)?;
            // 只运行生产已调度的有限executor一次；仅原Rc的成功终点可证明真实可见定位完成。
            self.step(|| {
                self.surface
                    .backend
                    .text_view
                    .run_scheduled_table_layout_update()
            })?;
            if request.probe_completed() != Some(true) {
                return Err("任务框内部x的原Find定位请求未实际完成".into());
            }
            for index in inside_x - 1..=inside_x + 1 {
                self.attribute(index, MarkdownListGlyphRole::Literal)?;
            }
            self.exact_selection(expected)?;
            drop(request);
            finish_list_marker_find_probe(self.surface, self.controller, &|| self.current())
        });
        self.require_current()?;
        result
    }
}

/// 原Native9权限内运行；失败仍只按已声明的模式/选区转换收尾，不采纳后继输入签新票。
pub(super) fn verify(
    surface: &NativeMarkdownEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    fixture: &str,
) -> Result<(), String> {
    let sample = "列表任务定位起点。\n\n- [x] 已完成任务\n- [ ] 待完成任务\n\n- 普通短横标记\n+ 普通加号标记\n* 普通星号标记\n";
    let start =
        markdown_probe_utf16_offset(fixture, fixture.find(sample).ok_or("列表标记静态夹具缺失")?)?;
    let at = |needle: &str| -> Result<usize, String> {
        markdown_probe_utf16_offset(
            fixture,
            fixture.find(needle).ok_or("列表标记夹具token缺失")?,
        )
    };
    let checked = at("- [x] 已完成任务")?;
    let unchecked = at("- [ ] 待完成任务")?;
    let bullets = [
        (at("- 普通短横标记")?, MarkdownListGlyphRole::Dash),
        (at("+ 普通加号标记")?, MarkdownListGlyphRole::Plus),
        (at("* 普通星号标记")?, MarkdownListGlyphRole::Star),
    ];
    let (generation, revision, length, original_selection) = {
        let document = surface.document().borrow();
        (
            document.generation(),
            document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?,
            document.utf16_len(),
            document.selection(),
        )
    };
    let original_mode = surface.backend.text_storage.presentation_mode();
    let permission = Permission {
        surface,
        controller,
        document: Rc::downgrade(surface.document()),
        generation,
        revision,
        length,
        epochs: [
            controller.find_input_intent(),
            controller.workspace_query_intent(),
            controller.ivars().header_layout_revision.get(),
            surface
                .backend
                .text_view
                .ivars()
                .table_input_intent_epoch
                .get(),
        ],
        fold_revision: Cell::new(surface.backend.text_view.ivars().folds.revision()),
        next_fold_revision: Cell::new(None),
        mode: Cell::new(original_mode),
        next_mode: Cell::new(None),
        selection: Cell::new(original_selection),
        next_selection: Cell::new(None),
        phase: Cell::new("initial"),
        step_sequence: Cell::new(0),
        attribute_index: Cell::new(0),
        attribute_role: Cell::new(MarkdownListGlyphRole::Literal),
    };
    let _payload = PayloadUseGuard::enter(&surface.backend.text_view.ivars().payload_use_depth);
    let result = (|| {
        permission.require_current()?;
        permission.graph()?;
        permission.select(MarkdownTextSelection::caret(0))?;
        let preparation = checked..unchecked + "- [ ] 待完成任务".encode_utf16().count();
        if preparation.len() > 64 || preparation.end > length {
            return Err("列表标记Native准备超过固定64unit预算".into());
        }
        let prepared = autoreleasepool(|_| {
            surface
                .backend
                .text_view
                .ensure_stable_table_source_range(&preparation, &|| permission.current())
                .map_err(|_| "列表标记有限Task布局准备失败".to_owned())
        });
        permission.require_current()?; // 全部临时Native图claim/pool结束后，才继续消费同一票。
        prepared?;
        for (bullet, role) in bullets {
            // 三种真实源bullet各自有单unit baseString；不能把所有源符号的字典共用为'-'。
            permission.attribute(bullet, role)?;
        }
        for (bullet, role) in [
            (checked, MarkdownListGlyphRole::Checked),
            (unchecked, MarkdownListGlyphRole::Unchecked),
        ] {
            permission.attribute(bullet, MarkdownListGlyphRole::Hidden)?;
            permission.attribute(bullet + 2, role)?;
            permission.attribute(bullet + 3, MarkdownListGlyphRole::Hidden)?;
            permission.attribute(bullet + 4, MarkdownListGlyphRole::Hidden)?;
            // 两端各取0..=3，覆盖全部四个caret与全部正/反/空选区；每次都读实际Native方向。
            for anchor in 0..=3 {
                for head in 0..=3 {
                    let selection = MarkdownTextSelection {
                        anchor_utf16: bullet + 2 + anchor,
                        head_utf16: bullet + 2 + head,
                    };
                    permission.select(selection)?;
                    permission.exact_selection(selection)?;
                    for index in bullet..=bullet + 4 {
                        permission.attribute(index, MarkdownListGlyphRole::Literal)?;
                    }
                }
            }
            permission.select(MarkdownTextSelection::caret(0))?;
            permission.attribute(bullet + 2, role)?; // 离开后须重新出现正确任务状态，不能沿活动字典误命中。
        }
        permission.find_internal_x(checked - 1, checked + 3)?;
        permission.change_mode(MarkdownPresentationMode::Source)?;
        permission.graph()?;
        for index in start..start + sample.encode_utf16().count() {
            permission.attribute(index, MarkdownListGlyphRole::Literal)?; // Source完整有限样本的每unit都无glyph/隐藏字体。
        }
        permission.source_units(start, sample)
    })();
    let restored = (|| {
        permission.phase.set("restore_prepare");
        permission.require_current()?;
        // 失败可能发生在合法模式setter已完成后；只能接纳此前明确允许的after，不从实际新输入续权。
        if let Some(next) = permission.next_mode.get()
            && surface.backend.text_storage.presentation_mode() == next
        {
            permission.mode.set(next);
            permission.next_mode.set(None);
        }
        if let Some(next) = permission.next_fold_revision.get()
            && surface.backend.text_view.ivars().folds.revision() == next
        {
            permission.fold_revision.set(next);
            permission.next_fold_revision.set(None);
        }
        if let Some(next) = permission.next_selection.get()
            && surface.document().borrow().selection() == next
        {
            permission.selection.set(next);
            permission.next_selection.set(None);
        }
        permission.select(original_selection)?;
        permission.change_mode(original_mode)?;
        permission.graph()?;
        permission.source_units(start, sample)?;
        permission.attribute(checked + 2, MarkdownListGlyphRole::Checked)?;
        permission.attribute(unchecked + 2, MarkdownListGlyphRole::Unchecked)?;
        Ok(())
    })();
    drop(_payload); // 原测试载荷guard不跨最终收据；析构之后仍要求同一owner/root与输入票。
    let restored = restored.and_then(|()| permission.require_current());
    match (result, restored) {
        (Err(first), Err(cleanup)) => Err(format!("{first}；恢复失败：{cleanup}")),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Ok(()), Ok(())) => {
            println!(
                "native_list_markers glyph_single_unit=true literal_source=true caret_exact=true directional_selection=true find_internal_x=true source=true restored=true graph=true generation_unchanged=true"
            );
            Ok(())
        }
    }
}
