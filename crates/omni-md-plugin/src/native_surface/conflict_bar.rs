//! 原正文顶部的冲突处理条；只拥有固定控件和已显示笔记的弱票，保存规则仍归原Core。

use super::*;
use objc2_app_kit::NSAttributedStringNSStringDrawing;

const MIN_HEIGHT: f64 = 38.0;
const PADDING: f64 = 12.0;
const GAP: f64 = 8.0;
const BUTTON_HEIGHT: f64 = 28.0;

/// controller是唯一强owner；原票只含Doc Weak、路径与窗口身份，发布代次只撤销旧显示动作。
pub(super) struct NativeMarkdownConflictBar {
    root: Retained<NSBox>,
    message: Retained<NSTextField>,
    buttons: [Retained<NSButton>; 2],
    original: RefCell<Option<NativeMarkdownCurrentNoteIntent>>,
    publication: Cell<u64>,
    published_message: Cell<Option<&'static str>>,
}

impl NativeMarkdownConflictBar {
    #[cfg(all(
        feature = "native-markdown-runtime-probe",
        feature = "native-table-row-append"
    ))]
    /// 原生验收只借已装配控件；不建立另一份条、保存状态或点击入口。
    pub(super) fn probe_controls(&self) -> (&NSBox, &NSTextField, &[Retained<NSButton>; 2]) {
        (&self.root, &self.message, &self.buttons)
    }

    /// composition root一次装配真实控件；构造失败不发布部分条，也不建立替代初始化图。
    pub(super) fn new(controller: &NativeMarkdownController) -> Result<Self, String> {
        let mtm = controller.mtm();
        let ledger = &controller.ivars().owned;
        let root = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
        root.setBoxType(NSBoxType::Custom);
        root.setTitlePosition(NSTitlePosition::NoTitle);
        root.setBorderWidth(1.0);
        root.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.353, 0.278, 0.188, 1.0,
        ));
        root.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.176, 0.157, 0.122, 1.0,
        ));
        root.setHidden(true);
        root.setAccessibilityLabel(Some(ns_string!("笔记冲突处理")));
        ledger.include("conflict.root", &root)?;
        let message = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        message.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        message.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.898, 0.757, 0.549, 1.0,
        )));
        message
            .cell()
            .expect("原只读字段已有cell")
            .setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByCharWrapping);
        message.setAccessibilityLabel(Some(ns_string!("当前笔记保存冲突")));
        ledger.include_control("conflict.message", &message)?;
        root.addSubview(&message);
        let mut buttons = Vec::with_capacity(2);
        for (title, tag) in [("重新加载磁盘版本", 4), ("将本地草稿另存为新笔记", 5)]
        {
            // SAFETY: 原主线程controller实现固定action；会话结束先清target/action再归还owner。
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(controller),
                    Some(sel!(resolveMarkdownConflict:)),
                    mtm,
                )
            };
            button.setTag(tag);
            button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            button.setRefusesFirstResponder(true);
            button.setFocusRingType(NSFocusRingType::None);
            button.setToolTip(Some(&NSString::from_str(title)));
            button.setAccessibilityLabel(Some(&NSString::from_str(title)));
            ledger.include_control("conflict.button", &button)?;
            root.addSubview(&button);
            buttons.push(button);
        }
        Ok(Self {
            root,
            message,
            buttons: buttons.try_into().expect("原冲突条有两个具体动作"),
            original: RefCell::new(None),
            publication: Cell::new(0),
            published_message: Cell::new(None),
        })
    }

    /// 只在实际安装/清理笔记时换弱票；同名A→B→A也因新Doc弱身份和发布代次失效。
    fn bind(&self, original: Option<NativeMarkdownCurrentNoteIntent>) {
        self.advance_publication();
        *self.original.borrow_mut() = original;
    }
    /// 单一显示代次不回绕；重复相同保存失败不构成新的显示决定，也不会阻断原确认框。
    fn advance_publication(&self) {
        self.publication.set(
            self.publication
                .get()
                .checked_add(1)
                .expect("冲突处理发布代次耗尽"),
        );
    }

    /// 消息来源仅为原footer结果；这里不猜测磁盘版本，也不添加一份保存状态机。
    pub(super) fn publish(&self, message: Option<&'static str>, current: &impl Fn() -> bool) {
        if self.published_message.replace(message) != message {
            self.advance_publication();
        }
        let revision = self.publication.get();
        let current = || current() && self.publication.get() == revision;
        if !current() {
            return;
        }
        self.root.setHidden(message.is_none());
        if !current() {
            return;
        }
        self.message
            .setStringValue(&NSString::from_str(message.unwrap_or_default()));
        if !current() {
            return;
        }
        self.message
            .setToolTip(message.map(NSString::from_str).as_deref());
        for button in &self.buttons {
            if !current() {
                return;
            }
            button.setEnabled(message.is_some());
        }
    }

    /// 真实按钮字体测宽后给原弹性文本留剩余宽；长提示提高行高，不能挤掉原两个全称。
    fn dimensions(
        &self,
        width: f64,
        current: &impl Fn() -> bool,
    ) -> Result<(f64, [f64; 2], f64), String> {
        let mut widths = [0.0; 2];
        for (index, button) in self.buttons.iter().enumerate() {
            if !current() {
                return Err("冲突处理显示已经变化".into());
            }
            let title = button.attributedTitle();
            if !current() {
                return Err("冲突处理显示已经变化".into());
            }
            widths[index] = (title.size().width + 16.0).ceil();
            drop(title); // 固定标题的临时Native claim不跨后续控件布局或状态借用。
        }
        if !current() {
            return Err("冲突处理显示已经变化".into());
        }
        let text_width = (width - 2.0 * PADDING - 2.0 * GAP - widths.iter().sum::<f64>()).max(1.0);
        let text_height = self
            .message
            .sizeThatFits(NSSize::new(text_width, f64::MAX))
            .height
            .ceil();
        if !current() {
            return Err("冲突处理显示已经变化".into());
        }
        Ok(((text_height + 12.0).max(MIN_HEIGHT), widths, text_width))
    }

    /// 所有Find显隐与普通resize都从这一真实高度扣正文，不在旧frame上相对叠减。
    pub(super) fn reserve_area(&self, mut base: NSRect) -> Result<NSRect, String> {
        if self.published_message.get().is_none() {
            return Ok(base);
        }
        let revision = self.publication.get();
        let (height, _, _) = self.dimensions(base.size.width, &|| {
            self.publication.get() == revision && self.published_message.get().is_some()
        })?;
        if self.publication.get() != revision {
            return Err("冲突处理显示已经变化".into());
        }
        base.size.height -= height;
        Ok(base)
    }

    /// 原错误行在最上方；冲突条随后、Find与正文再后，保持原页面垂直顺序。
    pub(super) fn position(
        &self,
        parent: &NSView,
        full: NSRect,
        reserved: NSRect,
        content_top: f64,
        current: &impl Fn() -> bool,
    ) {
        if self.published_message.get().is_none() || !current() {
            return;
        }
        let revision = self.publication.get();
        let current = || current() && self.publication.get() == revision;
        let height = full.size.height - reserved.size.height;
        let Ok((_, widths, text_width)) = self.dimensions(full.size.width, &current) else {
            return;
        };
        if !current() {
            return;
        }
        // content_top直接来自同一editor_area_frames的Find或正文顶部；不复制错误行高度常量。
        self.root.setFrame(NSRect::new(
            NSPoint::new(full.origin.x, content_top),
            NSSize::new(full.size.width, height),
        ));
        if !current() {
            return;
        }
        self.message.setFrame(NSRect::new(
            NSPoint::new(PADDING, 6.0),
            NSSize::new(text_width, height - 12.0),
        ));
        let mut x = PADDING + text_width + GAP;
        for (button, width) in self.buttons.iter().zip(widths) {
            if !current() {
                return;
            }
            button.setFrame(NSRect::new(
                NSPoint::new(x, (height - BUTTON_HEIGHT) / 2.0),
                NSSize::new(width, BUTTON_HEIGHT),
            ));
            x += width + GAP;
        }
        if !current() {
            return;
        }
        // SAFETY: 只在原主线程核本控件父关系；不递归探查系统视图树或延长另一owner。
        if unsafe { self.root.superview() }.is_none() && current() {
            parent.addSubview(&self.root);
        }
    }

    /// 清理不依赖原票current，必须撤掉旧显示数据；会话结束另外对称断开两个target。
    pub(super) fn clear(&self) {
        self.bind(None);
        self.published_message.set(None);
        let revision = self.publication.get();
        self.root.setHidden(true);
        if self.publication.get() != revision {
            return;
        }
        self.message.setStringValue(ns_string!(""));
        if self.publication.get() != revision {
            return;
        }
        self.message.setToolTip(None);
        for button in &self.buttons {
            if self.publication.get() != revision {
                return;
            }
            button.setEnabled(false);
        }
    }
}

