//! Document information within the original body. Project finite identity and
//! tree aggregates without taking over input, document, or window lifetime.

use super::*;
use objc2_app_kit::NSScrollerStyle;

const WIDTH: f64 = 270.0;
const PADDING: f64 = 13.0;
const LABEL_WIDTH: f64 = 48.0;
const GAP: f64 = 8.0;

/// Reuse fixed controls after the first explicit open. Hide revokes the original
/// ticket, clears business text, and detaches the card from its parent. Only the
/// session controller owns it; Weak intent cannot prolong document/editor/mapping life.
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
    /// Capture canonical tree aggregates, ending this read-only borrow before
    /// the caller presents any Native values.
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
    /// Lend this card's actually assembled controls to Native acceptance without
    /// constructing substitute controls or another document/observation owner.
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

    /// Assemble every fixed role once. A ledger rejection drops the local
    /// candidate instead of publishing an incompletely registered resource graph.
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
        // Propagate each real registration failure; rejecting a later field
        // cannot erase Weak evidence of previously constructed roles.
        let labels: [Retained<NSTextField>; 7] = labels
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .expect("信息卡有固定七个标签");
        // Words update on open/explicit refresh. The four-character label fits
        // the original column; its tooltip starts no text reads or timer.
        labels[3].setToolTip(Some(ns_string!("显示上次统计结果；编辑后点击刷新词数更新")));
        let values: [Retained<NSTextField>; 7] = (0..7)
            .map(|_| Self::label("", true, controller))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .expect("信息卡有固定七个值");
        for (label, value) in labels.iter().zip(&values) {
            // These fields contain only finite title/path/count/time values;
            // their actual text determines height, without copying document content.
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

    /// Standard read-only multiline labels display identity only and never join
    /// the shared field editor, IME, or document Undo chain.
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
        // Preserve complete paths/titles. Character wrapping keeps unspaced
        // paths inside the card without truncating their displayed identity.
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

    /// Existing card actions borrow the same controller; session teardown
    /// symmetrically removes target/action before returning control ownership.
    fn button(
        title: &str,
        explanation: &str,
        action: Sel,
        controller: &NativeMarkdownController,
    ) -> Result<Retained<NSButton>, String> {
        // SAFETY: The live main-thread controller implements this fixed action
        // ABI. Detach removes borrowed targets before its owner ends.
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

    /// Revoke scalar authority before reentrant cleanup. Repeated open/hide
    /// cycles reuse this one set of empty controls.
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
                return; // Reentry reopened the card; old cleanup cannot erase successor values.
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

    /// Accept only the same display revision. Native setters that close/reopen
    /// the card revoke any remaining old-value publication.
    fn current(&self, revision: u64, controller: &NativeMarkdownController) -> bool {
        self.visible.get()
            && self.revision.get() == revision
            && native_markdown_controller_has_foreground_authority(controller)
    }

    /// The caller already authenticated the document. Render four finite
    /// aggregates while preserving explicit word-refresh semantics.
    fn render_counts(&self, counts: Counts, refresh_words: bool, current: &impl Fn() -> bool) {
        for (index, value) in [
            (2, counts.characters),
            (3, counts.words),
            (4, counts.lines),
            (5, counts.bytes),
        ] {
            if index == 3 && !refresh_words {
                continue; // Ordinary edits cannot silently replace the word snapshot sampled on open/refresh.
            }
            if !current() {
                return;
            }
            self.values[index].setStringValue(&NSString::from_str(&value.to_string()));
        }
    }

    /// Preserve the 270-point overlay and existing top/right margins. Short
    /// body regions scroll within the card rather than compressing the editor.
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
            // Preserve distance from the card's top, so resizing or refreshing
            // counts does not unexpectedly move the user back to its heading.
            (old_height - old_bounds.origin.y - old_bounds.size.height).max(0.0)
        };
        // Measure only the seven finite display values at the actual column
        // width. The document's logical height never participates in this card.
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
        // Retain natural content height for internal scrolling while clipping
        // the overlay to the current body. Each Native setter may revoke current.
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
        // Lay out heading/actions before the independent label/value rows; the
        // close button remains a card action under the same display revision.
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
        // Allocate each actual measured value height without shrinking long
        // paths or overlapping the following row.
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
        // Restore the previous top-relative position only after final content
        // geometry is installed, then let the actual scroller reflect it.
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
        // Attach only when absent. Resize does not reorder siblings or bring
        // unrelated controls to the front.
        // SAFETY: Read this card's existing parent on the main thread, without
        // holding a state borrow across the Native getter.
        if unsafe { self.root.superview() }.is_none() && current() {
            parent.addSubview(&self.root);
        }
    }
}

/// Read file metadata only on open/successful save. Failure remains unavailable;
/// presentation uses the current local locale and time zone.
fn modified_time(document: &MarkdownDocumentBuffer) -> Option<std::time::SystemTime> {
    document
        .path()
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
}
/// Format this finite metadata value without opening the document or reading text.
fn display_time(time: Option<std::time::SystemTime>) -> String {
    let Some(time) = time else {
        return "未获取".to_owned();
    };
    let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(), // Preserve fractional time without another file/text read.
        Err(error) => -error.duration().as_secs_f64(), // Dates before Unix epoch remain legitimate metadata.
    };
    let date = objc2_foundation::NSDate::dateWithTimeIntervalSince1970(seconds);
    objc2_foundation::NSDateFormatter::localizedStringFromDate_dateStyle_timeStyle(
        &date,
        objc2_foundation::NSDateFormatterStyle::MediumStyle,
        objc2_foundation::NSDateFormatterStyle::MediumStyle,
    )
    .to_string()
}

/// The original menu captured note intent before sender getters. Read-only
/// information does not require committing IME composition or the active cell.
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
        return Ok(()); // Repeated Information hides only the card, preserving editor focus and draft.
    }
    let panel = match existing {
        Some(panel) => panel,
        None => {
            let panel = Rc::new(NativeMarkdownInformationPanel::new(&controller)?);
            if !intent.current() {
                return Ok(());
            } // Reentrant construction revocation drops this still-local candidate.
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
    drop(document); // End document borrows and transient ownership before Native date/field calls.
    if !intent.current() {
        return Ok(());
    }
    // Publish the original weak note intent and a fresh display revision before
    // Native formatting/setters can reenter and supersede this open action.
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
    /// Accepted document handoff/close synchronously revokes the original intent,
    /// text, and parent relation without waiting for another open or queue turn.
    pub(super) fn hide_information_panel(&self) {
        let panel = self.ivars().information_panel.borrow().clone();
        if let Some(panel) = panel {
            panel.hide();
        }
    }

    /// End both borrowed targets before returning this session's card owner;
    /// no project card owner escapes into a process-wide cache.
    pub(super) fn detach_information_panel(&self) {
        let panel = self.ivars().information_panel.borrow_mut().take();
        if let Some(panel) = panel {
            panel.hide();
            for button in [&panel.close, &panel.refresh] {
                // SAFETY: Controller/buttons remain live on this main thread.
                // Disconnect borrowed callbacks before releasing their owners.
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
            panel.scroll.setDocumentView(None);
        }
    }

    /// Consume actual edit completion or explicit word refresh. A hidden card
    /// reads no metadata and constructs no Native display value.
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

    /// The caller owns the original surface operation. Only the same document
    /// Weak may refresh successful-save time, without reacquiring Active authority.
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
        } // A late save from an old document cannot overwrite a successor note's information.
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

    /// Original body layout supplies the parent/frame. Clone the fixed control
    /// owner locally, without holding a RefCell borrow across Native positioning.
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
