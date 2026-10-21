//! 原正文内文档信息卡；只投影有限身份和树根聚合，不接管输入、文稿或窗口生命周期。

use super::*;
use objc2_app_kit::NSScrollerStyle;

const WIDTH: f64 = 270.0;
const PADDING: f64 = 13.0;
const LABEL_WIDTH: f64 = 48.0;
const GAP: f64 = 8.0;

/// 固定控件在首次明确打开后复用；隐藏时归还原票、清空业务文字并移出视图树。
/// 只有会话controller拥有此值；Weak票不延长Doc、scroll、编辑器或源映射生命周期。
pub(super) struct NativeMarkdownInformationPanel {
    root: Retained<NSBox>,
    scroll: Retained<NSScrollView>,
    content: Retained<NSView>,
    heading: Retained<NSTextField>,
    labels: [Retained<NSTextField>; 7],
    values: [Retained<NSTextField>; 7],
    close: Retained<NSButton>,
    refresh: Retained<NSButton>,
    original: RefCell<Option<NativeMarkdownCurrentNoteIntent>>,
    revision: Cell<u64>,
    visible: Cell<bool>,
    reset_scroll: Cell<bool>,
}

#[derive(Clone, Copy)]
struct Counts {
    characters: usize,
    words: usize,
    lines: usize,
    bytes: usize,
}
impl Counts {
    /// 四项都是canonical树聚合；调用方必须在Native展示前结束这份只读借用。
    fn capture(document: &MarkdownDocumentBuffer) -> Self {
        Self {
            characters: document.character_count(),
            words: document.word_count(),
            lines: document.line_count(),
            bytes: document.byte_len(),
        }
    }
}

impl NativeMarkdownInformationPanel {
    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 原生回归只借用这张卡已装配的实际控件；不创建观察目录、正文owner或替身。
    pub(super) fn probe_controls(
        &self,
    ) -> (
        &NSBox,
        &NSScrollView,
        &[Retained<NSTextField>; 7],
        &NSButton,
        &NSButton,
    ) {
        (
            &self.root,
            &self.scroll,
            &self.values,
            &self.close,
            &self.refresh,
        )
    }

