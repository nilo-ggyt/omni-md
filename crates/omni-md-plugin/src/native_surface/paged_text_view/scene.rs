//! Painting/geometry consumes ready same-Source Scenes; deferred edits borrow original Input authority and the sole producer, not another document owner.

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use objc2::{DefinedClass, rc::Retained};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGContext;

mod paint;
use objc2_foundation::{NSPoint, NSRange, NSRect};
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownLayoutTextAuthority, MarkdownTextSelection,
};
pub use paint::*;

use super::{
    NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState, NativeMarkdownPagedTextView,
    NativeMarkdownPagedTextViewError, Origin, Result,
};
use crate::native_surface::paged_input::{NativeMarkdownBidiCaretSide, NativeMarkdownVerticalGoal};

impl NativeMarkdownPagedTextView {
    /// Body revokes active authority after accepting close, then detaches Native graphs before finally closing this View.
    /// That synchronous callback interval has no display Origin; CPU/cursors retire this query only.
    pub(super) fn display_origin(&self) -> Result<Option<Origin>> {
        match self.origin() {
            Ok(origin) => Ok(Some(origin)),
            Err(NativeMarkdownPagedTextViewError::Revoked) => {
                let delegate = self.ivars().delegate.borrow().clone();
                let delegate = delegate.and_then(|delegate| delegate.upgrade());
                // The slot borrow ended; query only the same weak owner's existing accepted-close fact.
                // Neither !current nor absent Scene proves retirement; add no state, successor Source, or retry.
                let retired = delegate.is_some_and(|delegate| delegate.close_accepted());
                if retired {
                    Ok(None)
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                }
            }
            // Handle only bare revocation while acquiring Origin; actual Input and subsequent Scene/font/layer failures still fail.
            // This entry certifies no successful close; the original receipt still verifies Source/font/pixel release.
            Err(cause) => Err(cause),
        }
    }
}

#[derive(Clone)]
/// Each actual Ready publication owns a distinct nonce; a successor with equal document/generation/width cannot impersonate an old callback.
pub struct NativeMarkdownPagedSceneAuthority(Weak<()>);

/// Owned by the sole Scene publisher; weak authority retains neither Scene nor original Source.
pub struct NativeMarkdownPagedSceneNonce(Rc<()>);

impl Default for NativeMarkdownPagedSceneNonce {
    fn default() -> Self {
        Self::new()
    }
}
impl NativeMarkdownPagedSceneNonce {
    #[must_use]
    pub fn new() -> Self {
        Self(Rc::new(()))
    }
    #[must_use]
    pub fn authority(&self) -> NativeMarkdownPagedSceneAuthority {
        NativeMarkdownPagedSceneAuthority(Rc::downgrade(&self.0))
    }
}
impl NativeMarkdownPagedSceneAuthority {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0) && self.0.strong_count() != 0
    }
}

#[derive(Clone)]
/// Only original identities and scalar facts; requests retain no NSEvent, key string, or Native View.
pub struct NativeMarkdownPagedSceneRequest {
    pub document: Weak<RefCell<MarkdownDocumentBuffer>>,
    pub target: crate::native_surface::paged_input::NativeMarkdownPagedTargetDomain,
    pub input: NativeMarkdownPagedInputState,
    pub viewport: NSRect,
    pub bounds_width: f64,
    pub frame_in_parent: NSRect,
    pub geometry_epoch: u64,
    /// Supplied by the virtual-scroll owner; a fixed local visibleRect origin cannot substitute for actual document scrolling.
    pub virtual_viewport: NativeMarkdownPagedVirtualViewport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownPagedVirtualViewport {
    pub document_origin: NSPoint,
    /// The same Native conversion establishes lane X at zero scroll; keep it distinct from visible origin so rulers/insets cannot become horizontal scrolling.
    pub layout_origin_x: f64,
    /// Actual full clip-inset lane width; a bounded Body narrowing at document edges must not change wrapping width.
    pub layout_width: f64,
    pub scroll_epoch: u64,
}

impl NativeMarkdownPagedSceneRequest {
    /// Consumption requires the original document allocation and complete geometry; equal text/generation cannot replace identity.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.document, &other.document)
            && self.target.same(&other.target)
            && self.input == other.input
            && self.viewport == other.viewport
            && self.bounds_width == other.bounds_width
            && self.frame_in_parent == other.frame_in_parent
            && self.geometry_epoch == other.geometry_epoch
            && self.virtual_viewport == other.virtual_viewport
    }
}

