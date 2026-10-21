//! Finite selection facts share the view/fold lifetime, owning no body text, native objects or asynchronous execution authority.

use super::MarkdownTextSelection;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use super::NSRange;
use std::cell::Cell;

/// Bind the original L1/L2 caret to its real bidi level and logical grapheme side,
/// independently of soft-wrap affinity and visual screen direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "native-paged-production")]
pub struct NativeMarkdownBidiCaretSide {
    pub level: u8,
    /// True binds the following logical grapheme; false binds the preceding one.
    pub logical_after: bool,
}

/// Preserve a target column across vertical moves; line-fragment and container coordinates are not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownVerticalGoal {
    pub(super) line_offset: f64,
    pub(super) container_x: f64,
    pub(super) width_key: i64,
    pub(super) geometry_epoch: u64,
}

impl NativeMarkdownVerticalGoal {
    /// Accept finite coordinates from the original layout's actual caret. The caller separately
    /// matches source, width and layout generation; a scrolling view-event epoch is not a layout epoch.
    #[must_use]
    pub fn new(
        line_offset: f64,
        container_x: f64,
        width_key: i64,
        geometry_epoch: u64,
    ) -> Option<Self> {
        if !line_offset.is_finite() || !container_x.is_finite() {
            // Nonfinite coordinates cannot authorize native navigation; preserve selection instead of inventing a default column.
            return None;
        }
        Some(Self {
            line_offset,
            container_x,
            width_key,
            geometry_epoch,
        })
    }

    /// Revalidate both coordinate spaces before publishing a borrowed vertical goal.
    pub(super) fn is_finite(self) -> bool {
        self.line_offset.is_finite() && self.container_x.is_finite()
    }

    /// Return the original line-fragment offset, independently of the container-space horizontal position.
    #[cfg(feature = "native-paged-production")]
    #[must_use]
    pub fn line_offset(self) -> f64 {
        self.line_offset
    }

    /// Preserve the original container-space target even when a shorter line returns a nearer caret.
    #[cfg(feature = "native-paged-production")]
    #[must_use]
    pub fn container_x(self) -> f64 {
        self.container_x
    }

    /// Identify actual text-container line width, excluding window and viewport margins.
    #[cfg(feature = "native-paged-production")]
    #[must_use]
    pub fn width_key(self) -> i64 {
        self.width_key
    }

    /// Identify the original producer's layout coordinates, invalidated by source, style or width changes.
    #[cfg(feature = "native-paged-production")]
    #[must_use]
    pub fn layout_epoch(self) -> u64 {
        self.geometry_epoch
    }
}

/// Only the original receipt's two declared ranges belong to installation; other notifications return to real input handling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) enum NativeMarkdownSelectionInstallObservation {
    OwnDeclared,
    Foreign,
}

#[derive(Clone, Copy)]
/// Keep direction and geometry metadata for one exact logical selection, without retaining its source graph.
struct KnownSelection {
    selection: MarkdownTextSelection,
    head_is_upstream: Option<bool>,
    vertical_goal: Option<NativeMarkdownVerticalGoal>,
    #[cfg(feature = "native-paged-production")]
    bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

#[derive(Clone, Copy)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
/// Identify one stack-scoped TextKit installation, including its declared before/after ranges.
struct NativeInstall {
    id: u64,
    generation: u64,
    serial: u64,
    before: NSRange,
    after: NSRange,
    selection: MarkdownTextSelection,
}

/// Both clients share selection facts; only the independent TextKit client needs a stack-scoped
/// installation lease. Production paged input publishes Core facts without a TextKit before/after slot.
pub(super) struct NativeMarkdownSelectionState {
    generation: Cell<u64>,
    serial: Cell<u64>,
    known: Cell<Option<KnownSelection>>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    install: Cell<Option<NativeInstall>>,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    next_install_id: Cell<u64>,
    sealed: Cell<bool>,
}

impl NativeMarkdownSelectionState {
    /// Begin with known logical endpoints but no evidence of wrap affinity, vertical goal or bidi side.
    pub(super) fn new(generation: u64, selection: MarkdownTextSelection) -> Self {
        Self {
            generation: Cell::new(generation),
            serial: Cell::new(0),
            known: Cell::new(Some(KnownSelection {
                selection,
                head_is_upstream: None,
                vertical_goal: None,
                #[cfg(feature = "native-paged-production")]
                bidi_side: None,
            })),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            install: Cell::new(None),
            #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
            next_install_id: Cell::new(0),
            sealed: Cell::new(false),
        }
    }

