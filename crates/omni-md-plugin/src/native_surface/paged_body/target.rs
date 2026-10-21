//! A Source target records one original intent in the same Body or Cell input domain.
//! The production layout path alone certifies its actual position and visibility.

use super::*;
use crate::native_surface::{
    paged_input::{NativeMarkdownPagedInputState, NativeMarkdownPagedTargetDomain},
    paged_text_view::NativeMarkdownPagedFindVisibleProof,
};
use objc2_foundation::NSRange;
use omni_markdown_core::MarkdownLayoutTextAuthority;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Terminal {
    Pending,
    Visible,
    Revoked,
}

/// Retains a fixed request and weak identities; callers cannot manufacture a Source,
/// measured position, or visibility proof.
pub struct NativeMarkdownPagedBodySourceTarget(Rc<SourceTargetRequest>);

impl NativeMarkdownPagedBodySourceTarget {
    /// Report the original terminal state, so a consumed proof never looks pending.
    pub fn is_pending(&self) -> bool {
        self.0.terminal.get() == Terminal::Pending
    }
}

pub(super) struct SourceTargetRequest {
    owner: Weak<NativeMarkdownPagedBody>,
    pub(super) source: MarkdownLayoutTextAuthority,
    document: Weak<RefCell<MarkdownDocumentBuffer>>,
    domain: NativeMarkdownPagedTargetDomain,
    state: NativeMarkdownPagedInputState,
    serial: u64,
    layout_epoch: u64,
    layout_width: f64,
    layout_origin_x: f64,
    pub(super) range: NSRange,
    pub(super) first_end: usize,
    terminal: Cell<Terminal>,
    proof: RefCell<Option<NativeMarkdownPagedFindVisibleProof>>,
}

impl SourceTargetRequest {
    /// Revoke this request and discard only its own one-shot visibility proof.
    pub(super) fn revoke(&self) {
        self.terminal.set(Terminal::Revoked);
        drop(self.proof.borrow_mut().take());
    }
    /// Waiting is an explicit state, independent of whether a proof slot is empty.
    pub(super) fn pending(&self) -> bool {
        self.terminal.get() == Terminal::Pending
    }
    /// Match the original input domain and wrapping geometry against a Scene request.
    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && self.domain.same(&actual.target)
            && Weak::ptr_eq(&self.document, &actual.document)
            && actual.input == self.state
            && actual.virtual_viewport.layout_width == self.layout_width
            && actual.virtual_viewport.layout_origin_x == self.layout_origin_x
    }
    /// Check original Rust-owned epochs and weak identities without loading Native objects.
    pub(super) fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        self.terminal.get() != Terminal::Revoked
            && owner.current_body()
            && Weak::ptr_eq(&self.owner, &owner.weak)
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.layout_epoch
            && Weak::ptr_eq(&self.document, &owner.input.document_identity())
            && owner
                .input
                .target_domain()
                .is_ok_and(|actual| self.domain.same(&actual))
            && owner.input.state().is_ok_and(|state| state == self.state)
    }
    /// Recheck the original Native geometry around any reentrant view lookup.
    pub(super) fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        if !self.current(owner)
            || !owner.find_source_target_current(self)
            || !owner.mode_target_current_native(self)
        {
            return false;
        }
        // A stable target may cause its own scroll; the original wrapping width
        // must still survive every reentrant geometry operation.
        let matches = owner
            .view()
            .and_then(|view| view.capture_scene_request().map_err(Into::into))
            .is_ok_and(|actual| self.matches(owner, &actual));
        matches
            && self.current(owner)
            && owner.find_source_target_current(self)
            && owner.mode_target_current_native(self)
    }
}