#[derive(Clone)]
pub struct NativeMarkdownPagedSceneKey {
    pub request: NativeMarkdownPagedSceneRequest,
    /// Source/Preview use different actual line slots even without styling; mode is geometry identity and cannot be inferred from styles.
    pub presentation_mode: omni_markdown_core::MarkdownPresentationMode,
    pub source: MarkdownLayoutTextAuthority,
    pub nonce: NativeMarkdownPagedSceneAuthority,
}
impl NativeMarkdownPagedSceneKey {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.request.same(&other.request)
            && self.presentation_mode == other.presentation_mode
            && self.source == other.source
            && self.nonce.same(&other.nonce)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Semantic selection actions already interpreted by the system; these add no physical shortcuts or Host registrations.
pub enum NativeMarkdownPagedNavigation {
    Backward,
    Forward,
    Left,
    Right,
    Up,
    Down,
    WordBackward,
    WordForward,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    LineLeft,
    LineRight,
    ParagraphStart,
    ParagraphEnd,
    DocumentStart,
    DocumentEnd,
    PageUp,
    PageDown,
    SelectWord,
    SelectLine,
    SelectParagraph,
}

/// Keep logical/visual Word directions distinct across asynchronous queues; do not collapse original selector semantics into one boolean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedWordMotion {
    Backward,
    Forward,
    Left,
    Right,
}

impl NativeMarkdownPagedWordMotion {
    /// Only logical actions directly specify Source direction; visual actions require actual layout or a complete level-zero proof.
    pub(crate) fn logical_forward(self) -> Option<bool> {
        match self {
            Self::Backward => Some(false),
            Self::Forward => Some(true),
            Self::Left | Self::Right => None,
        }
    }