    /// Lend the current action-revocation serial without acquiring any native or document owner.
    pub(super) fn serial(&self) -> u64 {
        self.serial.get()
    }

    /// Require both generation and current Core selection; unseen edits cannot inherit old directional authority.
    pub(super) fn known_selection(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<MarkdownTextSelection> {
        self.known_metadata(generation, selection)
            .map(|known| known.selection)
    }

    /// Expose wrap affinity only while the original complete logical selection still matches.
    pub(super) fn known_head_is_upstream(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<bool> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.head_is_upstream)
    }

    /// Reuse the captured vertical target only for the same current logical selection and generation.
    pub(super) fn known_vertical_goal(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<NativeMarkdownVerticalGoal> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.vertical_goal)
    }

    #[cfg(feature = "native-paged-production")]
    /// Reuse the original bidi caret side only while its exact selection facts remain known.
    pub(super) fn known_bidi_side(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<NativeMarkdownBidiCaretSide> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.bidi_side)
    }

    /// Match complete logical identity before lending any optional direction or geometry metadata.
    fn known_metadata(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<KnownSelection> {
        if self.sealed.get() || self.generation.get() != generation {
            // A changed generation or sealed view has no inheritable native direction or target column.
            return None;
        }
        self.known
            .get()
            .filter(|known| known.selection == selection)
    }

    /// Publish only on the same nonreentrant Core stack. Every publication revokes old captures,
    /// including direction or intent changes whose numerical range remains unchanged.
    pub(super) fn publish(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
        proven_head: bool,
        head_is_upstream: Option<bool>,
        vertical_goal: Option<NativeMarkdownVerticalGoal>,
    ) -> u64 {
        self.generation.set(generation);
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        self.discard_install();
        let known = (proven_head || selection.anchor_utf16 == selection.head_utf16).then_some(
            KnownSelection {
                selection,
                head_is_upstream,
                vertical_goal: vertical_goal.filter(|goal| goal.is_finite()),
                #[cfg(feature = "native-paged-production")]
                // Ordinary input/history cannot inherit the original visual caret side.
                bidi_side: None,
            },
        );
        self.known.set(known);
        self.advance_serial()
    }

    /// Bind the real visual side in the same Core selection commit, without another serial, callback or repair window.
    #[cfg(feature = "native-paged-production")]
    pub(super) fn publish_with_bidi_side(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
        proven_head: bool,
        head_is_upstream: Option<bool>,
        vertical_goal: Option<NativeMarkdownVerticalGoal>,
        bidi_side: Option<NativeMarkdownBidiCaretSide>,
    ) -> u64 {
        let serial = self.publish(
            generation,
            selection,
            proven_head,
            head_is_upstream,
            vertical_goal,
        );
        if let Some(mut known) = self.known.get() {
            known.bidi_side = bidi_side;
            self.known.set(Some(known));
        }
        // This is still the original nonreentrant publication stack; publish advanced the sole serial exactly once.
        serial
    }

    /// New actual input revokes old installation/action authority; current Core direction may still be captured by a new action.
    pub(super) fn revoke_install(&self) {
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        self.discard_install();
        self.advance_serial();
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Revoke only the optional TextKit installation slot, leaving current logical Core facts intact.
    pub(super) fn discard_install(&self) {
        self.install.set(None);
    }

    /// Invalidate old layout coordinates without changing the logical selection endpoints.
    pub(super) fn clear_vertical_goal(&self) {
        if let Some(mut known) = self.known.get() {
            // A layout change preserves anchor/head but cannot apply old line offsets to new geometry.
            known.vertical_goal = None;
            known.head_is_upstream = None;
            #[cfg(feature = "native-paged-production")]
            {
                known.bidi_side = None;
            }
            // The original source/width/style's bidi side cannot authorize a successor set of visual lines.
            self.known.set(Some(known));
        }
    }

    /// Selection-visibility restyling of the same source preserves explicit logical wrap affinity.
    /// Vertical targets and bidi sides depend on retired glyph geometry and cannot survive into the new style's lines.
    #[cfg(feature = "native-paged-production")]
    pub(super) fn clear_style_geometry(&self) {
        if let Some(mut known) = self.known.get() {
            known.vertical_goal = None;
            known.bidi_side = None;
            self.known.set(Some(known));
        }
    }

    /// Unknown selection or external revision loses directional evidence; do not invent an active endpoint from an ordered range.
    pub(super) fn reset(&self, generation: u64) {
        self.generation.set(generation);
        self.known.set(None);
        self.revoke_install();
    }

    /// Permanently revoke this view's future selection authority before clearing its current facts.
    pub(super) fn seal(&self) {
        self.sealed.set(true);
        self.reset(self.generation.get());
    }

    /// Acquire one stack-scoped lease only after receipt validation; replacing its ID prevents an old guard from clearing a successor.
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(super) fn begin_install(
        &self,
        generation: u64,
        serial: u64,
        before: NSRange,
        selection: MarkdownTextSelection,
    ) -> Option<u64> {
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let after = NSRange::new(start, end - start);
        if self.sealed.get()
            || self.generation.get() != generation
            || self.serial.get() != serial
            || !valid_declared_range(before)
            || !valid_declared_range(after)
        {
            // Neither stale authority nor overflowing ranges permit installation. Current values cannot refresh an old receipt.
            return None;
        }
        let id = self.next_install_id.get().checked_add(1)?;
        self.next_install_id.set(id);
        self.install.set(Some(NativeInstall {
            id,
            generation,
            serial,
            before,
            after,
            selection,
        }));
        Some(id)
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Match generation, action serial and installation ID as independent facts of the same lease.
    pub(super) fn install_matches(&self, generation: u64, serial: u64, id: u64) -> bool {
        !self.sealed.get()
            && self.generation.get() == generation
            && self.serial.get() == serial
            && self.install.get().is_some_and(|install| {
                install.id == id && install.generation == generation && install.serial == serial
            })
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Classify a real native notification without accepting an undeclared intermediate range.
    pub(super) fn observe_install(
        &self,
        range: NSRange,
    ) -> Option<NativeMarkdownSelectionInstallObservation> {
        let install = self.install.get()?;
        let declared = self.install_matches(install.generation, install.serial, install.id)
            && valid_declared_range(range)
            && (range == install.before || range == install.after);
        Some(if declared {
            NativeMarkdownSelectionInstallObservation::OwnDeclared
        } else {
            NativeMarkdownSelectionInstallObservation::Foreign
        })
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Require installation identity and complete Core endpoints before the observer consumes its receipt.
    pub(super) fn install_core_is_current(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> bool {
        self.install.get().is_some_and(|install| {
            self.install_matches(generation, install.serial, install.id)
                && install.selection == selection
        })
    }

    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    /// Release this guard's lease only if no reentrant successor has replaced its exact ID and serial.
    pub(super) fn finish_install(&self, serial: u64, id: u64) {
        if self
            .install
            .get()
            .is_some_and(|install| install.serial == serial && install.id == id)
        {
            // An old guard cannot erase a new installation acquired during native reentry.
            self.install.set(None);
        }
    }

    /// Revoke previous captures with a checked monotonic serial; exhaustion is a programming/lifetime error.
    fn advance_serial(&self) -> u64 {
        let serial = self
            .serial
            .get()
            .checked_add(1)
            .expect("selection serial overflow");
        self.serial.set(serial);
        serial
    }
}

/// Validate finite declarations only; an old native before-range never grants current body-read authority.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn valid_declared_range(range: NSRange) -> bool {
    range.location != objc2_foundation::NSNotFound as usize
        && range.location.checked_add(range.length).is_some()
}
