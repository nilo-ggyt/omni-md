//! 同一真实TextKit图上的下标属性回归；只读有限属性与5个源unit，不建立替代正文或编辑器。

use super::*;

/// 纯测试权限只借原surface/controller；root只有Weak，允许的模式/选区转换须在首Native前声明。
struct Permission<'a> {
    surface: &'a NativeMarkdownEditorSurface,
    controller: &'a Retained<NativeMarkdownController>,
    document: WeakMarkdownDocument,
    generation: u64,
    revision: omni_markdown_core::MarkdownDocumentRevision,
    length: usize,
    epochs: [u64; 4],
    mode: Cell<MarkdownPresentationMode>,
    next_mode: Cell<Option<MarkdownPresentationMode>>,
    selection: Cell<MarkdownTextSelection>,
    next_selection: Cell<Option<MarkdownTextSelection>>,
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
        let mode = self.surface.backend.text_storage.presentation_mode();
        let document = self.surface.document().borrow();
        document.generation() == self.generation
            && document
                .current_revision()
                .is_ok_and(|actual| actual == self.revision)
            && document.utf16_len() == self.length
            && (mode == self.mode.get() || self.next_mode.get() == Some(mode))
            && (document.selection() == self.selection.get()
                || self.next_selection.get() == Some(document.selection()))
    }

    fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        native_markdown_table_intent_step(&|| self.current(), work)
            .map_err(|_| "下标属性回归的原窗口、正文或输入权限已变更".into())
    }

    fn require_current(&self) -> Result<(), String> {
        self.current()
            .then_some(())
            .ok_or_else(|| "下标属性回归已撤权".into())
    }

    fn select(&self, selection: MarkdownTextSelection) -> Result<(), String> {
        self.require_current()?;
        self.next_selection.set(Some(selection)); // 只允许本次预先声明的真实方向，不接受任意后继选择。
        self.step(|| {
            self.surface
                .backend
                .text_view
                .set_semantic_native_selection(selection)
        })?;
        if self.surface.document().borrow().selection() != selection {
            return Err("下标回归的生产选择setter未同步完整选区".into());
        }
        self.selection.set(selection);
        self.next_selection.set(None);
        // 原生方向读取也走生产TK2 helper，不能只凭升序selectedRange声称恢复了anchor/head。
        let captured = self
            .step(|| self.surface.backend.text_view.primary_text_selection())?
            .map_err(|_| "下标回归无法读取真实主选区")?;
        if captured.selection != selection {
            return Err("下标回归的原生主选区方向不符".into());
        }
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
        self.next_mode.set(Some(mode)); // 原request先捕获，随后只允许它声明的before→after。
        self.step(|| {
            self.surface
                .backend
                .set_presentation_mode(mode, self.controller)
        })??;
        if self.surface.backend.text_storage.presentation_mode() != mode {
            return Err("下标回归未实际切换展示模式".into());
        }
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
            return Err("下标模式往返未取得原真实定位完成收据".into());
        }
        drop(receipt); // 收据仅Weak/标量；局部context已在工厂返回前析构，结束后仍核原权限。
        self.require_current()
    }

    /// 所有Native返回值在局部pool中结束，只把实际字体/基线/删除线标量带出。
    fn metrics(&self, index: usize) -> Result<(f64, f64, bool), String> {
        let result = autoreleasepool(|_| {
            if index >= self.length {
                return Err("下标属性下标超出原正文".into());
            }
            let mut range = NSRange::new(0, 0);
            let attributes = self.step(|| {
                // SAFETY: index由原静态夹具定位且在原g0长度内；range是本栈可写的有效NSRange。
                unsafe {
                    NSAttributedString::attributesAtIndex_effectiveRange(
                        self.surface.backend.text_storage(),
                        index,
                        &mut range,
                    )
                }
            })?;
            if range.length == 0
                || range.length > NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
                || range.location > index
                || range
                    .location
                    .checked_add(range.length)
                    .is_none_or(|end| end <= index || end > self.length)
            {
                return Err("下标实际属性范围不是覆盖该unit的有限原文范围".into());
            }
            // SAFETY: 三个公开AppKit属性键是进程期不可变NSString，不建立新的字体或属性字典。
            let (font_key, baseline_key, strike_key) = unsafe {
                (
                    NSFontAttributeName,
                    NSBaselineOffsetAttributeName,
                    NSStrikethroughStyleAttributeName,
                )
            };
            let font = self
                .step(|| attributes.objectForKey(font_key))?
                .ok_or("下标属性缺少字体")?;
            let font = self
                .step(|| font.downcast::<NSFont>())?
                .map_err(|_| "下标字体类型不符")?;
            let size = self.step(|| font.pointSize())?;
            let baseline = match self.step(|| attributes.objectForKey(baseline_key))? {
                Some(value) => {
                    let number = self
                        .step(|| value.downcast::<NSNumber>())?
                        .map_err(|_| "下标基线类型不符")?;
                    self.step(|| number.doubleValue())?
                }
                None => 0.0, // NSBaselineOffset缺省为普通基线，不将缺失下标属性当成-3pt。
            };
            let strike = self.step(|| attributes.objectForKey(strike_key))?.is_some();
            if !size.is_finite() || size <= 0.0 || !baseline.is_finite() {
                return Err("下标实际字体或基线无效".into());
            }
            Ok((size, baseline, strike))
        });
        self.require_current()?; // 字体/NSNumber/字典和pool全部结束后，才消费本次标量。
        result
    }

    fn graph(&self) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let manager = self
                .step(|| self.surface.backend.text_view.textLayoutManager())?
                .ok_or("下标回归丢失TK2 manager")?;
            let content = self
                .step(|| manager.textContentManager())?
                .ok_or("下标回归丢失content manager")?;
            let observer = ProtocolObject::<dyn NSTextStorageObserving>::from_ref(
                &*self.surface.backend.text_content_storage,
            );
            let storage = self
                .step(|| observer.textStorage())?
                .ok_or("下标回归丢失piece storage")?;
            if !std::ptr::eq(&*manager, &*self.surface.backend.text_layout_manager)
                || !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                    &*content,
                    self.surface.backend.text_content_storage.as_super(),
                )
                || !std::ptr::eq::<NSTextStorage>(&*storage, self.surface.backend.text_storage())
            {
                return Err("下标模式往返更换了真实TextKit图".into());
            }
            Ok(())
        });
        self.require_current()?; // 真实图claim不跨下一次模式切换或关闭。
        result
    }

    fn source_units(&self, start: usize) -> Result<(), String> {
        let result = autoreleasepool(|_| {
            let value = self.step(|| self.surface.backend.text_storage().string())?;
            let mut units = [0u16; 5];
            self.step(|| {
                // SAFETY: 原夹具的H~2~O恰为5个ASCII unit；首Native前已核start+5在原长度内。
                unsafe {
                    value.getCharacters_range(
                        NonNull::new(units.as_mut_ptr()).unwrap(),
                        NSRange::new(start, 5),
                    );
                }
            })?;
            if units
                != [
                    u16::from(b'H'),
                    u16::from(b'~'),
                    u16::from(b'2'),
                    u16::from(b'~'),
                    u16::from(b'O'),
                ]
            {
                return Err("真实虚拟NSString没有保留H~2~O的源unit".into());
            }
            Ok(())
        });
        self.require_current()?;
        result
    }
}