/// 首个sender getter之前固定当前显示的原票；隐藏条或后继笔记不能借旧按钮领取新权限。
pub(super) fn enqueue_action(controller: &NativeMarkdownController, sender: &AnyObject) {
    let bar = controller
        .ivars()
        .conflict_bar
        .get()
        .expect("真实窗口已安装原冲突条");
    let original = bar.original.borrow().clone();
    let revision = bar.publication.get();
    let Some(original) = original.filter(NativeMarkdownCurrentNoteIntent::current) else {
        return;
    };
    if bar.published_message.get().is_none() {
        return;
    }
    let Some(button) = sender.downcast_ref::<NSButton>() else {
        return;
    };
    let Some(index) = bar
        .buttons
        .iter()
        .position(|owned| std::ptr::eq(&**owned, button))
    else {
        return;
    };
    let tag = button.tag();
    if tag != [4, 5][index]
        || !button.isEnabled()
        || !original.current()
        || bar.publication.get() != revision
    {
        return;
    }
    let original = MainThreadBound::new(original, controller.mtm());
    controller.enqueue_owned(move || {
        let mtm = MainThreadMarker::new().expect("冲突处理沿原主队列执行");
        if let Err(error) = original.get(mtm).perform_resolution(tag, revision) {
            if original.get(mtm).current() {
                present_native_markdown_error(&error);
            } else {
                // 外部副作用的原错误仍返回到此，但旧A的提示不能覆盖后来B的状态或另开窗口。
                eprintln!("原笔记的冲突处理未完成，所属窗口或文稿已经变化");
            }
        }
    });
}