impl NativeMarkdownPagedBody {
    /// Capture intent in the current Ready input domain without changing selection
    /// or treating a raw range as a measured location.
    /// Return Pending until a first frame exists; Find and navigation retain their owners.
    pub fn request_source_target(
        &self,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedBodySourceTarget> {
        if self.preparing.get() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let view = self.view()?;
        let key = view.capture_ready_scene_key()?;
        let source = {
            let producer = self.producer.borrow();
            let producer = producer.as_ref().ok_or(ViewError::Pending)?;
            if !producer.owns_target_source(&key) {
                return Err(ViewError::Revoked.into());
            }
            producer.target_source_authority(&key.request, self.serial.get())?
        };
        let request =
            self.admit_source_target(&key.request, source, range, &|| self.scene_current(&key))?;
        self.install_source_target(request.clone())?;
        Ok(NativeMarkdownPagedBodySourceTarget(request))
    }

    /// Admit only the original Producer Source identity and caller scope. A committed Find
    /// may use its actual successor before Ready; this acquires no Source, reader, or visibility.
    pub(super) fn admit_source_target(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        source: MarkdownLayoutTextAuthority,
        range: NSRange,
        scope: &impl Fn() -> bool,
    ) -> Result<Rc<SourceTargetRequest>> {
        let state = actual.input;
        let serial = self.serial.get();
        let layout_epoch = self.layout_epoch.get();
        let domain = self.input.target_domain().map_err(ViewError::Input)?;
        if !domain.same(&actual.target)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
        {
            return Err(ViewError::Revoked.into());
        }
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(ViewError::InvalidRange)?;
        if end > state.utf16_len {
            return Err(ViewError::InvalidRange.into());
        }
        let current = |epoch| {
            epoch == self.configuration.epoch
                && self.current_body()
                && self.serial.get() == serial
                && self.layout_epoch.get() == layout_epoch
                && self
                    .input
                    .target_domain()
                    .is_ok_and(|actual| domain.same(&actual))
                && self.input.state().is_ok_and(|value| value == state)
                && scope()
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        let mut first_end = range.location;
        if range.location < state.utf16_len {
            let mut units = [0u16; 2];
            self.input
                .copy_utf16_code_units(&permit, range.location, &mut units[..1], &current)
                .map_err(ViewError::Input)?;
            if (0xdc00..=0xdfff).contains(&units[0]) {
                return Err(ViewError::InvalidRange.into());
            }
            let width = if (0xd800..=0xdbff).contains(&units[0]) {
                // Read only the low surrogate of the first scalar; a long match
                // does not authorize reading or shaping the complete source range.
                self.input
                    .copy_utf16_code_units(&permit, range.location + 1, &mut units[1..], &current)
                    .map_err(ViewError::Input)?;
                if !(0xdc00..=0xdfff).contains(&units[1]) {
                    return Err(ViewError::InvalidRange.into());
                }
                2
            } else {
                1
            };
            if range.length != 0 {
                first_end = range
                    .location
                    .checked_add(width)
                    .ok_or(ViewError::InvalidRange)?;
                if first_end > end {
                    return Err(ViewError::InvalidRange.into());
                }
            }
        }
        let accepted = current(self.configuration.epoch);
        // The Cell scope checks its Source failure before ordinary revocation;
        // a canonical weak identity cannot renew authority over local coordinates.
        let actual_domain = self.input.target_domain().map_err(ViewError::Input)?;
        if !accepted || !domain.same(&actual_domain) {
            return Err(ViewError::Revoked.into());
        }
        Ok(Rc::new(SourceTargetRequest {
            owner: self.weak.clone(),
            source,
            document: actual.document.clone(),
            domain,
            state,
            serial,
            layout_epoch,
            layout_width: actual.virtual_viewport.layout_width,
            layout_origin_x: actual.virtual_viewport.layout_origin_x,
            range,
            first_end,
            terminal: Cell::new(Terminal::Pending),
            proof: RefCell::new(None),
        }))
    }

    /// Replace scroll intent only after the new target proves its original ownership.
    pub(super) fn install_source_target(&self, request: Rc<SourceTargetRequest>) -> Result<()> {
        if !request.current(self) {
            return Err(ViewError::Revoked.into());
        }
        drop(self.pending_page_scroll.borrow_mut().take()); // Rejected foreign requests must not cancel the currently accepted scroll.
        let old = self.source_target.replace(Some(request.clone()));
        if let Some(old) = old {
            old.revoke();
        }
        drop(self.scene.borrow_mut().take()); // Keep the complete CPU frame while returning the previous reader before prefix growth.
        if let Err(cause) = self.schedule() {
            request.revoke();
            return Err(cause);
        }
        Ok(())
    }

    /// Consume the original View visibility proof once; consumed proofs are never reissued.
    /// None with is_pending=true means the original continuation is unfinished; None
    /// with false means consumed, not a reason to resubmit. Revocation and close remain errors.
    pub fn take_source_target_visible(
        &self,
        target: &NativeMarkdownPagedBodySourceTarget,
    ) -> Result<Option<NativeMarkdownPagedFindVisibleProof>> {
        self.take_source_target_request_visible(&target.0)
    }

    /// Preserve Source health and original Native authority before consuming a proof.
    pub(super) fn take_source_target_request_visible(
        &self,
        target: &Rc<SourceTargetRequest>,
    ) -> Result<Option<NativeMarkdownPagedFindVisibleProof>> {
        if self.input.is_cell() {
            // Check the Cell scope and original mapping first; a successor content
            // epoch must not read an obsolete reader merely to diagnose revocation.
            let actual = self.input.target_domain().map_err(ViewError::Input)?;
            if actual.same(&target.domain)
                && let Some(producer) = self.producer.borrow().as_ref()
            {
                producer.target_source_health()?;
            } // Borrow only the existing Producer; never acquire a Source to renew stale content.
        } else {
            // Body keeps its canonical endpoint health check; Cell integration
            // does not relax the Find domain or its original document authority.
            let document = self
                .document
                .borrow()
                .clone()
                .ok_or(NativeMarkdownPagedBodyError::Closed)?;
            document
                .try_borrow()
                .map_err(|_| ViewError::Busy)?
                .byte_offset_for_utf16(0)
                .map_err(ViewError::Document)?;
        }
        if !target.current_native(self) {
            target.revoke();
            return Err(ViewError::Revoked.into());
        }
        let proof = target.proof.borrow_mut().take();
        if let Some(proof) = &proof {
            self.view()?
                .verify_find_target_visible(proof.key(), target.range)?;
            if !target.current_native(self) {
                target.revoke();
                return Err(ViewError::Revoked.into());
            }
        }
        Ok(proof)
    }

    /// Report only accepted, unfinished target work.
    pub(super) fn source_target_pending(&self) -> bool {
        self.source_target
            .borrow()
            .as_ref()
            .is_some_and(|request| request.pending())
    }
    /// A new actual request revokes a mismatching target without granting successor rights.
    pub(super) fn refresh_source_target(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let target = self.source_target.borrow().clone();
        if let Some(target) = target
            && !target.matches(self, actual)
        {
            target.revoke();
        }
    }
    /// Detach the old owner slot before releasing the request and its proof.
    pub(super) fn revoke_source_target(&self) {
        let target = self.source_target.borrow_mut().take();
        if let Some(target) = target {
            target.revoke();
        }
    }
    /// Certify visibility only from the same Source and completed production Scene.
    pub(super) fn finish_source_target(&self) -> Result<()> {
        let target = self.source_target.borrow().clone();
        let scene = self.scene.borrow().clone();
        let (Some(target), Some(scene)) = (target, scene) else {
            return Ok(());
        };
        if !target.pending() || !target.matches(self, &scene.key().request) {
            return Ok(());
        }
        if target.source != scene.key().source {
            target.revoke();
            return Ok(());
        }
        let result = if self.reopening_empty_cell_target(&target)? {
            // An empty raw Cell has no parent display glyph. Its original target still
            // drives real row/column reveal; only the current Reopen consumes the actual
            // content hit after preparation. Find and mode never receive an empty proof.
            None
        } else {
            Some(
                self.view()?
                    .verify_find_target_visible(scene.key(), target.range),
            )
        };
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // The original Source fault outranks revocation during visibility verification.
        }
        if !target.current_native(self) {
            target.revoke();
            self.schedule()?;
            return Ok(()); // Revoke only this old target; successor display keeps its existing continuation.
        }
        let Some(result) = result else {
            return Ok(());
        };
        match result {
            Ok(proof) => {
                if !target.current_native(self) {
                    target.revoke();
                    return Ok(());
                }
                *target.proof.borrow_mut() = Some(proof);
                target.terminal.set(Terminal::Visible);
            }
            Err(ViewError::Pending) => {
                return Err(ViewError::Pending.into());
            }
            Err(cause) => return Err(cause.into()),
        }
        Ok(())
    }
}