    /// Only Left/Right specify screen direction; RTL logical order cannot reconstruct the original system action.
    pub(crate) fn visual_right(self) -> Option<bool> {
        match self {
            Self::Left => Some(false),
            Self::Right => Some(true),
            Self::Backward | Self::Forward => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Extension retains original logical anchor; word drag retains actual initial grapheme group/hit side, not events/text.
pub enum NativeMarkdownPagedWordAnchor {
    Selection(usize),
    /// Use the actual initial hit only until its first group is Ready, then replace it with that original query's Group.
    Word {
        head_utf16: usize,
        backward_bias: bool,
        /// A table hit must reacquire actual same-row/column Cell Source; its canonical head cannot enter Body UBA.
        cell: Option<super::NativeMarkdownPagedCellWordTarget>,
    },
    /// Core's completed initial group retains both endpoints across drag/commit; do not expand its seed again in successor display.
    Group {
        from_utf16: usize,
        to_utf16: usize,
        /// The initial hit decides direction within that group, not a new word query; map it through the same actual commit.
        origin_utf16: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Actual source coordinates for one Native word-selection request; the same SceneRequest/permit constrains original document/Input/geometry.
pub struct NativeMarkdownPagedWordSelection {
    pub head_utf16: usize,
    pub backward_bias: bool,
    pub anchor: Option<NativeMarkdownPagedWordAnchor>,
    /// Cell query intent for the latest target only; Ready initial groups retain no Source, while the initial query keeps its own Cell domain.
    pub cell: Option<super::NativeMarkdownPagedCellWordTarget>,
}

impl NativeMarkdownPagedNavigation {
    /// As in the original Native path, only consecutive linewise Up/Down retains the initial actual caret goal column.
    #[must_use]
    pub fn preserves_vertical_goal(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// Upstream/downstream describes soft-wrap affinity only; separate level/logical-grapheme-side facts identify the Bidi part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedHeadAffinity {
    Upstream,
    Downstream,
}
impl NativeMarkdownPagedHeadAffinity {
    #[must_use]
    pub fn from_upstream(upstream: bool) -> Self {
        if upstream {
            Self::Upstream
        } else {
            Self::Downstream
        }
    }
    #[must_use]
    pub fn is_upstream(self) -> bool {
        self == Self::Upstream
    }
}

#[derive(Clone)]
/// The original producer's prepared layout coordinates: layout_epoch changes with Source, font/style, and layout,
/// not with View geometry_epoch or virtual scroll_epoch that changes on every clip scroll.
pub struct NativeMarkdownPagedNavigationGeometry {
    pub source: MarkdownLayoutTextAuthority,
    /// The actual composer line-width key; window bounds cannot stand in for text-container width.
    pub width_key: i64,
    pub layout_epoch: u64,
}
impl NativeMarkdownPagedNavigationGeometry {
    /// Compare bounded coordinates from the same producer without acquiring new Source/layout to renew old navigation.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.source == other.source
            && self.width_key == other.width_key
            && self.layout_epoch == other.layout_epoch
    }
    /// Source must match original SceneKey and positive width comes from actual layout; the original Scene guard still checks liveness.
    #[must_use]
    pub fn permits(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        self.source == key.source && self.width_key > 0
    }
    /// Inherit goal column only for equal container width/layout generation; scroll-event generation has different semantics.
    #[must_use]
    pub fn permits_goal(&self, goal: NativeMarkdownVerticalGoal) -> bool {
        goal.width_key() == self.width_key && goal.layout_epoch() == self.layout_epoch
    }
}

/// Facts from Input's sole selection slot, without another cache, history, or Native object.
pub struct NativeMarkdownPagedNavigationContext {
    pub geometry: NativeMarkdownPagedNavigationGeometry,
    pub head_affinity: Option<NativeMarkdownPagedHeadAffinity>,
    pub vertical_goal: Option<NativeMarkdownVerticalGoal>,
    pub bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

/// The original Scene's complete target for one directed selection; a short-line head column must not replace vertical_goal.
pub struct NativeMarkdownPagedNavigationTarget {
    pub selection: MarkdownTextSelection,
    pub head_affinity: Option<NativeMarkdownPagedHeadAffinity>,
    pub vertical_goal: Option<NativeMarkdownVerticalGoal>,
    pub bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedEditCommand {
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    DeleteToLineStart,
    DeleteToLineEnd,
    InsertNewline,
    InsertTab,
    InsertBacktab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Client actions already interpreted by the original field; no new keys, and the controller must consume original authority/client checks.
pub enum NativeMarkdownPagedCellAction {
    Next,
    Previous,
    NextRow,
    Cancel,
}

#[derive(Clone)]
/// Actual Cell hit facts: original row/raw authority owns Source, and the content frame comes from measured rows, not paginated suffix height.
/// Retain no Source text, document, View, or event; the sole Body rechecks the same request before deferred activation.
pub struct NativeMarkdownPagedCellHit {
    pub row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    pub source: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource,
    pub content_frame: NSRect,
    pub head_utf16: usize,
    pub head_affinity: NativeMarkdownPagedHeadAffinity,
    pub backward_bias: bool,
    /// Only the same Grid word hit supplies this; ordinary caret/Tab cannot impersonate acquired word-query Source.
    pub word_target: Option<super::NativeMarkdownPagedCellWordTarget>,
}

#[derive(Clone)]
/// One interpreted Cell pointer intent; weak gesture identity means original Down has not ended, not that asynchronous Ready may revive dragging.
/// Shift/double-click seeds retain their original directed origins, not replayable events or a second selection cache.
pub struct NativeMarkdownPagedCellPointer {
    pub hit: NativeMarkdownPagedCellHit,
    pub clicks: usize,
    pub extending: bool,
    pub anchor: Option<usize>,
    pub word_anchor: Option<NativeMarkdownPagedWordAnchor>,
    pub dragging: bool,
    pub intent: Weak<()>,
}

#[derive(Clone, Copy)]
/// Local seed established by child Down; preserve Word rather than reduce it to a caret, and retain Shift's directed anchor.
pub enum NativeMarkdownPagedCellDragAnchor {
    Caret { anchor_utf16: usize },
    Word(NativeMarkdownPagedWordAnchor),
}

#[derive(Clone)]
/// One decoded finite endpoint from the current Native gesture; convert its window point only after original Scene readiness.
/// Only actual Up that still owns original Down sets mouse_up. An accepted endpoint may finish without renewing drag lifetime.
/// Retain no event, View, text, or second Input permit; original input domain/generation/geometry remain in this same request.
pub struct NativeMarkdownPagedPointerLocation {
    pub window_point: NSPoint,
    pub clicks: usize,
    pub extending: bool,
    pub anchor: Option<NativeMarkdownPagedCellDragAnchor>,
    pub dragging: bool,
    pub intent: Weak<()>,
    pub mouse_up: bool,
}

#[derive(Clone)]
/// Actual Cell-to-Body extension: word drag retains its seed, ordinary selection its directed endpoints/wrap affinity.
/// The sole parent Body publishes the draft then maps through that actual change; an old Scene cannot directly edit canonical text.
pub enum NativeMarkdownPagedCellExtension {
    Selection {
        selection: MarkdownTextSelection,
        head_affinity: NativeMarkdownPagedHeadAffinity,
        intent: Weak<()>,
    },
    Word {
        selection: NativeMarkdownPagedWordSelection,
        intent: Weak<()>,
    },
}

pub struct NativeMarkdownPagedEditPlan {
    pub replacement: NSRange,
    /// Newly supplied text only; never copy the whole document or an offscreen paragraph into the plan.
    pub inserted: String,
}

pub struct NativeMarkdownPagedCharacterRect {
    /// Original Body View-local coordinates; only the actual View/window holding this authority can convert to screen space.
    pub rect: NSRect,
    pub actual_range: NSRange,
}

/// Implementations return Ready bounded line/hit/paint results without synchronous shaping or full-paragraph layout in these methods.
/// paint uses the sealed-spool painter with original Source/font/output nonce to draw actual glyphs while preserving background,
/// selection/decorations. A cache hit alone is not success, and an empty image cannot stand in for nonempty text.
pub trait NativeMarkdownPagedTextScene {
    /// Return this actual publication's complete identity, including original
    /// Input/geometry/Source and the live publication nonce, without recapture.
    fn key(&self) -> &NativeMarkdownPagedSceneKey;

    /// Enumerate current sealed/clipped link-label geometry only, without URL lookup, parser progress, or a link index.
    /// The caller independently proves full CPU publication; visit collects Copy facts, with Native registration after returning Scene.
    fn visit_link_cursor_rects(&self, _visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        Ok(())
    }

    /// Hit-testing reads sealed visible LINK parts/original grapheme boundaries only; mouse-down does not parse/open URLs.
    fn link_hit(&self, _local: NSPoint) -> Result<Option<(std::ops::Range<usize>, usize)>> {
        Ok(None)
    }
    /// Mouse-up admits weak original gesture authority; success means the sole Body accepted intent, with actual Source still needed for target preparation.
    fn follow_link(&self, _index: usize, _intent: Weak<()>) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Resolve same-document headings through this Scene's original Body owner without requiring a global workspace Surface.
    /// Success admits the original click query; the new selection's SourceTarget and complete CPU frame still establish visibility.
    fn follow_heading(&self, _fragment: &str, _intent: Weak<()>) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Only the actual link producer returns a parsed hit target; ordinary Source/Cell cannot guess URLs from visible text.
    fn link_target(&self, _local: NSPoint) -> Result<Option<String>> {
        Ok(None)
    }
    /// One notification after full CPU publication and actual count increase, granting no extra Source/Native visibility authority.
    /// Queue only weak successor notification; after returning the paint stack recheck original key/actual publication count.
    fn cpu_frame_published(&self, _paints: usize) {
        // A Scene without an observer stores no extra state; default behavior queues/reads/reenters nothing.
    }
    /// Check actual original Source/font/output health; even an empty glyph read uses the reader authority gate.
    /// Run current/Native outside borrows with Source-error priority; this method performs no layout, text reads, or image allocation.
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()>;
    /// Return measured line/caret geometry in this Scene's View-local coordinates;
    /// actual_range states which original UTF-16 source positions that rect covers.
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect>;
    /// Resolve a point through the same sealed glyph/grapheme data; unprepared
    /// positions remain Pending instead of being approximated from source length.
    fn hit(&self, local: NSPoint) -> Result<usize>;
    /// Dragging may leave the physical viewport; only the actual window owner projects to sealed nearest lines while retaining the physical point.
    fn drag_selection_point(&self, local: NSPoint) -> Result<NSPoint> {
        Ok(local)
    }
    /// Double-click consumes actual glyph hit/click side; a source offset alone cannot choose which group touches a word boundary.
    fn word_hit(
        &self,
        _local: NSPoint,
    ) -> Result<(
        usize,
        bool,
        Option<super::NativeMarkdownPagedCellWordTarget>,
    )> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Deliver this actual Grid hit to the original Cell owner; word requests need glyph hit side, not word-range selection here.
    fn cell_hit(&self, _local: NSPoint, _word: bool) -> Result<Option<NativeMarkdownPagedCellHit>> {
        Ok(None)
    }
    /// Triple-click/menu paragraph selection consumes verified Source paragraphs rather than infer them from visual wraps/spaces.
    fn paragraph_selection(
        &self,
        _position: usize,
        _affinity: NativeMarkdownPagedHeadAffinity,
    ) -> Result<MarkdownTextSelection> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Interpret the actual click granularity and optional directed anchor using
    /// this Scene only; callers retain the original gesture and Input permit.
    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<(MarkdownTextSelection, NativeMarkdownPagedHeadAffinity)>;
    /// Without actual layout-coordinate production remain Pending; Ready painting alone grants no navigation authority.
    fn navigation_geometry(&self) -> Result<NativeMarkdownPagedNavigationGeometry> {
        Err(super::NativeMarkdownPagedTextViewError::Pending)
    }
    /// Resolve semantic motion using the supplied original layout/affinity/goal;
    /// a short line cannot overwrite the established vertical target column.
    fn navigate(
        &self,
        action: NativeMarkdownPagedNavigation,
        extend: bool,
        context: &NativeMarkdownPagedNavigationContext,
    ) -> Result<NativeMarkdownPagedNavigationTarget>;
    /// Prepare one complete source replacement from certified grapheme/line facts;
    /// inserted contains new text only and does not materialize replaced content.
    fn edit_plan(
        &self,
        action: NativeMarkdownPagedEditCommand,
    ) -> Result<NativeMarkdownPagedEditPlan>;
    /// Begin consuming paint for the original CPU candidate; implementations without a real incremental producer explicitly return Pending.
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>>;
    /// Synchronous painting obeys the same Source/font/output health and current
    /// guards; its return grants no CPU tile-completion authority by itself.
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()>;
}

/// The composition root implements this weak delegate. current may reenter, so View ends borrows and revalidates original facts around each call.
/// ready_scene queries published output only; unfinished work is Pending without retaining/replaying the Native event.
pub trait NativeMarkdownPagedTextViewDelegate {
    /// If parent Body owns an active Cell, context-menu actions still belong to that client; ordinary Body creates no substitute input View.
    fn context_menu_client(
        &self,
    ) -> Result<Option<Rc<Retained<super::NativeMarkdownPagedTextView>>>> {
        Ok(None)
    }
    fn current(&self, epoch: u64) -> bool;
    /// Report actual accepted-close only; current=false, absent Scene, or preparation does not imply exit.
    /// Non-Body clients have no Body close transaction and default to false; this certifies no input, paint, or release success.
    fn close_accepted(&self) -> bool {
        false
    }
    /// bounds_width is this bounded Body's actual width; standalone components without scrolling use it directly, not a default width.
    fn virtual_viewport(
        &self,
        epoch: u64,
        bounds_width: f64,
    ) -> Result<NativeMarkdownPagedVirtualViewport>;
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>>;
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool;
    /// Actual scrollWheel revokes old mode positioning then follows NSView's responder chain; standalone Scenes have no mode authority to revoke.
    fn user_scroll(&self, _epoch: u64) -> Result<()> {
        Ok(())
    }
    /// After the same Down leaves the viewport, the scroll owner moves bounded distance toward its window endpoint without reading/editing text.
    fn drag_autoscroll(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _point: NSPoint,
        _intent: &Weak<()>,
    ) -> Result<bool> {
        Ok(false) // A standalone client without a scroll owner has no scrollable graph; do not guess through parent responders.
    }
    /// Notify the existing coalesced scheduler only, without paragraph-wide layout or silent canonical replacement in input callbacks.
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// After an endpoint selection commits, admit only this receipt's display intent; the sole Body producer performs actual Source binding
    /// and scrolling. Admission is not Ready and cannot authorize replay of the committed selection.
    fn reveal_committed_selection(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// After table insertion, actual Preview Body may admit first-cell activation; standalone Source retains its existing caret reveal.
    /// Borrow the receipt only on this stack; retain no old Scene, repeat no insertion, and derive no activation from a newer selection.
    fn activate_inserted_table(&self, _commit: &NativeMarkdownPagedInputCommit) -> Result<()> {
        Ok(()) // Original Source/standalone consumers without table editing graphs have no Cell to mount.
    }
    /// Body may admit an actual vertical target for deferred layout; true means admission, not completed selection movement.
    /// Other clients retain synchronous Scene navigation without another Body or a Ready claim for unprepared results.
    fn navigate_vertical(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _action: NativeMarkdownPagedNavigation,
        _extend: bool,
    ) -> Result<bool> {
        Ok(false)
    }
    /// The Body/Cell owner may admit logical word deletion at an empty caret; the Ready range must consume this same Input permit.
    /// Clients without a word producer reject explicitly, without scalar approximation or a temporary second Source.
    fn delete_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _forward: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// A Body may enqueue one visual intent; false retains direct navigation for an already prepared finite Scene.
    fn move_visual(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _right: bool,
        _extend: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Keep system logical/visual Word intent; the original Body consumer decides whether actual layout is needed.
    fn move_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _motion: NativeMarkdownPagedWordMotion,
        _extend: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Body/Cell resolves logical word selection incrementally; admission is not committed selection or mixed-Bidi visual-motion authority.
    fn select_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _selection: NativeMarkdownPagedWordSelection,
        _gesture: Option<Weak<()>>,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Retain the original key before interpretation, keeping marked callbacks synchronous; false retains the existing direct keyboard path.
    fn defer_key_down(
        &self,
        _event: &objc2_app_kit::NSEvent,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Only public menu actions queue behind a real barrier, without capturing new Source or creating another event graph.
    fn has_native_input_barrier(&self) -> bool {
        false
    }
    /// Paste carries bounded new text captured on arrival; other actions retain their original typed selector only.
    fn defer_native_command(
        &self,
        _selector: objc2::runtime::Sel,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Paths/original mouse hit enter the existing input queue; a None anchor pastes after the preceding actual selection completes.
    /// The drop tuple is original canonical generation plus actual Scene-hit UTF-16 position, not successor coordinates.
    fn defer_attachments(
        &self,
        _paths: Vec<std::path::PathBuf>,
        _drop: Option<(u64, usize)>,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Prequeue URL/batch rejection waits for the original chain before display; true admits this bounded feedback only.
    fn defer_attachment_rejection(&self, _message: &str) -> bool {
        false
    }
    /// Queue formatting after admitted navigation; clients without a barrier retain synchronous transactions.
    fn defer_format(
        &self,
        _action: omni_markdown_core::MarkdownFormatAction,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Retain table-insertion semantics rather than generate/insert Markdown prematurely at an old caret.
    fn defer_table_insertion(
        &self,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Queue fold-menu/gutter intent after unfinished navigation; resolve the actual current paragraph only at queue head.
    fn defer_source_fold(
        &self,
        _action: super::NativeMarkdownPagedSourceFoldAction,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Only this interpretation callback's original before-state and this Rc receipt can hand off successor-event authority.
    fn begin_interpreted_commit(
        &self,
        _before: NativeMarkdownPagedInputState,
        _commit: Rc<NativeMarkdownPagedInputCommit>,
    ) -> Result<bool> {
        Ok(false)
    }
    /// After admitting this same receipt, finish display/permit handoff without reading successor state to renew authority.
    fn finish_interpreted_commit(
        &self,
        _commit: &NativeMarkdownPagedInputCommit,
        _reveal: bool,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::Revoked)
    }
    /// Interpret document endpoints after the original Word/Visual chain; true means admission, not moved selection or visible EOF.
    fn defer_document_motion(
        &self,
        _end: bool,
        _extend: bool,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // Clients without an input chain retain synchronous endpoint semantics without a replacement queue.
    }
    /// Retain arrival order for existing Body commands; actual continuation/indent consumes a Core transaction after the preceding receipt.
    fn defer_source_command(
        &self,
        _command: omni_markdown_core::MarkdownEditingCommand,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // Clients without the original input chain remain synchronous; Cell cannot use this entry to invent table navigation.
    }
    /// Queue ordinary backward/forward deletion after incrementally prepared motion; false retains the existing synchronous path.
    /// Admission records direction only; the same Source after preceding actual commit determines complete grapheme/marker commands.
    fn defer_grapheme_delete(
        &self,
        _forward: bool,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // Clients without an active input chain create no replacement queue and do not alter deletion semantics.
    }
    /// During word deletion Body may admit later ordinary input; borrow payload synchronously and retain direct input on false.
    /// The admitting owner copies only bounded new payload in order, retaining no Native object or old permit for replay.
    fn defer_insert(
        &self,
        _text: &objc2::runtime::AnyObject,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // A Scene without a deferred-input owner creates no replacement queue or second Input.
    }
    /// Plain Page is scrolling only; Source owner waits for actual next-page height before system scrolling, without committing selection.
    fn scroll_page(&self, _request: &NativeMarkdownPagedSceneRequest, _down: bool) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// The composing child Cell consumes this actual mouse event synchronously, without storage or delayed delivery after parent focus transfer.
    fn handle_cell_marked_mouse(&self, _event: &objc2_app_kit::NSEvent) -> Result<bool> {
        Ok(false)
    }
    /// Out-of-bounds child Drag borrows the original event only synchronously; parent Body hits its current Scene without replaying mouse methods.
    fn drag_cell_pointer_outside(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedPointerLocation,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Selection beyond active Cell follows the original parent/child commit boundary; true transfers ownership and forbids editing through old Scene.
    fn extend_cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _extension: NativeMarkdownPagedCellExtension,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Actual parent Body may admit Cell pointer intent; only true stops ordinary Body selection, while other clients retain Source behavior.
    fn cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedCellPointer,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// While Scene prepares, admit only the same caret gesture's latest actual window endpoint; false explicitly means not admitted.
    /// Admit actual Up before View revokes Down authority; afterward do not infer endpoint/cancellation from weak liveness.
    fn defer_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedPointerLocation,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// New Down/key/command/close revokes old admitted endpoints without acquiring Scene or revoking other Cell navigation.
    fn cancel_deferred_pointer(&self) -> bool {
        false
    }
    /// Original Up ends an existing gesture only; even during Scene preparation do not reopen a Cell or synthesize Down.
    fn finish_cell_pointer(&self, _request: &NativeMarkdownPagedSceneRequest) -> Result<()> {
        Ok(())
    }
    /// Cell without its RequiredRow/BorrowedEditor controller remains Pending, without inserting Tab/newline or publishing Body text instead.
    fn cell_action(
        &self,
        _action: NativeMarkdownPagedCellAction,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::Pending)
    }
    /// Only explicit Copy/Cut uses the existing clipboard producer; View may Cut with original authority only after successful copy.
    /// The producer preserves complete original selection/Source; truncated output must never masquerade as complete copy.
    fn copy_selection(&self, request: &NativeMarkdownPagedSceneRequest) -> Result<()>;
}