    /// 一次装配所有固定角色；账本拒绝时candidate局部析构，不发布不完整资源图。
    fn new(controller: &NativeMarkdownController) -> Result<Self, String> {
        let mtm = controller.mtm();
        let ledger = &controller.ivars().owned;
        let root = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
        root.setBoxType(NSBoxType::Custom);
        root.setTitlePosition(NSTitlePosition::NoTitle);
        root.setCornerRadius(10.0);
        root.setBorderWidth(1.0);
        root.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.227, 0.259, 0.314, 1.0,
        ));
        root.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.125, 0.141, 0.176, 1.0,
        ));
        root.setAccessibilityLabel(Some(ns_string!("文档信息")));
        ledger.include("information.root", &root)?;
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::ZERO);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setScrollerStyle(NSScrollerStyle::Overlay);
        scroll.setAutohidesScrollers(true);
        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        scroll.setDocumentView(Some(&content));
        ledger.include("information.scroll", &scroll)?;
        ledger.include("information.clip", &scroll.contentView())?;
        ledger.include(
            "information.scroller",
            &scroll.verticalScroller().expect("已装配信息卡垂直滚动条"),
        )?;
        ledger.include("information.content", &content)?;
        root.addSubview(&scroll);
        let heading = Self::label("文档信息", true, controller)?;
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        content.addSubview(&heading);
        let labels = ["标题", "路径", "字符", "词数快照", "行数", "字节", "修改"]
            .map(|text| Self::label(text, false, controller));
        // 固定七项逐项传播真实登记错误；不因后项失败丢掉先前已登记的weak证据。
        let labels: [Retained<NSTextField>; 7] = labels
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .expect("信息卡有固定七个标签");
        // 词数只在打开/显式刷新时统计；四字标签保原48pt列，悬停解释不会新增正文读取或计时器。
        labels[3].setToolTip(Some(ns_string!("显示上次统计结果；编辑后点击刷新词数更新")));
        let values: [Retained<NSTextField>; 7] = (0..7)
            .map(|_| Self::label("", true, controller))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .expect("信息卡有固定七个值");
        for (label, value) in labels.iter().zip(&values) {
            // 只有标题/路径/数字/时间的短展示值，行高按实际文本测量；绝不设置正文内容。
            content.addSubview(label);
            content.addSubview(value);
        }
        let close = Self::button(
            "×",
            "关闭文档信息",
            sel!(closeMarkdownInformation:),
            controller,
        )?;
        let refresh = Self::button(
            "刷新词数",
            "刷新当前文档词数",
            sel!(refreshMarkdownInformationWords:),
            controller,
        )?;
        close.setBordered(false);
        close.setFont(Some(&NSFont::systemFontOfSize(18.0)));
        refresh.setFont(Some(&NSFont::systemFontOfSize(10.0)));
        content.addSubview(&close);
        content.addSubview(&refresh);
        Ok(Self {
            root,
            scroll,
            content,
            heading,
            labels,
            values,
            close,
            refresh,
            original: RefCell::new(None),
            revision: Cell::new(0),
            visible: Cell::new(false),
            reset_scroll: Cell::new(true),
        })
    }

    /// 标准只读多行字段只解释显示身份；不参与共享field editor、IME或Undo。
    fn label(
        text: &str,
        value: bool,
        controller: &NativeMarkdownController,
    ) -> Result<Retained<NSTextField>, String> {
        let label =
            NSTextField::wrappingLabelWithString(&NSString::from_str(text), controller.mtm());
        label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            if value { 0.788 } else { 0.494 },
            if value { 0.816 } else { 0.537 },
            if value { 0.863 } else { 0.612 },
            1.0,
        )));
        label.setSelectable(false);
        // 路径与长标题也需完整展示；按字符换行避免无空格路径越过卡片宽度。
        label
            .cell()
            .expect("原生只读字段已有cell")
            .setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByCharWrapping);
        controller
            .ivars()
            .owned
            .include_control("information.label", &label)?;
        Ok(label)
    }

    /// 两个原有卡片动作借用同一controller，关闭会话时对称断开target/action。
    fn button(
        title: &str,
        explanation: &str,
        action: Sel,
        controller: &NativeMarkdownController,
    ) -> Result<Retained<NSButton>, String> {
        // SAFETY: controller在当前主线程会话中存活，实现此固定action ABI；detach先清借用边。
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(controller),
                Some(action),
                controller.mtm(),
            )
        };
        button.setRefusesFirstResponder(true);
        button.setToolTip(Some(&NSString::from_str(explanation)));
        button.setAccessibilityLabel(Some(&NSString::from_str(explanation)));
        controller
            .ivars()
            .owned
            .include_control("information.button", &button)?;
        Ok(button)
    }

    /// 先撤销纯标量权限，再做可重入清理；反复开关只复用这一组空控件。
    fn hide(&self) {
        self.visible.set(false);
        self.revision.set(
            self.revision
                .get()
                .checked_add(1)
                .expect("信息卡显示代次耗尽"),
        );
        self.original.borrow_mut().take();
        let revision = self.revision.get();
        let clearing = || !self.visible.get() && self.revision.get() == revision;
        self.root.removeFromSuperview();
        for value in &self.values {
            if !clearing() {
                return; // 清理中的Native重入已重新打开，旧hide不能抹掉后继显示值。
            }
            value.setStringValue(ns_string!(""));
            if !clearing() {
                return;
            }
            value.setToolTip(None);
            if !clearing() {
                return;
            }
            value.setAccessibilityLabel(None);
        }
    }

    /// 只接受同一次显示；原生setter触发关闭或后继打开后，不再发布旧值。
    fn current(&self, revision: u64, controller: &NativeMarkdownController) -> bool {
        self.visible.get()
            && self.revision.get() == revision
            && native_markdown_controller_has_foreground_authority(controller)
    }

    /// caller已核对原Doc；本方法只展示四个聚合，词数遵循原卡片的明确刷新动作。
    fn render_counts(&self, counts: Counts, refresh_words: bool, current: &impl Fn() -> bool) {
        for (index, value) in [
            (2, counts.characters),
            (3, counts.words),
            (4, counts.lines),
            (5, counts.bytes),
        ] {
            if index == 3 && !refresh_words {
                continue; // 原页面词数在打开/刷新时采样，普通编辑不偷换该按钮的语义。
            }
            if !current() {
                return;
            }
            self.values[index].setStringValue(&NSString::from_str(&value.to_string()));
        }
    }

    /// 原浮层固定270pt并留正文上12/right14；窄高区域用卡片自身滚动，不压缩编辑区。
    fn position(&self, parent: &NSView, body: NSRect, current: &impl Fn() -> bool) {
        if !self.visible.get() || !current() {
            return;
        }
        let revision = self.revision.get();
        let current = || current() && self.visible.get() && self.revision.get() == revision;
        let clip = self.scroll.contentView();
        if !current() {
            return;
        }
        let old_bounds = clip.bounds();
        if !current() {
            return;
        }
        let old_height = self.content.frame().size.height;
        let from_top = if self.reset_scroll.get() {
            0.0
        } else {
            // 卡片内部滚动位置按距顶偏移保存；resize/字符统计刷新不把用户拉回标题。
            (old_height - old_bounds.origin.y - old_bounds.size.height).max(0.0)
        };
        let value_width = WIDTH - 2.0 * PADDING - LABEL_WIDTH - GAP;
        let mut heights = [16.0_f64; 7];
        for (index, value) in self.values.iter().enumerate() {
            if !current() {
                return;
            }
            heights[index] = value
                .sizeThatFits(NSSize::new(value_width, f64::MAX))
                .height
                .ceil()
                .max(16.0);
        }
        let natural_height =
            PADDING * 2.0 + 22.0 + 10.0 + heights.iter().sum::<f64>() + 6.0 * GAP + 11.0 + 28.0;
        let height = natural_height.min((body.size.height - 24.0).max(1.0));
        if !current() {
            return;
        }
        self.root.setFrame(NSRect::new(
            NSPoint::new(
                body.origin.x + body.size.width - WIDTH - 14.0,
                body.origin.y + body.size.height - height - 12.0,
            ),
            NSSize::new(WIDTH, height),
        ));
        if !current() {
            return;
        }
        self.scroll.setFrame(NSRect::new(
            NSPoint::new(1.0, 1.0),
            NSSize::new(WIDTH - 2.0, (height - 2.0).max(1.0)),
        ));
        if !current() {
            return;
        }
        self.content.setFrame(NSRect::new(
            NSPoint::ZERO,
            NSSize::new(WIDTH - 2.0, natural_height),
        ));
        let mut top = natural_height - PADDING;
        for (view, frame) in [
            (
                self.heading.as_super(),
                NSRect::new(
                    NSPoint::new(PADDING, top - 22.0),
                    NSSize::new(WIDTH - 2.0 * PADDING - 28.0, 22.0),
                ),
            ),
            (
                self.close.as_super(),
                NSRect::new(
                    NSPoint::new(WIDTH - PADDING - 25.0, top - 22.0),
                    NSSize::new(22.0, 22.0),
                ),
            ),
        ] {
            if !current() {
                return;
            }
            view.setFrame(frame);
        }
        top -= 32.0;
        for (index, height) in heights.into_iter().enumerate() {
            if !current() {
                return;
            }
            self.labels[index].setFrame(NSRect::new(
                NSPoint::new(PADDING, top - 16.0),
                NSSize::new(LABEL_WIDTH, 16.0),
            ));
            if !current() {
                return;
            }
            self.values[index].setFrame(NSRect::new(
                NSPoint::new(PADDING + LABEL_WIDTH + GAP, top - height),
                NSSize::new(value_width, height),
            ));
            top -= height + GAP;
        }
        if !current() {
            return;
        }
        self.refresh.setFrame(NSRect::new(
            NSPoint::new(PADDING, PADDING),
            NSSize::new(WIDTH - 2.0 * PADDING, 28.0),
        ));
        if !current() {
            return;
        }
        let bounds = clip.bounds();
        if !current() {
            return;
        }
        clip.scrollToPoint(NSPoint::new(
            0.0,
            (natural_height - bounds.size.height - from_top).max(0.0),
        ));
        if !current() {
            return;
        }
        self.scroll.reflectScrolledClipView(&clip);
        if !current() {
            return;
        }
        self.reset_scroll.set(false);
        // 本卡只在首次打开加入parent，resize不重新排序或把其他控件带到前台。
        // SAFETY: 只在主线程读取本卡既有父关系，不跨getter持有状态借用。
        if unsafe { self.root.superview() }.is_none() && current() {
            parent.addSubview(&self.root);
        }
    }
}

