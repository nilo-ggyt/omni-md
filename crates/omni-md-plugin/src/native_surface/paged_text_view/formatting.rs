//! Formatting consumes complete Input authority and reuses existing commit notifications; pure Source transactions do not wait for pixel layout.

use super::*;
use omni_markdown_core::MarkdownFormatAction;

impl NativeMarkdownPagedTextView {
    /// The existing Source Insert Table action inserts the complete Chinese template once, with first-cell caret in the same history receipt.
    ///
    /// # Errors
    /// Return Input/IME/Core failure directly; postcommit revocation remains CommittedEffect.
    pub fn insert_default_table(&self) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent(); // Formatting is successor input; even failure cannot leave the original click authorized to activate later.
        let origin = self.origin()?; // Public UI actions cannot claim event ownership from the current Native stack depth.
        self.insert_default_table_at(&origin)
    }

    /// UI and queue head share this Core transaction; only the controlled queued entry supplies an interpreted original Origin.
    pub(super) fn insert_default_table_at(&self, origin: &Origin) -> Result<()> {
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.interpreted && origin.delegate.defer_table_insertion(&permit)? {
            return Ok(());
        }
        // The template needs original document/directed selection only, not geometry; old Scene must already return after Cell commit.
        // The original queue orders admitted word navigation; pending pixels cannot discard this explicit insertion action.
        let commit = Rc::new(origin.input.insert_default_table(&permit, &current)?);
        // Notify, hand off interpretation, and publish history once; first-cell intent continues borrowing the same actual receipt.
        // Do not recapture the caret after Native reentry and misidentify a successor user selection as the inserted first cell.
        self.finish_owned_commit(origin, commit.clone(), true)?;
        origin
            .delegate
            .activate_inserted_table(&commit)
            .map_err(|cause| {
                // The template is published; failed activation is an effect, never authority to insert a second table.
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
            })
    }

    /// Apply one Core format transaction to current Body/Cell selection without reinterpreting menu tags or switching targets.
    ///
    /// Original Input validates Source/generation/complete selection; after Core publication the same Commit authorizes display.
    /// Body retains its viewport and Cell reveals its actual edited endpoint, without jumping to document end or materializing selected text.
    ///
    /// # Errors
    /// IME composition, stale Input authority, and Core formatting failures return directly; display failure after publication remains
    /// CommittedEffect and cannot imply no edit or authorize replay of formatting.
    pub fn apply_format(&self, action: MarkdownFormatAction) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.cancel_link_intent();
        let origin = self.origin()?; // Even a toolbar reentering top-level NSTextInputClient cannot impersonate that event.
        self.apply_format_at(&origin, action)
    }

    /// Duplicate neither formatting nor selection; original Origin decides queuing/event-receipt handoff for one Core commit.
    pub(super) fn apply_format_at(
        &self,
        origin: &Origin,
        action: MarkdownFormatAction,
    ) -> Result<()> {
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if !origin.interpreted && origin.delegate.defer_format(action, &permit)? {
            return Ok(());
        }
        // Core prepares directly from the original piece root; selected text needs neither visible glyphs nor a paragraph String.
        // Keep original Input health/current checks; rebuilding display cannot authorize successor selection or swallow the menu action.
        let commit = origin.input.apply_format(&permit, action, &current)?;
        // Core/observer borrows ended; the original commit path retires old CPU
        // work and schedules Body layout. Only actual scene_ready publication
        // notifies the system that successor character coordinates are available.
        self.finish_edit_commit(origin, commit)
    }
}
