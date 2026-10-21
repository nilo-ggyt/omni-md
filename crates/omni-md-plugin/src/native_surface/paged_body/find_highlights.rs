//! Live queries borrow the original bar and currently laid-out windows. Match coordinates
//! travel with the Scene, without retaining query text or another document copy.

use super::*;
use crate::native_surface::{
    NativeMarkdownController,
    find_bar::{
        NativeMarkdownFindBar, NativeMarkdownFindViewportFeedback, NativeMarkdownFindVisibleSource,
        NativeMarkdownPagedFindHighlightError,
    },
};
use objc2::rc::{Retained, Weak as NativeWeak};
use std::ops::Range;

#[derive(Clone)]
pub(super) struct Binding {
    bar: Weak<NativeMarkdownFindBar>,
    controller: Rc<NativeWeak<NativeMarkdownController>>,
}

/// Match output and temporary claims on the original bar survive through Scene installation.
/// If releasing a Native claim reenters input, that input can already revoke the installed
/// Scene rather than having an old query's frame installed after its revocation.
pub(super) struct Prepared {
    pub ranges: Box<[Range<usize>]>,
    _binding: Option<(
        Rc<NativeMarkdownFindBar>,
        Retained<NativeMarkdownController>,
    )>,
}

impl NativeMarkdownPagedBody {
    /// The composition root binds this note's actual bar using weak references,
    /// without adding document, Source, or Native strong ownership.
    pub(in crate::native_surface) fn bind_find_highlights(
        &self,
        bar: Weak<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
    ) {
        let binding = Binding {
            bar,
            controller: Rc::new(NativeWeak::from(controller)),
        };
        drop(self.find_highlights.replace(Some(binding)));
        self.refresh_bound_find_highlights();
    }

    /// During mode handoff a bar may unbind only itself. A late old bar cannot
    /// remove the successor bar's binding.
    pub(in crate::native_surface) fn unbind_find_highlights(
        &self,
        bar: &Weak<NativeMarkdownFindBar>,
    ) {
        let owns = self
            .find_highlights
            .borrow()
            .as_ref()
            .is_some_and(|actual| Weak::ptr_eq(&actual.bar, bar));
        if owns {
            self.clear_find_highlights_binding();
        }
    }

    /// Accepted close retains neither weak control blocks nor old matches. While
    /// Body remains active, its original queue replaces the old highlight image.
    pub(in crate::native_surface) fn clear_find_highlights_binding(&self) {
        let binding = self.find_highlights.borrow_mut().take();
        drop(binding); // The final NativeWeak control block may enter the platform; no Body RefMut survives here.
        self.refresh_bound_find_highlights();
    }

    fn refresh_bound_find_highlights(&self) {
        if self.current_body() {
            // Revoke the CPU candidate immediately. Explicit heading-query close
            // failure also reaches the same Body first-failure boundary.
            if let Err(cause) = self
                .clear_scene()
                .map_err(Into::into)
                .and_then(|()| self.schedule())
            {
                self.fail(cause);
            }
        }
    }

    /// Input owns the original Feedback. Revoke the frame and coalesce the same
    /// queue without reading the query, changing selection, or acquiring a Source.
    pub(in crate::native_surface) fn refresh_find_highlights(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        feedback: &NativeMarkdownFindViewportFeedback,
    ) {
        let binding = self.find_highlights.borrow().clone();
        if self.current_body()
            && binding
                .as_ref()
                .is_some_and(|actual| Weak::ptr_eq(&actual.bar, &Rc::downgrade(bar)))
            && feedback.current(controller)
        {
            self.refresh_bound_find_highlights();
        }
    }