/// 元数据只在打开和成功保存时读取，时间展示用本机区域与时区；失败明确显示未获取。
fn modified_time(document: &MarkdownDocumentBuffer) -> Option<std::time::SystemTime> {
    document
        .path()
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
}
fn display_time(time: Option<std::time::SystemTime>) -> String {
    let Some(time) = time else {
        return "未获取".to_owned();
    };
    let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(), // 正向时间保留亚秒值，不额外复制文件或正文。
        Err(error) => -error.duration().as_secs_f64(), // Unix epoch以前同样是合法时间。
    };
    let date = objc2_foundation::NSDate::dateWithTimeIntervalSince1970(seconds);
    objc2_foundation::NSDateFormatter::localizedStringFromDate_dateStyle_timeStyle(
        &date,
        objc2_foundation::NSDateFormatterStyle::MediumStyle,
        objc2_foundation::NSDateFormatterStyle::MediumStyle,
    )
    .to_string()
}

/// 原菜单已在首getter之前捕获笔记票；信息完全只读，不经过通用IME/Cell提交前置流程。
pub(super) fn toggle(intent: &NativeMarkdownCurrentNoteIntent) -> Result<(), String> {
    if !intent.current() {
        return Ok(());
    }
    let controller = ACTIVE_NATIVE_MARKDOWN_SURFACE
        .with_borrow(|active| active.as_ref().map(|surface| surface.controller.clone()))
        .ok_or("笔记窗口已关闭")?;
    let existing = controller.ivars().information_panel.borrow().clone();
    if let Some(panel) = existing.as_ref().filter(|panel| panel.visible.get()) {
        panel.hide();
        return Ok(()); // 再点原“信息”按钮只收起卡片，编辑器焦点和草稿保持原状。
    }
    let panel = match existing {
        Some(panel) => panel,
        None => {
            let panel = Rc::new(NativeMarkdownInformationPanel::new(&controller)?);
            if !intent.current() {
                return Ok(());
            } // 构造重入失权时局部candidate自行释放。
            *controller.ivars().information_panel.borrow_mut() = Some(Rc::clone(&panel));
            panel
        }
    };
    let Some(document) = intent.document.upgrade() else {
        return Ok(());
    };
    let (counts, modified) = {
        let document = document.borrow();
        (Counts::capture(&document), modified_time(&document))
    };
    drop(document); // 原生日期/字段setter之前结束Doc借用和临时强引用。
    if !intent.current() {
        return Ok(());
    }
    *panel.original.borrow_mut() = Some(intent.clone());
    panel.revision.set(
        panel
            .revision
            .get()
            .checked_add(1)
            .expect("信息卡显示代次耗尽"),
    );
    panel.reset_scroll.set(true);
    panel.visible.set(true);
    let revision = panel.revision.get();
    let current = || panel.current(revision, &controller) && intent.current();
    let title = Path::new(&intent.relative_path)
        .file_stem()
        .and_then(|title| title.to_str())
        .expect("原Core已验证笔记UTF-8路径");
    for (index, text) in [
        (0, title.to_owned()),
        (1, intent.relative_path.clone()),
        (6, display_time(modified)),
    ] {
        if !current() {
            return Ok(());
        }
        let text = NSString::from_str(&text);
        panel.values[index].setStringValue(&text);
        if !current() {
            return Ok(());
        }
        panel.values[index].setToolTip(Some(&text));
    }
    panel.render_counts(counts, true, &current);
    if !current() {
        return Ok(());
    }
    controller.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
    Ok(())
}