impl NativeMarkdownController {
    /// 实际Window装配一次固定条并纳入原资源账本；后续只更换弱票和显示文字。
    pub(super) fn install_conflict_bar(&self) -> Result<(), String> {
        let bar = NativeMarkdownConflictBar::new(self)?;
        assert!(
            self.ivars().conflict_bar.set(bar).is_ok(),
            "原冲突条只能装配一次"
        );
        Ok(())
    }
    /// 原文稿安装点绑定显示身份，不从较晚的Active或侧栏路径补领权限。
    pub(super) fn bind_conflict_note(&self, original: NativeMarkdownCurrentNoteIntent) {
        self.ivars()
            .conflict_bar
            .get()
            .expect("原窗口已装配冲突条")
            .bind(Some(original));
    }
    /// 菜单与原两个按钮共享同一个实际显示代次，不新增另一套文件动作初始化图。
    pub(super) fn conflict_publication(&self) -> u64 {
        self.ivars()
            .conflict_bar
            .get()
            .expect("实际窗口已装配冲突条")
            .publication
            .get()
    }
    /// 构造失败没有发布控件时只清原状态；正常拆图须真实清空本条并撤弱票。
    pub(super) fn clear_conflict_bar(&self) {
        if let Some(bar) = self.ivars().conflict_bar.get() {
            bar.clear();
        }
    }
    /// 会话结束清借用target/action，不添加关闭功能或替代原返回生命周期。
    pub(super) fn detach_conflict_bar(&self) {
        if let Some(bar) = self.ivars().conflict_bar.get() {
            bar.clear();
            for button in &bar.buttons {
                // SAFETY: 原主线程controller仍活，归还控件owner前先解除标准action借用。
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
            bar.root.removeFromSuperview();
        }
    }
}
