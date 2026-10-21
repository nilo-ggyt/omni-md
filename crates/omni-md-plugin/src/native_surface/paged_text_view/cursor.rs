//! Cursors belong to the original NSView cursor rectangles; retain no URL, link table, or tracking
//! area.
//! Semantic Ready alone is insufficient: registration requires the same complete CPU publication and
//! Source/font/Input, with synchronous region retirement.

use super::*;
use dispatch2::MainThreadBound;
use objc2_app_kit::NSCursor;
use objc2_foundation::NSIntersectionRect;

impl NativeMarkdownPagedTextView {
    /// Read bounded sealed geometry before ending every strong Scene/reader borrow, allowing Native
    /// reentry to actually retire the old graph.
    /// The temporary vector contains Copy rectangles bounded by existing Scene LINK parts, never a
    /// second semantic link cache.
    pub(super) fn install_link_cursor_rects(&self) -> Result<()> {
        let (ticket, key, weak_scene, mut areas) = {
            let Some(origin) = self.display_origin()? else {
                return Ok(());
            };
            let ticket = cpu::candidate::Ticket::new(&origin);
            let scene = match self.scene_for_origin(origin) {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(self)? => {
                    return Ok(());
                }
                Err(cause) => return Err(cause),
            };
            scene.scene.validate_ready(&|| scene.current(self))?;
            let mut areas = Vec::new();
            let mut allocation_failure = false;
            scene.scene.visit_link_cursor_rects(&mut |area| {
                if allocation_failure {
                    return;
                }
                if areas.try_reserve(1).is_err() {
                    allocation_failure = true;
                } else {
                    areas.push(area);
                }
            })?;
            scene.scene.validate_ready(&|| scene.current(self))?;
            if allocation_failure {
                return Err(NativeMarkdownPagedTextViewError::Raster(
                    "无法分配当前可见链接矩形".into(),
                ));
            }
            let key = scene.key.clone();
            let weak_scene = Rc::downgrade(&scene.scene);
            drop(scene); // Release Origin's Body/Input claims too; subsequent AppKit calls hold no original reader.
            (ticket, key, weak_scene, areas)
        };
        // Body and Cells already share View coordinates; bidi or negative advances mean Source order
        // need not increase in x.
        // Sort the existing necessary Copy collection in place, without repeated Scene-part scans or a
        // second allocation.
        areas.sort_unstable_by(|left, right| {
            left.origin
                .y
                .total_cmp(&right.origin.y)
                .then_with(|| left.size.height.total_cmp(&right.size.height))
                .then_with(|| left.origin.x.total_cmp(&right.origin.x))
        });
        let mut used = 0;
        for index in 0..areas.len() {
            let area = areas[index];
            if used != 0 {
                let previous = &mut areas[used - 1];
                // Merge only touching/overlapping x intervals with exactly identical y extents.
                // Different lines, Cell clipping heights,
                // or vertical coordinates remain separate so ordinary text gaps between links never
                // gain a hand cursor.
                if previous.origin.y == area.origin.y
                    && previous.size.height == area.size.height
                    && area.origin.x <= previous.origin.x + previous.size.width
                {
                    let right = (previous.origin.x + previous.size.width)
                        .max(area.origin.x + area.size.width);
                    previous.size.width = right - previous.origin.x;
                    continue;
                }
            }
            areas[used] = area;
            used += 1;
        }
        areas.truncate(used);
        let current = || ticket.current(self, &key);
        let validate = || {
            let scene = weak_scene
                .upgrade()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            scene.validate_ready(&current)
        }; // Each validation returns its temporary Scene before Native work; no Scene enters the Native operation closure.
        let result = (|| {
            validate()?;
            let cpu = self.ivars().cpu.borrow().clone();
            let Some(cpu) = cpu else { return Ok(()) };
            let published = cpu.cursor_publication_current(self, &key, &current);
            validate()?; // Original Source/font failure takes precedence over revocation of a Native relationship.
            if !published? {
                return Ok(());
            }
            let clip = key.request.viewport;
            if !finite_rect(clip, true) {
                return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
            }
            let text = cursor_step(&validate, &current, NSCursor::IBeamCursor)?;
            cursor_step(&validate, &current, || {
                self.addCursorRect_cursor(clip, &text)
            })?;
            let hand = cursor_step(&validate, &current, NSCursor::pointingHandCursor)?;
            // The I-beam base contains hand-cursor subregions; overlapping segments merge per line
            // while original clips separate Cells.
            // Neither trailing whitespace nor hidden Markdown becomes a link, and equal URLs do not
            // merge regions across lines.
            for area in areas {
                let area = NSIntersectionRect(area, clip);
                if area.size.width > 0.0 && area.size.height > 0.0 {
                    cursor_step(&validate, &current, || {
                        self.addCursorRect_cursor(area, &hand)
                    })?;
                }
            }
            let published = cpu.cursor_publication_current(self, &key, &current);
            validate()?;
            if !published? {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            Ok(())
        })();
        match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                // Share the CPU candidate's strict weak ticket; a false boolean alone does not
                // establish ordinary retirement.
                // Preserve resource failure first, then accept only actual client/Input/geometry
                // succession; an empty serial after cleanup grants no authority.
                match validate() {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                }
                self.discardCursorRects();
                if ticket.superseded(self)? {
                    Ok(())
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            other => other,
        }
    }

    /// While synchronous CPU work holds Source, queue weak View/key identities only and enter AppKit
    /// after that stack returns.
    /// Create no new serial, observer, or queue; obsolete notices end without authorizing old cursor
    /// regions under successor Input.
    pub(super) fn queue_link_cursor_refresh(&self, key: &NativeMarkdownPagedSceneKey) {
        let mtm = self.mtm();
        let weak = MainThreadBound::new(objc2::rc::Weak::from(self), mtm);
        let key = MainThreadBound::new(key.clone(), mtm);
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("光标刷新只沿原AppKit主队列");
            let Some(view) = weak.get(mtm).load() else {
                return;
            };
            if view.ivars().closed.get() {
                return;
            }
            let result = view.refresh_link_cursor_rects(key.get(mtm));
            view.record(result);
        });
    }

    /// Notify only for the original actual complete frame, without additional painting or forced cursor
    /// reset/set.
    /// End the Scene strong borrow before window queries; original weak identity lends Source briefly
    /// around each Native operation for validation.
    fn refresh_link_cursor_rects(&self, key: &NativeMarkdownPagedSceneKey) -> Result<()> {
        let (ticket, weak_scene) = {
            let Some(origin) = self.display_origin()? else {
                return Ok(());
            };
            if origin.state != key.request.input
                || origin.geometry_epoch != key.request.geometry_epoch
                || !origin.delegate.scene_current(key)
            {
                return Ok(());
            }
            let ticket = cpu::candidate::Ticket::new(&origin);
            let scene = match self.scene_for_origin(origin) {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => return Ok(()),
                Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(self)? => {
                    return Ok(());
                }
                Err(cause) => return Err(cause),
            };
            if !scene.key.same(key) {
                return Ok(());
            }
            scene.scene.validate_ready(&|| scene.current(self))?;
            let weak_scene = Rc::downgrade(&scene.scene);
            drop(scene);
            (ticket, weak_scene)
        };
        let current = || ticket.current(self, key);
        let validate = || {
            let scene = weak_scene
                .upgrade()
                .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
            scene.validate_ready(&current)
        };
        let result = (|| {
            let window = cursor_step(&validate, &current, || self.window())?;
            if let Some(window) = window {
                cursor_step(&validate, &current, || {
                    window.invalidateCursorRectsForView(self)
                })?;
            }
            Ok(())
        })();
        match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                // AppKit notification may deliver successor edits; preserve original resource failures
                // before accepting actual complete-ticket succession.
                match validate() {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                }
                if ticket.superseded(self)? {
                    Ok(())
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            other => other,
        }
    }
}

/// Validation briefly borrows original Source/font around Native calls; the operation itself retains
/// neither resource.
/// Post-validation returns actual Source failure before ordinary revocation, so hovering cannot hide
/// disk/font failures as an empty cursor.
fn cursor_step<T>(
    validate: &impl Fn() -> Result<()>,
    current: &impl Fn() -> bool,
    operation: impl FnOnce() -> T,
) -> Result<T> {
    validate()?;
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    let value = operation();
    validate()?;
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    Ok(value)
}
