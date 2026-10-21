//! Resolve headings in the existing link slot, then commit one selection through SourceTarget and CPU publication.

use super::*;
use crate::native_surface::paged_body::NativeMarkdownPagedBodyError as Error;
use omni_markdown_core::{MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep};

/// Keep one original request, an optional weak click, and one Core query; no old Scene or document survives note handoff.
/// The query's anonymous scratch belongs to this operation and closes on cancellation/failure, never as a window cache.
pub(super) struct PendingHeading {
    request: NativeMarkdownPagedSceneRequest,
    click: Option<(NativeMarkdownPagedSceneKey, Weak<()>)>,
    pub(super) query: MarkdownHeadingAnchorRequest,
}

impl NativeMarkdownPagedBody {
    /// The URI boundary already decoded the fragment once; the click authorizes only this Scene's heading request.
    /// End Source borrows before enqueueing; never scan files, parse a whole document, or reshape on the mouse stack.
    pub(in crate::native_surface) fn follow_heading(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        intent: Weak<()>,
        fragment: &str,
    ) -> std::result::Result<(), ViewError> {
        // Sharing a numeric anchor does not let an ended click navigate under a newer Scene.
        if !self.scene_current(key) || intent.strong_count() == 0 {
            return Err(ViewError::Revoked);
        }
        self.begin_heading(key.request.clone(), Some((key.clone(), intent)), fragment)
            .map_err(cell::cell_view_error)
    }

    /// A newly installed document owns this fragment after the original save/old-graph barrier.
    /// Use its complete Input/geometry without publishing BOF first or borrowing the previous note's click authority.
    pub(in crate::native_surface) fn navigate_heading(&self, fragment: &str) -> Result<()> {
        let request = self.view()?.capture_scene_request()?;
        self.begin_heading(request, None, fragment)
    }

    /// Bind one Core query to the admitted document before replacing the existing link continuation.
    fn begin_heading(
        &self,
        request: NativeMarkdownPagedSceneRequest,
        click: Option<(NativeMarkdownPagedSceneKey, Weak<()>)>,
        fragment: &str,
    ) -> Result<()> {
        // Heading navigation belongs to the full document, not a cell's local coordinate domain or stale input.
        if self.input.is_cell()
            || !self.current_body()
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
            || self.input.state().map_err(ViewError::Input)? != request.input
        {
            return Err(ViewError::Revoked.into());
        }
        // The bounded query acquires the actual source through Core; no document borrow survives slot publication.
        let document = self.document.borrow().clone().ok_or(Error::Closed)?;
        let query = document
            .try_borrow()
            .map_err(|_| ViewError::Busy)?
            .heading_anchor_request(fragment)
            .map_err(ViewError::HeadingAnchor)?;
        let previous = self.pending_link.replace(Some(PendingLink {
            task: LinkTask::Heading(Box::new(PendingHeading {
                request,
                click,
                query,
            })),
        }));
        if let Some(previous) = previous {
            previous.retire()?; // The old task is out of the slot before fallible scratch cleanup; preserve its error.
        }
        self.schedule()
    }

    /// Run one Core 4096-source-unit step per turn, without borrowing another query's or composer's allowance.
    /// Consume the click and commit selection only on completion; original Source errors outrank later revocation.
    pub(super) fn advance_heading(
        &self,
        mut pending: PendingHeading,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        // Check source health before classifying a changed foreground as ordinary retirement.
        let preparation = (|| -> Result<_> {
            let document = self.document.borrow().clone().ok_or(Error::Closed)?;
            {
                let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
                document
                    .byte_offset_for_utf16(0)
                    .map_err(ViewError::Document)?;
            }
            let view = self.view()?;
            let live = self.current_body()
                && pending.request.same(actual)
                && self.input.state().map_err(ViewError::Input)? == pending.request.input;
            // Same-note clicks also need their original gesture; installed-note fragments use only their own request.
            let click_live = if let Some((key, intent)) = &pending.click {
                match view.link_intent_current(key, intent) {
                    Ok(live) => live,
                    Err(ViewError::Revoked | ViewError::Closed | ViewError::Pending) => false,
                    Err(cause) => return Err(cause.into()),
                }
            } else {
                live // Installation supplies no mouse event and must not fabricate one.
            };
            Ok((document, view, live && click_live))
        })();
        let (document, view, live) = match preparation {
            Ok(prepared) => prepared,
            Err(cause) => {
                // Once taken from the slot, even admission/source failure must explicitly close the scratch.
                // Merge cleanup failure with the original cause; Drop alone cannot report the second error.
                let cleanup = pending
                    .query
                    .cancel()
                    .map_err(|error| ViewError::HeadingAnchor(error).into());
                return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|()| false);
            }
        };
        if !live {
            // This operation lost its original authority. Cancel its own query and weak intent, never a successor's.
            pending.query.cancel().map_err(ViewError::HeadingAnchor)?;
            if let Some((_, intent)) = &pending.click {
                view.retire_link_intent(intent);
            }
            return Ok(false);
        }
        // Keep the document lease only for the bounded Core step and same-source target conversion.
        let result = (|| -> Result<_> {
            let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
            let step = pending
                .query
                .advance(&document, 4096)
                .map_err(ViewError::HeadingAnchor)?;
            Ok(match step {
                MarkdownHeadingAnchorStep::Progress { .. } => None,
                MarkdownHeadingAnchorStep::Found { target, .. } => Some(Some(
                    target
                        .position_for(&document)
                        .map_err(ViewError::HeadingAnchor)?,
                )),
                MarkdownHeadingAnchorStep::NotFound { .. } => Some(None),
            })
        })(); // All Source windows and document borrows end before native work or selection publication.
        let result = match result {
            Ok(result) => result,
            Err(cause) => {
                // A failed step still owns scratch cleanup; retain the actual query error as the primary failure.
                let cleanup = pending
                    .query
                    .cancel()
                    .map_err(|error| ViewError::HeadingAnchor(error).into());
                return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|()| false);
            }
        };
        let Some(position) = result else {
            if self.pending_link.borrow().is_none() {
                *self.pending_link.borrow_mut() = Some(PendingLink {
                    task: LinkTask::Heading(Box::new(pending)),
                });
            } else {
                // Intent validation can admit a newer click; never overwrite it, but still close this query's FD.
                pending.query.cancel().map_err(ViewError::HeadingAnchor)?;
            }
            self.schedule()?;
            return Ok(true);
        };
        if let Some((key, intent)) = &pending.click {
            // Completion is not permission to replace the original click with whatever input is current now.
            if !view.link_intent_current(key, intent)? {
                view.retire_link_intent(intent);
                return Ok(false);
            }
            view.retire_link_intent(intent); // Found and absent targets both consume once; selection retires the old Scene normally.
        }
        let Some(position) = position else {
            // Missing headings produce feedback for the original owner while the installed document remains editable.
            if let Some((key, _)) = &pending.click {
                view.report_link_failure(key, ViewError::LinkAnchorNotFound);
            } else {
                view.report_heading_failure(&pending.request, ViewError::LinkAnchorNotFound);
            }
            self.schedule()?; // The new note must still prepare its first viewport; absence is not document destruction.
            return Ok(true);
        };
        // Core certified the heading's physical source position. The normal selection preserves generation/undo
        // and unfolds only as required; finish_edit_outcome reveals through SourceTarget rather than a fake Find jump.
        view.navigate_to_heading(pending.request.input, position)?;
        Ok(true)
    }
}