impl NativeMarkdownController {
    /// accepted文稿交接和关闭都同步撤原票、文本和父关系，不等待队列或下一次打开。
    pub(super) fn hide_information_panel(&self) {
        let panel = self.ivars().information_panel.borrow().clone();
        if let Some(panel) = panel {
            panel.hide();
        }
    }

    /// 真正结束会话先断两个target；空卡片owner随后随controller释放，不逃到进程缓存。
    pub(super) fn detach_information_panel(&self) {
        let panel = self.ivars().information_panel.borrow_mut().take();
        if let Some(panel) = panel {
            panel.hide();
            for button in [&panel.close, &panel.refresh] {
                // SAFETY: 当前主线程仍持有controller和按钮，先断借用再归还各自强引用。
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
            panel.scroll.setDocumentView(None);
        }
    }

    /// 消费真实编辑完成通知或原“刷新词数”按钮；隐藏卡片不读metadata或构造Native值。
    pub(super) fn refresh_information_panel(&self, refresh_words: bool) {
        let panel = self.ivars().information_panel.borrow().clone();
        let Some(panel) = panel.filter(|panel| panel.visible.get()) else {
            return;
        };
        let original = panel.original.borrow().clone();
        let Some(original) = original.filter(NativeMarkdownCurrentNoteIntent::current) else {
            return;
        };
        let Some(document) = original.document.upgrade() else {
            return;
        };
        let counts = Counts::capture(&document.borrow());
        drop(document);
        let revision = panel.revision.get();
        let current = || panel.current(revision, self) && original.current();
        panel.render_counts(counts, refresh_words, &current);
        if current() {
            self.layout_document_area(NativeMarkdownHeaderAuthority::Foreground);
        }
    }

    /// caller持有原surface的独占用例；仅相同Doc Weak可更新成功保存的时间，不重新领取Active。
    pub(super) fn refresh_saved_information(&self, document: &SharedMarkdownDocument) {
        let panel = self.ivars().information_panel.borrow().clone();
        let Some(panel) = panel.filter(|panel| panel.visible.get()) else {
            return;
        };
        let matches =
            panel.original.borrow().as_ref().is_some_and(|original| {
                std::ptr::eq(original.document.as_ptr(), Rc::as_ptr(document))
            });
        if !matches {
            return;
        } // 旧Doc完成保存不能把后来笔记的卡片覆盖成旧统计。
        let (counts, modified) = {
            let document = document.borrow();
            (Counts::capture(&document), modified_time(&document))
        };
        let revision = panel.revision.get();
        let current = || panel.current(revision, self);
        panel.render_counts(counts, false, &current);
        if !current() {
            return;
        }
        let time = NSString::from_str(&display_time(modified));
        if !current() {
            return;
        }
        panel.values[6].setStringValue(&time);
        if current() {
            panel.values[6].setToolTip(Some(&time));
        }
    }

    /// 原正文布局提供唯一parent/frame；仅局部克隆空控件owner，不跨Native持RefCell借用。
    pub(super) fn position_information_panel(
        &self,
        parent: &NSView,
        body: NSRect,
        current: &impl Fn() -> bool,
    ) {
        let panel = self.ivars().information_panel.borrow().clone();
        if let Some(panel) = panel {
            panel.position(parent, body, current);
        }
    }
}