/// 由现有ProbeEditingOwnerLease内调用；失败也尝试按原许可恢复模式/完整选择，保留首因。
pub(super) fn verify(
    surface: &NativeMarkdownEditorSurface,
    controller: &Retained<NativeMarkdownController>,
    fixture: &str,
) -> Result<(), String> {
    let start =
        markdown_probe_utf16_offset(fixture, fixture.find("H~2~O").ok_or("下标静态夹具缺失")?)?;
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
    if start.checked_add(5).is_none_or(|end| end > length) {
        return Err("下标静态夹具范围非法".into());
    }
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
        mode: Cell::new(original_mode),
        next_mode: Cell::new(None),
        selection: Cell::new(original_selection),
        next_selection: Cell::new(None),
    };
    let _payload = PayloadUseGuard::enter(&surface.backend.text_view.ivars().payload_use_depth);
    let result = (|| {
        permission.graph()?;
        permission.select(MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 0,
        })?;
        let normal = permission.metrics(start)?;
        let subscript = permission.metrics(start + 2)?;
        if (subscript.0 - normal.0 * 0.75).abs() > 0.001
            || (subscript.1 + 3.0).abs() > 0.001
            || subscript.2
        {
            return Err("非活动Preview下标的实际字体/基线/删除线属性不符".into());
        }
        permission.select(MarkdownTextSelection {
            anchor_utf16: start + 2,
            head_utf16: start + 2,
        })?;
        for marker in [start + 1, start + 3] {
            let actual = permission.metrics(marker)?;
            if (actual.0 - normal.0).abs() > 0.001 || actual.1 != 0.0 || actual.2 {
                return Err("活动单~没有恢复实际普通字体与基线".into());
            }
        }
        permission.change_mode(MarkdownPresentationMode::Source)?;
        permission.graph()?;
        let normal = permission.metrics(start)?;
        for index in start..start + 5 {
            let actual = permission.metrics(index)?;
            if (actual.0 - normal.0).abs() > 0.001 || actual.1 != 0.0 || actual.2 {
                return Err("Source下标源字符没有保留等大字体与普通基线".into());
            }
        }
        permission.source_units(start)
    })();
    // 只接受此前声明的模式/选区状态；不从新root/输入epoch重新领权。必要恢复不依赖属性断言成功。
    let restored = (|| {
        permission.require_current()?;
        permission
            .mode
            .set(surface.backend.text_storage.presentation_mode());
        permission.next_mode.set(None);
        permission
            .selection
            .set(surface.document().borrow().selection());
        permission.next_selection.set(None);
        permission.select(original_selection)?;
        permission.change_mode(original_mode)?;
        permission.graph()?;
        permission.source_units(start)?;
        let normal = permission.metrics(start)?;
        let subscript = permission.metrics(start + 2)?;
        if original_mode != MarkdownPresentationMode::InlinePreview
            || (subscript.0 - normal.0 * 0.75).abs() > 0.001
            || (subscript.1 + 3.0).abs() > 0.001
            || subscript.2
        {
            return Err("恢复原Preview后没有恢复实际下标属性".into());
        }
        Ok(())
    })();
    match (result, restored) {
        (Err(first), Err(cleanup)) => Err(format!("{first}；恢复失败：{cleanup}")),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Ok(()), Ok(())) => {
            println!(
                "native_subscript_attributes preview=true active_marker=true source=true restored=true graph=true generation_unchanged=true"
            );
            Ok(())
        }
    }
}
