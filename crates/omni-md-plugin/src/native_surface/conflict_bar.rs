//! The existing conflict strip owns fixed controls and a weak intent for the
//! displayed note. Core retains all save and disk-conflict rules.

use super::*;
use objc2_app_kit::NSAttributedStringNSStringDrawing;

const MIN_HEIGHT: f64 = 38.0;
const PADDING: f64 = 12.0;
const GAP: f64 = 8.0;
const BUTTON_HEIGHT: f64 = 28.0;

/// The controller is the sole strong owner. The original intent contains a weak
/// document, path, and window identity; publication revokes stale display actions.
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
    /// Native acceptance borrows the actual assembled controls without a second
    /// strip, save state, or substitute action entry point.
    pub(super) fn probe_controls(&self) -> (&NSBox, &NSTextField, &[Retained<NSButton>; 2]) {
        (&self.root, &self.message, &self.buttons)
    }

    /// Assemble actual controls once at the existing composition root. Failure
    /// leaves no published partial strip or alternative initialization graph.
    pub(super) fn new(controller: &NativeMarkdownController) -> Result<Self, String> {
        let mtm = controller.mtm();
        let ledger = &controller.ivars().owned;
        let root = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
        // Construct hidden until an actual save conflict supplies a message.
        // The original owned ledger observes every control's Native release.
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
            // SAFETY: The original main-thread controller implements this action.
            // Session teardown disconnects target/action before returning the owner.
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

    /// Change the weak intent only on actual note installation/cleanup. Returning
    /// to the same filename cannot revive a different document's old action.
    fn bind(&self, original: Option<NativeMarkdownCurrentNoteIntent>) {
        self.advance_publication();
        *self.original.borrow_mut() = original;
    }
    /// Never wrap publication authority. Repeated identical save failures do not
    /// create another display decision or invalidate the original confirmation.
    fn advance_publication(&self) {
        self.publication.set(
            self.publication
                .get()
                .checked_add(1)
                .expect("冲突处理发布代次耗尽"),
        );
    }

    /// Project the original footer result without guessing disk versions or
    /// introducing another save state machine.
    pub(super) fn publish(&self, message: Option<&'static str>, current: &impl Fn() -> bool) {
        if self.published_message.replace(message) != message {
            self.advance_publication();
        }
        let revision = self.publication.get();
        // Every Native setter may reenter. Continue only for this exact message
        // publication and the caller's original document authority.
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

    /// Measure actual button titles first, then give the message the remaining
    /// width. A long message grows vertically without hiding either full action name.
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
            drop(title); // The finite Native title claim does not cross later layout or state borrows.
        }
        if !current() {
            return Err("冲突处理显示已经变化".into());
        }
        let text_width = (width - 2.0 * PADDING - 2.0 * GAP - widths.iter().sum::<f64>()).max(1.0);
        // AppKit measures wrapping for this fixed message, never for document text.
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

    /// Find visibility and resize reserve the same measured height, without
    /// subtracting repeatedly from a previously modified body frame.
    pub(super) fn reserve_area(&self, mut base: NSRect) -> Result<NSRect, String> {
        if self.published_message.get().is_none() {
            // An absent conflict consumes no space and performs no Native measurement.
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

    /// Preserve feedback, conflict strip, Find, and body order on the original page.
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
        // The shared editor_area_frames result supplies this exact Find/body top;
        // no duplicated feedback height can shift the strip into another region.
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
            // Publish only the original batch. A setter reentering into a new note
            // must stop the remaining old controls before they can overwrite it.
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
        // SAFETY: Query only this control's parent on the original main thread,
        // without recursively inspecting system views or retaining another owner.
        if unsafe { self.root.superview() }.is_none() && current() {
            parent.addSubview(&self.root);
        }
    }

    /// Clear stale display data even after its original intent expires. Session
    /// teardown separately disconnects both targets before releasing controls.
    pub(super) fn clear(&self) {
        self.bind(None);
        // Revoke authority before Native cleanup. A newer reentrant publication
        // stops the remaining old setters without restoring the retired intent.
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

/// Fix the displayed note's original intent before the first sender getter.
/// Hidden strips and successor notes cannot acquire authority from an old button.
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
        // Matching an action tag alone is insufficient: only these actual two
        // assembled buttons may resolve this strip's original note.
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
    // Queue only the original weak intent. The actual modal/save flow revalidates
    // it again before a disk side effect or accepted editor replacement.
    controller.enqueue_owned(move || {
        let mtm = MainThreadMarker::new().expect("冲突处理沿原主队列执行");
        if let Err(error) = original.get(mtm).perform_resolution(tag, revision) {
            if original.get(mtm).current() {
                present_native_markdown_error(&error);
            } else {
                // Preserve the failure at its final boundary without displaying
                // the retired note's feedback over a successor or opening a window.
                eprintln!("markdown_conflict operation=resolve outcome=failed reason=original_owner_changed");
            }
        }
    });
}

impl NativeMarkdownController {
    /// Install one actual strip into the original window ledger; later updates
    /// replace only its weak intent and projected text.
    pub(super) fn install_conflict_bar(&self) -> Result<(), String> {
        let bar = NativeMarkdownConflictBar::new(self)?;
        assert!(
            self.ivars().conflict_bar.set(bar).is_ok(),
            "原冲突条只能装配一次"
        );
        Ok(())
    }
    /// Bind display identity at actual note installation, without recapturing a
    /// later Active document or sidebar path as the original authority.
    pub(super) fn bind_conflict_note(&self, original: NativeMarkdownCurrentNoteIntent) {
        self.ivars()
            .conflict_bar
            .get()
            .expect("原窗口已装配冲突条")
            .bind(Some(original));
    }
    /// The menu and both existing buttons share the same actual display revision
    /// and original file-action construction path.
    pub(super) fn conflict_publication(&self) -> u64 {
        self.ivars()
            .conflict_bar
            .get()
            .expect("实际窗口已装配冲突条")
            .publication
            .get()
    }
    /// Clear actual text and weak intents on teardown. A failed construction
    /// with no installed strip cannot allocate a substitute during cleanup.
    pub(super) fn clear_conflict_bar(&self) {
        if let Some(bar) = self.ivars().conflict_bar.get() {
            bar.clear();
        }
    }
    /// Return borrowed targets/actions at session end through the existing
    /// return lifecycle, without adding another close interaction.
    pub(super) fn detach_conflict_bar(&self) {
        if let Some(bar) = self.ivars().conflict_bar.get() {
            bar.clear();
            for button in &bar.buttons {
                // SAFETY: The original main-thread controller is alive while
                // detaching standard actions before returning the control owners.
                unsafe {
                    button.setTarget(None);
                    button.setAction(None);
                }
            }
            bar.root.removeFromSuperview();
        }
    }
}
