//! Plain Page actions execute the original NSScrollView operation one at a time.
//! They wait for actual layout height from the same Source without moving text selection.

use super::*;
use omni_markdown_core::MarkdownLayoutTextAuthority;

const MAX_DIRECTION_SEGMENTS: usize = 32;

struct DirectionSegment {
    down: bool,
    remaining: usize,
}

/// Retains the original weak input ticket, scroll authority, and bounded direction runs,
/// without events, document text, a strong Source claim, or Native objects.
pub(super) struct PendingPageScroll {
    request: NativeMarkdownPagedSceneRequest,
    pub source: MarkdownLayoutTextAuthority,
    serial: u64,
    layout_epoch: u64,
    origin: Cell<objc2_foundation::NSPoint>,
    segments: RefCell<VecDeque<DirectionSegment>>,
    executing: Cell<bool>,
}

impl PendingPageScroll {
    /// The original owner, epochs, complete Input, and slot identity must all survive.
    fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.layout_epoch
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && owner
                .pending_page_scroll
                .borrow()
                .as_ref()
                .is_some_and(|pending| std::ptr::eq(&**pending, self))
    }

    /// This operation may change its own origin, but not the original wrapping domain or height.
    fn same_input_geometry(&self, actual: &NativeMarkdownPagedSceneRequest) -> bool {
        actual.target.is_body(&actual.document)
            && Weak::ptr_eq(&actual.document, &self.request.document)
            && actual.input == self.request.input
            && actual.virtual_viewport.layout_width == self.request.virtual_viewport.layout_width
            && actual.virtual_viewport.layout_origin_x
                == self.request.virtual_viewport.layout_origin_x
            && actual.viewport.size.height == self.request.viewport.size.height
    }

    /// Between operations, an external origin change retires this exact scroll sequence.
    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && self.same_input_geometry(actual)
            && actual.virtual_viewport.document_origin == self.origin.get()
    }

    fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        // This Native operation changes origin itself. Preserve original Source/Input/size
        // authority here; the next matches check detects an external scroll takeover.
        self.current(owner)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| self.same_input_geometry(&actual))
            })
            && self.current(owner)
    }

    fn append(&self, down: bool) -> std::result::Result<(), ViewError> {
        if self.executing.get() {
            return Err(ViewError::Busy); // An on-stack Native effect has not established the origin needed to order a new action.
        }
        let mut segments = self.segments.borrow_mut();
        if let Some(last) = segments.back_mut().filter(|last| last.down == down) {
            let remaining = last
                .remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            last.remaining = remaining; // Reject overflow before mutation; repeated direction uses no additional run slot.
        } else {
            if segments.len() == MAX_DIRECTION_SEGMENTS {
                return Err(ViewError::Busy); // Reject a full run queue without dropping admitted reverse actions or reordering them.
            }
            segments.push_back(DirectionSegment { down, remaining: 1 });
        }
        Ok(())
    }

    /// Only the next admitted direction determines whether more lower layout is required.
    pub(super) fn needs_next_page(&self) -> bool {
        self.segments
            .borrow()
            .front()
            .is_some_and(|segment| segment.down)
    }
}

impl NativeMarkdownPagedBody {
    /// Admit against the existing Producer and original Native request, without another Source.
    pub(super) fn accept_page_scroll(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        down: bool,
    ) -> Result<()> {
        self.find_source_health()?; // Ordinary foreground revocation cannot hide the original Source's first I/O failure.
        let previous = self.pending_page_scroll.borrow().clone();
        if let Some(previous) = previous.as_ref() {
            if previous.executing.get() {
                return Err(ViewError::Busy.into());
            }
            if previous.matches(self, actual) {
                previous.append(down)?;
                self.schedule()?;
                return Ok(());
            }
        }
        let serial = self.serial.get();
        let source = {
            let slot = self.producer.borrow();
            slot.as_ref()
                .ok_or(ViewError::Pending)?
                .target_source_authority(actual, serial)?
        }; // Borrow the existing Producer only; absent Source remains Pending instead of acquiring another document.
        if !self.current_body()
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
            || self.input.state().map_err(ViewError::Input)? != actual.input
        {
            return Err(ViewError::Revoked.into());
        }
        self.cancel_pending_find()?;
        let latest = self.view()?.capture_scene_request()?;
        self.find_source_health()?;
        if !latest.same(actual) || self.serial.get() != serial {
            return Err(ViewError::Revoked.into());
        }
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target(); // New scroll intent supersedes old Find/selection reveal so it cannot pull the viewport back.
        self.pending_page_scroll
            .replace(Some(Rc::new(PendingPageScroll {
                request: actual.clone(),
                source,
                serial,
                layout_epoch: self.layout_epoch.get(),
                origin: Cell::new(actual.virtual_viewport.document_origin),
                segments: RefCell::new(VecDeque::from([DirectionSegment { down, remaining: 1 }])),
                executing: Cell::new(false),
            })));
        let old_scene = self.scene.borrow_mut().take();
        drop(old_scene); // Revoke the business Scene; an old unfinished candidate cannot finish this scroll, while the prior complete image may remain visible.
        self.schedule()
    }

    /// Actual editing, wrapping changes, or external scrolling revoke the old sequence.
    pub(super) fn refresh_page_scroll(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let pending = self.pending_page_scroll.borrow().clone();
        if pending
            .as_ref()
            .is_some_and(|pending| !pending.matches(self, actual))
        {
            drop(self.pending_page_scroll.borrow_mut().take()); // Never renew the old queue after input, width, scroll, or owner takeover.
        }
    }

    /// Execute at most one admitted direction per turn. Consume its count before Native
    /// entry so failure cannot replay a scroll whose external effect may already have occurred.
    pub(super) fn consume_page_scroll(&self, pending: &Rc<PendingPageScroll>) -> Result<()> {
        let actual = self.view()?.capture_scene_request()?;
        if !pending.matches(self, &actual) {
            return Err(ViewError::Revoked.into());
        }
        let down = {
            let mut segments = pending.segments.borrow_mut();
            let segment = segments.front_mut().ok_or(ViewError::Revoked)?;
            let down = segment.down;
            segment.remaining -= 1;
            if segment.remaining == 0 {
                segments.pop_front();
            }
            down
        };
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Closed)?;
        pending.executing.set(true);
        let result = viewport.scroll_page(down, &|| pending.current_native(self));
        pending.executing.set(false); // All RefCell borrows ended before Native; end execution ownership before handling its result.
        pending.origin.set(result?);
        if pending.segments.borrow().is_empty() {
            let mut slot = self.pending_page_scroll.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, pending))
            {
                slot.take(); // Remove only this queue; an old callback cannot take a successor request.
            }
        }
        self.schedule() // Even a real EOF no-op yields; repeated successors cannot monopolize one main-thread callback.
    }
}