    /// Read this bar's committed query only when publishing actual windows. The
    /// visitor's cancellation check uses Rust facts and the already-held controller,
    /// without per-character Scene.current, viewport.geometry, or Native getters.
    pub(super) fn prepare_find_highlights(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        window: Option<&super::super::paged_line_viewport::NativeMarkdownComposedViewport>,
        grids: &[super::scene::GridRowPublication],
        before_publish: impl FnOnce() -> Result<()>,
    ) -> Result<Option<Prepared>> {
        let empty = || Prepared {
            ranges: Box::new([]),
            _binding: None,
        };
        let binding = self.find_highlights.borrow().clone();
        let Some(binding) = binding else {
            before_publish()?;
            return Ok(self.find_highlights.borrow().is_none().then(empty));
        };
        let bound = || {
            self.find_highlights
                .borrow()
                .as_ref()
                .is_some_and(|actual| {
                    Weak::ptr_eq(&actual.bar, &binding.bar)
                        && Rc::ptr_eq(&actual.controller, &binding.controller)
                })
        };
        let Some(bar) = binding.bar.upgrade() else {
            before_publish()?;
            return Ok(bound().then(empty));
        };
        // Load the Native weak reference outside Body RefCell borrows. The hot
        // loop borrows this stack's actual controller rather than repeatedly loading it.
        let Some(controller) = binding.controller.load() else {
            before_publish()?;
            return Ok(bound().then(empty));
        };
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            &controller,
            controller.find_input_intent(),
            false,
        ) else {
            before_publish()?;
            return Ok(bound().then(|| Prepared {
                ranges: Box::new([]),
                _binding: Some((bar, controller)),
            })); // The original window lost foreground authority; another controller cannot substitute for it.
        };
        let serial = self.serial.get();
        let layout_epoch = self.layout_epoch.get();
        let current = || {
            self.current_body()
                && self.serial.get() == serial
                && self.layout_epoch.get() == layout_epoch
                && self
                    .input
                    .state()
                    .is_ok_and(|state| state == key.request.input)
                && bound()
                && feedback.current(&controller)
        };
        if !current() {
            return Ok(None);
        }
        if key.request.input.marked.is_some()
            || self.input.is_cell()
            || self.active_cell_body().is_some()
        {
            // Uncommitted composition or a local draft cannot display stale canonical
            // matches. The final Native authority check may also change the query.
            before_publish()?;
            return Ok(current().then(|| Prepared {
                ranges: Box::new([]),
                _binding: Some((bar, controller)),
            }));
        }
        let mut visible = Vec::new();
        let mut sources = Vec::new();
        if let Some(window) = window {
            visible.extend(
                window
                    .visible_find_ranges(&current)
                    .map_err(producer::scene_error)?,
            );
        }
        for row in grids {
            for cell in &row.cells {
                if let super::scene::GridCellContent::Composed(window) = &cell.content
                    && let Some(range) = window
                        .visible_canonical_range(&current)
                        .map_err(producer::scene_error)?
                {
                    sources.push(NativeMarkdownFindVisibleSource::Cell {
                        canonical: range,
                        window,
                    });
                }
            }
        }
        // Body/cell edges can overlap the same physical range. Sort and merge the
        // bounded ranges so neither Source reads nor translucent highlight coverage repeat.
        visible.sort_unstable_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(visible.len());
        for range in visible {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        sources.extend(
            merged
                .into_iter()
                .map(NativeMarkdownFindVisibleSource::Body),
        );
        sources.sort_unstable_by_key(|source| match source {
            NativeMarkdownFindVisibleSource::Body(range) => range.start,
            NativeMarkdownFindVisibleSource::Cell { canonical, .. } => canonical.start,
        }); // Sort only laid-out windows; matching lazily advances each Grid cell's actual raw fragments through its original directory.
        let document = self
            .document
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        let result = match feedback.prepare_paged_highlights(&bar, &document, &sources, &current) {
            Ok(result) => result,
            Err(NativeMarkdownPagedFindHighlightError::Source(
                omni_markdown_core::MarkdownDocumentBufferError::FindInterrupted,
            )) if !current() => {
                return Ok(None);
            }
            Err(NativeMarkdownPagedFindHighlightError::Source(cause)) => {
                return Err(NativeMarkdownPagedBodyError::View(ViewError::Document(
                    cause,
                )));
            }
            Err(NativeMarkdownPagedFindHighlightError::Window(cause)) => {
                return Err(NativeMarkdownPagedBodyError::View(producer::scene_error(
                    cause,
                )));
            }
        };
        // Perform the original ticket's final Source/Native geometry check here.
        // If its getter admits a new query, the Rust checks below reject old matches;
        // no additional Native check separates this output from Producer Scene installation.
        before_publish()?;
        if current() {
            Ok(Some(Prepared {
                ranges: result,
                _binding: Some((bar, controller)),
            }))
        } else {
            Ok(None)
        }
    }
}
