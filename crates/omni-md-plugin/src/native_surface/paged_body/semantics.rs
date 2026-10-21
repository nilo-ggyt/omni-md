//! RootLeaf font integration for the same Body Input. Consume bounded original publications without changing canonical syntax or coordinates for visibility.

use std::{ops::Range, ptr::NonNull, rc::Rc};

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSLayoutManager};
use objc2_core_foundation::CFRetained;
use objc2_core_text::CTFont;
use omni_markdown_core::{
    MarkdownLayoutTextAuthority, MarkdownLayoutTextSource, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineStyleRequest, MarkdownOrdinarySemanticState,
    MarkdownPresentationStyles, MarkdownTableCellDisplaySource, MarkdownTextSelection,
    MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest, MarkdownViewportSourceStep,
    VerifiedMarkdownLayoutTableRow, VerifiedMarkdownRootLeafStyleBoundary,
    VerifiedMarkdownViewportSources,
};

use omni_markdown_core::markdown_document_buffer::{MarkdownTableRequestError, SemanticWork};
use omni_markdown_core::markdown_viewport_presentation::table_window::MarkdownTableWindowError;

use super::preparation::PreparationTicket;
use super::{NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error, Result};
use crate::native_surface::{
    paged_fonts::{NativeMarkdownFontCapture, NativeMarkdownPagedFont},
    paged_line_fonts::NativeMarkdownLineFont,
    text_attributes::{MarkdownTextAttributePalette, native_markdown_base_font},
};

const WINDOW_UTF16: usize = 16_384;
const QUANTUM: usize = 4096;

/// Base roles preserve the existing TextKit font-selection priority; weight, italic, and subscript use fixed slots independent of text length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    Body,
    Heading(usize),
    Code,
    Table,
    Hidden,
}
impl Role {
    fn from_styles(styles: MarkdownPresentationStyles) -> Self {
        // Visibility comes from actual selection-bound semantics and must precede heading/code roles so markers cannot borrow their larger fonts.
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER) {
            return Self::Hidden;
        }
        for (index, heading) in [
            MarkdownPresentationStyles::HEADING_1,
            MarkdownPresentationStyles::HEADING_2,
            MarkdownPresentationStyles::HEADING_3,
            MarkdownPresentationStyles::HEADING_4,
            MarkdownPresentationStyles::HEADING_5,
            MarkdownPresentationStyles::HEADING_6,
        ]
        .into_iter()
        .enumerate()
        {
            if styles.contains(heading) {
                return Self::Heading(index);
            }
        }
        if styles.contains(MarkdownPresentationStyles::TABLE_HEADER)
            || (styles.contains(MarkdownPresentationStyles::TABLE)
                && styles.contains(MarkdownPresentationStyles::STRONG))
        {
            return Self::Table;
        }
        if styles.contains(MarkdownPresentationStyles::INLINE_CODE)
            || styles.contains(MarkdownPresentationStyles::FENCED_CODE)
        {
            Self::Code
        } else {
            Self::Body // Certified rules and other ordinary roles retain literal base-font behavior, without claiming background or hidden-preview completion.
        }
    }
    /// Merge only combinations with identical actual font roles. Headings are already bold and code uses its mono face; Body slots must not erase italics.
    fn slot(self, styles: MarkdownPresentationStyles) -> Option<usize> {
        if self == Self::Hidden {
            return Some(40); // Hidden fonts inherit no bold, italic, or subscript size, retaining their actual 0.1-point identity.
        }
        let italic = usize::from(styles.contains(MarkdownPresentationStyles::EMPHASIS));
        let base = match self {
            Self::Body => {
                usize::from(styles.contains(MarkdownPresentationStyles::STRONG)) + italic * 2
            }
            Self::Heading(index) => 4 + index + italic * 6,
            Self::Code => 16 + italic,
            Self::Table => 18 + italic,
            Self::Hidden => unreachable!("隐藏角色已先返回独占槽"),
        };
        let subscript = styles.contains(MarkdownPresentationStyles::SUBSCRIPT)
            && !styles.contains(MarkdownPresentationStyles::MARKER);
        let slot = base + usize::from(subscript) * 20;
        (slot != 0).then_some(slot) // Ordinary Body reuses registered face zero; other roles occupy at most forty fixed slots.
    }
}

/// The existing attribute policy selects actual NSFonts; established exact-face/axis capture still owns Native/Core registration.
pub(super) struct Typography {
    palette: MarkdownTextAttributePalette,
    slots: [Option<usize>; 41],
    /// Uses only public default-line metrics, without storage, container, strings, or another document graph.
    pub(super) metrics_owner: Rc<Retained<NSLayoutManager>>,
}
impl Typography {
    pub(super) fn new(font: &CTFont) -> Result<Self> {
        // SAFETY: The live CTFont remains valid here; retain acquires an independent +1, and Apple defines the NSFont/CTFont toll-free bridge.
        let retained = unsafe { CFRetained::retain(NonNull::from(font)) };
        let retained: Retained<CTFont> = retained.into();
        // SAFETY: The actual CTFont and NSFont are the same object; change only the Rust view while preserving the original +1 claim.
        let normal: Retained<NSFont> = unsafe { Retained::cast_unchecked(retained) };
        Ok(Self {
            // Role creation failure aborts assembly; do not publish a partial palette missing heading/code fonts.
            palette: MarkdownTextAttributePalette::new(normal)?,
            slots: [None; 41],
            metrics_owner: Rc::new(NSLayoutManager::new()),
        })
    }
}

/// An in-flight trial retains original style, coverage, and publication. A changed selection cannot renew its old result.
pub(super) struct Selection {
    pub(super) coverage: Range<usize>,
    pub(super) style: MarkdownLineComposeStyle,
    role: Role,
    styles: MarkdownPresentationStyles,
    /// Closing EOL owns only the original physical separator ticket, never inline or hidden-syntax authority.
    fence_separator: bool,
    scope: SelectionScope,
    source: MarkdownLayoutTextAuthority,
}

/// Literal Source, Body semantics, and cell display directories are distinct authorities; equal numeric ranges cannot renew another domain.
enum SelectionScope {
    Literal,
    Body(
        Rc<VerifiedMarkdownViewportSources>,
        MarkdownViewportSourceAuthority,
    ),
    Cell(Rc<MarkdownTableCellDisplaySource>, MarkdownTextSelection),
}

/// A structural table row transfers through the actual publication, rather than becoming an ordinary font-styled root paragraph.
pub(super) enum StyleDecision {
    Font(Selection),
    Table(VerifiedMarkdownLayoutTableRow),
}
impl Selection {
    /// Expose styles already bound to the original publication; geometry cannot expand Source coverage or reinterpret Markdown.
    pub(super) fn styles(&self) -> MarkdownPresentationStyles {
        self.styles
    }

    /// The left-font response uses this Selection's original publication to certify a semantic seam, without a newer window or another Parser.
    /// The ticket permits the original Composer to try the right GCB; that same complete trial still determines actual font change.
    pub(super) fn style_boundary(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Option<VerifiedMarkdownRootLeafStyleBoundary>> {
        self.require(owner, source, request)?;
        let SelectionScope::Body(publication, authority) = &self.scope else {
            return Ok(None); // Literal/cell display owns no RootLeaf authority; equal coordinates cannot certify a Body style boundary.
        };
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf样式沿原Body文档")
            .clone();
        Ok(source.verified_root_leaf_style_boundary(
            &document.borrow(),
            publication.clone(),
            *authority,
            request,
        )?)
    }

    /// The right-font trial consumes Core's original slot ticket and returns metadata to its original Body scope with per-Native-call checks intact.
    /// This entry neither advances SourceRequest, changes left coverage, nor appends the right grapheme to Body output.
    pub(super) fn root_leaf_boundary(
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        boundary: &VerifiedMarkdownRootLeafStyleBoundary,
        ticket: &PreparationTicket,
    ) -> Result<Self> {
        ticket.require_pure(owner, source)?;
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf右字素沿原Body文档")
            .clone();
        let (query, publication, authority) =
            boundary.right_query_for(&document.borrow(), source, request)?;
        let styles = query.styles;
        Ok(Self {
            coverage: request.source_range(),
            style: MarkdownLineComposeStyle {
                presentation: styles,
                points: 0.0, // base_index supplies the actual semantic font size, not the preceding emoji or hidden font's size.
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(styles),
            styles,
            fence_separator: false,
            scope: SelectionScope::Body(publication, authority),
            source: source.authority(),
        })
    }
    pub(super) fn literal(
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        points: f32,
    ) -> Self {
        Self::literal_range(source, request.source_range(), points)
    }
    /// Actual blank controls have layout but no hideable syntax; keep their ordinary font under the same frozen Source.
    fn literal_range(
        source: &MarkdownLayoutTextSource,
        coverage: Range<usize>,
        points: f32,
    ) -> Self {
        Self {
            coverage,
            style: MarkdownLineComposeStyle {
                presentation: MarkdownPresentationStyles::default(),
                points,
                language: None,
                features: Vec::new(),
            },
            role: Role::Body,
            styles: MarkdownPresentationStyles::default(),
            fence_separator: false,
            scope: SelectionScope::Literal, // Source mode owns no semantic authority and constructs no placeholder capability.
            source: source.authority(),
        }
    }
    pub(super) fn context_changed(&self, owner: &Owner) -> bool {
        let selection = match &self.scope {
            SelectionScope::Literal => return false,
            SelectionScope::Body(_, authority) => authority.selection,
            SelectionScope::Cell(_, selection) => *selection,
        };
        owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf只绑定原Body文档")
            .borrow()
            .selection()
            != selection
    }

    /// Consume the intersection of this actual cell display directory and style request, without rereading raw Markdown or borrowing Body ranges.
    pub(super) fn cell(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        request: &MarkdownLineStyleRequest,
    ) -> Result<Self> {
        Self::cell_range(owner, display, request.source(), request.source_range())
    }

    /// Cell line breaks and empty trailing lines keep their display Source; controls cannot read adjacent cells or assume ordinary Body styling.
    pub(super) fn cell_control(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        request: &omni_markdown_core::MarkdownLineInlineControl,
    ) -> Result<Self> {
        Self::cell_range(
            owner,
            display,
            request.source(),
            request.source_utf16.clone(),
        )
    }

    /// Styles/controls share the actual display directory. Empty EOF authorizes its original empty range, never default styling of unknown content.
    fn cell_range(
        owner: &Owner,
        display: Rc<MarkdownTableCellDisplaySource>,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<Self> {
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格显示属于原Body")
            .clone();
        let document = document.borrow();
        display.require_for(&document)?;
        if authority != &display.authority() {
            // Equal offsets cannot renew another display Source; the old cell callback must stop here.
            return Err(super::ViewError::Revoked.into());
        }
        let (end, styles) = if range.is_empty() && range.start == display.source().utf16_len() {
            // An actual empty trailing control consumes no text; default style applies only to that empty range.
            (range.end, MarkdownPresentationStyles::default())
        } else {
            // Style authority ends at actual directory coverage; later style changes require the next original NeedStyle.
            let query = display
                .styles_at(range.start)
                .map_err(super::ViewError::Document)?;
            let Some(query) = query else {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_unsupported phase=style_missing range_start={} range_end={} display_utf16={}",
                    range.start,
                    range.end,
                    display.source().utf16_len(),
                );
                return Err(Error::Unsupported);
            };
            let end = range.end.min(query.utf16_range.end);
            if end <= range.start {
                // Uncovered nonempty text has unavailable semantics; a default Body font cannot silently substitute.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_unsupported phase=style_empty range_start={} range_end={} covered_end={}",
                    range.start, range.end, query.utf16_range.end,
                );
                return Err(Error::Unsupported);
            }
            (end, query.styles)
        };
        Ok(Self {
            coverage: range.start..end,
            style: MarkdownLineComposeStyle {
                presentation: styles,
                points: 0.0,
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(styles),
            styles,
            fence_separator: false,
            source: display.authority(),
            scope: SelectionScope::Cell(display, document.selection()),
        })
    }
    pub(super) fn require(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
    ) -> Result<()> {
        self.require_source(owner, source, request.source())
    }
    /// Original controls and font requests share Source validation; neighboring parts cannot grant controls another style.
    pub(super) fn require_control(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &omni_markdown_core::MarkdownLineInlineControl,
    ) -> Result<()> {
        self.require_source(owner, source, request.source())
    }
    fn require_source(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<()> {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source || authority != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        if let SelectionScope::Cell(display, _) = &self.scope {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("格语义持原Body文档")
                .clone();
            display.require_for(&document.borrow())?;
            if !self.coverage.is_empty() {
                // Native font preparation may span event-loop turns; recheck original directory coverage/style before responding.
                let query = display
                    .styles_at(self.coverage.start)
                    .map_err(super::ViewError::Document)?
                    .ok_or(Error::Unsupported)?;
                if query.utf16_range.end < self.coverage.end || query.styles != self.styles {
                    return Err(super::ViewError::Revoked.into());
                }
            } else if self.coverage.start != source.utf16_len() {
                // An empty control must be this Source's actual EOF, not an empty style ticket for unknown content.
                return Err(super::ViewError::Revoked.into());
            }
        }
        if let SelectionScope::Body(publication, authority) = &self.scope {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("RootLeaf只绑定原Body文档")
                .clone();
            let document = document.borrow();
            if self.fence_separator {
                if publication.root_fence_separator_for(&document, *authority, &self.coverage)?
                    != Some(self.coverage.clone())
                {
                    return Err(super::ViewError::Revoked.into());
                }
                return Ok(()); // Revalidate the same publication around font callbacks without changing ordinary-leaf coverage or semantic state.
            }
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) =
                publication.ordinary_semantics_for(&document, *authority)?
            else {
                return Err(Error::Unsupported);
            };
            let query = leaf
                .query_for(&document, *authority, self.coverage.start)?
                .ok_or(Error::Unsupported)?;
            if query.utf16_range.end < self.coverage.end
                || query.styles != self.styles
                || Role::from_styles(query.styles) != self.role
            {
                return Err(super::ViewError::Revoked.into());
            }
        }
        Ok(()) // PreparationTicket separately checks Input/width/selection around Native; numeric publication facts cannot reconstruct Source authority.
    }
    pub(super) fn base_index(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        ticket: &PreparationTicket,
    ) -> Result<usize> {
        let Some(slot) = self.role.slot(self.styles) else {
            // Ordinary Body already owns palette zero; no role-specific font
            // construction or additional registration is needed for this run.
            return Ok(0);
        };
        let cached = owner
            .typography
            .borrow()
            .as_ref()
            .expect("InlinePreview持活动字体角色策略")
            .slots[slot];
        if let Some(index) = cached {
            // This slot names an actually registered font in this Body's current
            // palette. Copy its real size into the response, not a guessed role size.
            self.style.points = owner.fonts.borrow()[index].native().point_size() as f32;
            return Ok(index);
        }
        // Take the policy out before Native entry so no RefCell borrow spans NSFont/current callbacks; original preparation admission serializes close.
        let mut typography = owner
            .typography
            .borrow_mut()
            .take()
            .expect("活动策略只有本次装配能借出");
        let result = (|| {
            ticket.require(owner, source)?;
            // Pass the complete semantic run to the existing policy; hidden markers use the original palette's actual small size,
            // without deleting Source characters, mixing markers into heading slots, or overriding certified visibility with a default size.
            // The same style bits travel in Composer-sealed parts for CPU ink/decorations; fonts own actual shape and width only.
            let native = native_markdown_base_font(self.styles, &typography.palette);
            // Native style conversion may reenter; preserve original Source/ticket failure before accepting its font result.
            // Font failure cannot hide simultaneous original mapping failure, and a revoked candidate cannot register under successor Input.
            ticket.require(owner, source)?;
            let native = native?;
            let current = || ticket.current(owner, source);
            // Borrow the same Body's bounded registered palette, ending slot borrows before reentrant Native entry.
            // Slots describe semantic roles, which may share one actual font; a new role must not recreate identical file snapshots/Core payloads.
            let palette = owner.fonts.borrow().clone();
            let captured = NativeMarkdownPagedFont::capture_reusing(
                native.as_ref(),
                &palette,
                owner.configuration.maximum_font_bytes,
                owner.mtm,
                &current,
            );
            ticket.require(owner, source)?;
            let (existing_index, registered) = match captured? {
                NativeMarkdownFontCapture::Reused(captured) => {
                    // Reused comes only from the original palette's same Core identity, never a name, size, or slot-number guess.
                    // The original Rc already captured this mode's metrics; reuse preserves catalog registration and resource-release ownership.
                    let authority = captured.core_font().authority();
                    let index = palette
                        .iter()
                        .position(|font| font.native().core_font().permits(&authority))
                        .expect("复用字体来自同一原 Body palette");
                    (Some(index), palette[index].clone())
                }
                NativeMarkdownFontCapture::Fresh(captured)
                | NativeMarkdownFontCapture::SharedBytes(captured) => {
                    // Shared frozen bytes do not imply the same instance: another size/axis registers its own Core and captures actual Preview line metrics.
                    let registered =
                        NativeMarkdownLineFont::register(captured, &owner.catalog, &current);
                    ticket.require(owner, source)?;
                    let registered =
                        registered?.capture_preview_metrics(&typography.metrics_owner, &current);
                    ticket.require(owner, source)?;
                    (None, Rc::new(registered?))
                }
            };
            ticket.require(owner, source)?; // Recheck original authority before slot publication; close or original Source failure cannot publish the role.
            // If the platform cannot construct italics, the policy may return a base face; pagination must not cache that as completed emphasis.
            let mut required = objc2_core_text::CTFontSymbolicTraits::empty();
            if self.role != Role::Hidden {
                if self.styles.contains(MarkdownPresentationStyles::EMPHASIS) {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitItalic;
                }
                if matches!(self.role, Role::Heading(_))
                    || (self.role == Role::Body
                        && self.styles.contains(MarkdownPresentationStyles::STRONG))
                {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitBold;
                }
                if self.role == Role::Code {
                    required |= objc2_core_text::CTFontSymbolicTraits::TraitMonoSpace;
                }
            }
            if !registered.traits().contains(required) {
                return Err(crate::native_surface::paged_line_fonts::NativeMarkdownLineFontError::StyleUnavailable.into());
            } // Return a face unable to satisfy required traits; ordinary fonts cannot masquerade as strong, emphasis, or code.
            self.style.points = registered.native().point_size() as f32;
            let index = if let Some(index) = existing_index {
                index // The same instance needs only a new role slot, not another palette entry or catalog registration.
            } else {
                let mut fonts = owner.fonts.borrow_mut();
                let index = fonts.len();
                fonts.push(registered);
                index
            };
            typography.slots[slot] = Some(index);
            Ok(index)
        })();
        if owner.active.get() {
            *owner.typography.borrow_mut() = Some(typography);
        } // Actual close prevents returning the extracted Native font policy to the business slot.
        result
    }
}

/// One Source continuation advances its original Global; selection replacement revokes old authority before its sole same-Doc successor.
enum CellNavigation {
    Required {
        column: usize,
    },
    Inline {
        row: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
        column: usize,
        started: bool,
    },
}

/// A destination carries this actual Row/CellInline Source only; the parent Body's original target path still resolves visual position.
pub(super) struct CellDestination {
    pub row: VerifiedMarkdownLayoutTableRow,
    pub source: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource,
}

pub(super) enum CellNavigationStep {
    /// Original glyph/CellInline work still owns the request; continue it before another consumer can borrow that same request.
    Occupied,
    Pending,
    Ready(CellDestination),
    End {
        terminal: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
        authority: MarkdownViewportSourceAuthority,
    },
    Boundary,
}

pub(super) struct RootLeaf {
    source: MarkdownLayoutTextAuthority,
    context: u64,
    selection: Option<MarkdownTextSelection>,
    request: Option<MarkdownViewportSourceRequest>,
    ready: Option<Rc<VerifiedMarkdownViewportSources>>,
    window: Range<usize>,
    // Ordinary Source preparation has ready=None. After selection changes, pending+ready temporarily holds only the final old publication,
    // awaiting the first actual style range for Core reprojection; it is never Ready authority for the new selection.
    pending: bool,
    /// Track this click's actual reposition start only; missing coverage after its completed replay retains the error, without infinite retries.
    link_repositioned: Option<usize>,
    /// Current cell recovery permits one actual root replay; its range is query intent, not the structural row ticket.
    row_repositioned: Option<usize>,
    /// Active-cell navigation and read-only cells share the original Request; retain only the actual queried structure ticket, not another Global.
    cell_navigation: Option<CellNavigation>,
    pub(super) work: SemanticWork,
}
impl RootLeaf {
    /// The actual request already accounts CellInline in the same total; a previous ordinary-leaf snapshot must not be added again.
    pub(super) fn current_work(&self) -> SemanticWork {
        self.request
            .as_ref()
            .map_or(self.work, MarkdownViewportSourceRequest::total_work)
    }

    /// Structural cells borrow the original request's sole Global; raw tickets cannot create another Parser or default semantics.
    pub(super) fn advance_cell(
        &mut self,
        preparation: &mut omni_markdown_core::MarkdownTableCellDisplayPreparation,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> std::result::Result<
        omni_markdown_core::MarkdownTableCellDisplayPreparationStep,
        MarkdownTableRequestError,
    > {
        preparation.advance_with_source_request(
            document,
            self.request.as_mut().expect("活表格延续原唯一语义请求"),
            false,
        ).inspect_err(|_cause| {
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_root_source_failure stage=cell_display window={:?} selection={:?} total={:?} cause={_cause:?}",
                self.window,
                document.selection(),
                self.current_work(),
            );
        })
    }

    /// Original preparation determines cancellation ownership; advance only its already-borrowed Inline, with actual Display directory close in Core.
    pub(super) fn cancel_cell(
        &mut self,
        preparation: &mut omni_markdown_core::MarkdownTableCellDisplayPreparation,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> std::result::Result<
        omni_markdown_core::MarkdownTableCellDisplayPreparationStep,
        MarkdownTableRequestError,
    > {
        preparation.cancel_with_source_request(
            document,
            self.request.as_mut().expect("取消格仍由原唯一来源请求拥有"),
        )
    }

    pub(super) fn new(source: &MarkdownLayoutTextSource, context: u64) -> Self {
        Self {
            source: source.authority(),
            context,
            selection: None,
            request: None,
            ready: None,
            window: 0..0,
            pending: false,
            link_repositioned: None,
            row_repositioned: None,
            cell_navigation: None,
            work: SemanticWork::default(),
        }
    }

    /// Published fonts depend on complete canonical selection; unchanged text generation cannot renew old hidden-marker authority.
    pub(super) fn selection_changed(&self, selection: MarkdownTextSelection) -> bool {
        self.selection.is_some_and(|old| old != selection)
    }

    /// Old viewports and paused current rows share this entry, obtaining actual row tickets from the original request's latest publication.
    /// The same Global replays from root in bounded quanta, without nearest-checkpoint restoration or renewed scan budgets.
    fn row_source(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> Result<Option<omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource>>
    {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source || self.selection != Some(document.selection()) {
            return Err(super::ViewError::Revoked.into());
        }
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.pending && self.ready.is_some() {
            return Ok(None); // The original style range has not arrived; new selection cannot advance the old request or borrow its row ticket.
        }
        let parser = self.request.as_mut().expect("真实表格沿原唯一来源请求恢复");
        if self.pending {
            // Finish original in-flight Source work before another visible row can use it; no second Global/publication runs alongside it.
            let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work();
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源发布只移交一次"),
                ));
                self.pending = false;
            }
            return Ok(None);
        }
        let ready = self.ready.as_ref().expect("表格恢复持原最新来源发布");
        if self.window.contains(&position)
            && let Some(row) = ready.table_row_at_for(document, authority, position)?
        {
            self.row_repositioned = None;
            return Ok(Some(row));
        }
        if self.row_repositioned == Some(position) {
            // The same query already finished replay without an actual row; bare coordinates cannot gain Source authority or trigger endless retry.
            return Err(Error::Unsupported);
        }
        // Row recovery needs only its first complete scalar's query window; original Scanner still certifies the full physical row and header/delimiter Source.
        // Including another 16K tail would fill the 256-record Source page with unrelated blocks and repeatedly rescan a large table prefix per cell.
        let Some(desired) =
            parser.table_row_source_viewport(document, authority, position, QUANTUM)?
        else {
            self.work = parser.total_work();
            return Ok(None); // PositionLocator remains in the same request/total, without borrowing Doc's 8-KiB UTF-16 cache.
        };
        if desired.start >= self.window.start && desired.end >= self.window.end {
            // Forward rows continue original Global/checkpoints from the latest publication; Core still validates complete forward coverage.
            parser.continue_forward(document, ready, desired.clone(), authority).inspect_err(|_cause| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_forward previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                    self.window,
                    parser.total_work(),
                );
            })?;
        } else {
            parser.reposition(document, ready, desired.clone(), authority).inspect_err(|_cause| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=row_reposition previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                    self.window,
                    parser.total_work(),
                );
            })?;
        } // Only actual backward/narrower requests replay root; forward rows do not restart at the header or renew scan fuel.
        self.ready = None;
        self.window = desired;
        self.pending = true;
        self.row_repositioned = Some(position);
        Ok(None)
    }

    /// Layout and active cells both use the same latest publication; only an actual structural row converts into the original flow row.
    pub(super) fn table_row(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
    ) -> Result<Option<VerifiedMarkdownLayoutTableRow>> {
        let Some(row) = self.row_source(document, source, position)? else {
            return Ok(None);
        };
        Ok(Some(source.verified_table_layout_row_from_source(
            document,
            row,
            self.authority(document),
        )?))
    }

    fn authority(
        &self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
    ) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        }
    }

    pub(super) fn navigating_cell(&self) -> bool {
        self.cell_navigation.is_some()
    }

    pub(super) fn request_started(&self) -> bool {
        self.request.is_some() && !(self.pending && self.ready.is_some())
    }

    /// Advance one actual request stage per turn. Even same-row column navigation obtains latest structure Source; old Layout ticket
    /// coordinates cannot act as publication authority. Cross-row movement continues original RequiredRow; only actual End authorizes append.
    pub(super) fn navigate_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        original: &VerifiedMarkdownLayoutTableRow,
        column: usize,
        action: crate::native_surface::paged_text_view::NativeMarkdownPagedCellAction,
    ) -> Result<CellNavigationStep> {
        use crate::native_surface::paged_text_view::NativeMarkdownPagedCellAction as Action;
        use omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;
        original.require_for(document, source)?;
        let authority = self.authority(document);
        if self.cell_navigation.is_none() {
            let Some(row) = self.row_source(document, source, original.source_range().start)?
            else {
                return Ok(CellNavigationStep::Pending);
            };
            let (header, ordinal, columns) = row.coordinate_for(document, authority)?;
            if (header, ordinal) != original.coordinate() || columns != original.columns() {
                return Err(super::ViewError::Revoked.into());
            }
            // Move only inside the actual column domain; original Scanner still supplies implicit-empty Sources for missing cells.
            let local = match action {
                Action::Next if column + 1 < columns => Some(column + 1),
                Action::Previous if column > 0 => Some(column - 1),
                Action::Cancel => return Err(Error::Unsupported),
                _ => None,
            };
            if let Some(column) = local {
                self.cell_navigation = Some(CellNavigation::Inline {
                    row,
                    column,
                    started: false,
                });
                return Ok(CellNavigationStep::Pending);
            }
            let direction = match action {
                Action::Next => MarkdownTableWindowNavigation::Next,
                Action::Previous => MarkdownTableWindowNavigation::Previous,
                Action::NextRow => MarkdownTableWindowNavigation::NextRow,
                Action::Cancel => unreachable!("取消不发来源导航"),
            };
            let ready = self.ready.as_ref().expect("最新行来自当前出版物");
            let Some(query) = ready.required_row_query_for(document, authority, &row, direction)?
            else {
                return Ok(CellNavigationStep::Boundary); // The actual header-start boundary leaves draft and selection unchanged.
            };
            self.request
                .as_mut()
                .expect("活动格复用原来源请求")
                .continue_forward_with_required_row(
                    document,
                    ready,
                    self.window.clone(),
                    authority,
                    query,
                )?;
            self.ready = None;
            self.pending = true;
            self.cell_navigation = Some(CellNavigation::Required {
                column: match action {
                    Action::Next => 0,
                    Action::Previous => columns - 1,
                    Action::NextRow => column,
                    Action::Cancel => unreachable!("取消不发来源导航"),
                },
            });
            return Ok(CellNavigationStep::Pending);
        }
        self.advance_cell_navigation(document, source)
    }

    /// After commit the old position is query intent only; the actual new row must be recertified, never reopening a draft under old raw authority.
    pub(super) fn activate_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        position: usize,
        column: usize,
        next_row: bool,
    ) -> Result<CellNavigationStep> {
        if self.cell_navigation.is_none() {
            let Some(row) = self.row_source(document, source, position)? else {
                return Ok(CellNavigationStep::Pending);
            };
            if next_row {
                use omni_markdown_core::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;
                let authority = self.authority(document);
                let ready = self.ready.as_ref().expect("新canonical的真实行来源");
                let query = ready
                    .required_row_query_for(
                        document,
                        authority,
                        &row,
                        MarkdownTableWindowNavigation::Next,
                    )?
                    .expect("真实向后查询不是表头前界");
                self.request
                    .as_mut()
                    .expect("新canonical唯一来源请求")
                    .continue_forward_with_required_row(
                        document,
                        ready,
                        self.window.clone(),
                        authority,
                        query,
                    )?;
                self.ready = None;
                self.pending = true;
                self.cell_navigation = Some(CellNavigation::Required { column });
                return Ok(CellNavigationStep::Pending);
            } // Only actual append receipts trigger this; original RequiredRow certifies the new row without inferring its start from inserted length.
            self.cell_navigation = Some(CellNavigation::Inline {
                row,
                column,
                started: false,
            });
            return Ok(CellNavigationStep::Pending);
        }
        self.advance_cell_navigation(document, source)
    }

    fn advance_cell_navigation(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
    ) -> Result<CellNavigationStep> {
        use omni_markdown_core::markdown_document_buffer::{
            MarkdownTableCellInlineStep, MarkdownViewportSourceRequiredRowResult,
        };
        let authority = self.authority(document);
        let navigation = self.cell_navigation.take().expect("原导航仍在同一请求");
        match navigation {
            CellNavigation::Required { column } => {
                // Continue the already-admitted row search once. Consuming its
                // publication and Row/End result ends this stage before Inline starts.
                let parser = self.request.as_mut().expect("原 RequiredRow 仍有 owner");
                let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_required window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                    self.cell_navigation = Some(CellNavigation::Required { column });
                    return Ok(CellNavigationStep::Pending);
                }
                let ready = Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源仅交付一次"),
                );
                let result = ready
                    .required_row_result_for(document, authority)?
                    .expect("原 RequiredRow Ready 必须交真实行或 End");
                self.ready = Some(ready);
                self.pending = false;
                match result {
                    MarkdownViewportSourceRequiredRowResult::Row { row } => {
                        self.cell_navigation = Some(CellNavigation::Inline {
                            row,
                            column,
                            started: false,
                        });
                        Ok(CellNavigationStep::Pending)
                    }
                    MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } => {
                        Ok(CellNavigationStep::End {
                            terminal,
                            authority,
                        })
                    }
                }
            }
            CellNavigation::Inline {
                row,
                column,
                started,
            } => {
                // Starting Inline and advancing it occupy separate turns; the
                // same Parser retains its actual row ticket and total work budget.
                let parser = self.request.as_mut().expect("格语义短借原唯一请求");
                if !started {
                    parser.prepare_cell_inline(document, row.clone(), authority, column)?;
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started: true,
                    });
                    return Ok(CellNavigationStep::Pending);
                }
                let step = parser.advance_cell_inline(document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_inline window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started,
                    });
                    return Ok(CellNavigationStep::Pending);
                }
                let inline = parser
                    .take_cell_inline_ready_for(document, authority)?
                    .expect("真实 CellInline Ready 只移交一次");
                let raw = inline.cell_source_for(document, authority)?.clone();
                let row = source.verified_table_layout_row_from_source(document, row, authority)?;
                Ok(CellNavigationStep::Ready(CellDestination {
                    row,
                    source: raw,
                }))
            }
        }
    }

    /// Cancellation stops successor-cell admission, but the sole request must actually return all work it already started.
    /// Stop RequiredRow after its actual Row/End result; cleanup must not start another CellInline.
    pub(super) fn drain_cell_navigation(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
    ) -> Result<bool> {
        use omni_markdown_core::markdown_document_buffer::MarkdownTableCellInlineStep;

        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if self.cell_navigation.is_none() && !self.pending {
            return Ok(true); // Intent that never borrowed Source has no external task to cancel; do not create a request to demonstrate idleness.
        }
        if source.authority() != self.source || self.selection != Some(document.selection()) {
            return Err(super::ViewError::Revoked.into());
        }
        if self.pending && self.ready.is_some() && self.cell_navigation.is_none() {
            // Source health and complete new selection were checked; pending+ready awaits its first new style range and has no cell job to drain.
            // Navigation cancellation cannot pass new selection to the unreprojected old request or cancel original Composer preparation.
            return Ok(true);
        }
        let authority = self.authority(document);
        let navigation = self.cell_navigation.take();
        match navigation {
            Some(CellNavigation::Inline { started: false, .. }) => Ok(true), // Only a certified row ticket exists; Parser has not started cell work, so returning that ticket is sufficient.
            Some(CellNavigation::Inline {
                row,
                column,
                started: true,
            }) => {
                let parser = self.request.as_mut().expect("原 CellInline 仍有唯一 owner");
                let step = parser.advance_cell_inline(document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_drain_inline window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                    self.cell_navigation = Some(CellNavigation::Inline {
                        row,
                        column,
                        started: true,
                    });
                    return Ok(false);
                }
                // Taking the result actually returns the request's cell occupancy; discard it without Display conversion or writable-draft creation.
                drop(
                    parser
                        .take_cell_inline_ready_for(document, authority)?
                        .expect("原 CellInline Ready 只归还一次"),
                );
                Ok(true)
            }
            navigation => {
                // This includes original RequiredRow and ordinary Source root replay still in flight before navigation obtains its row.
                // Both consume the same actual publication; only RequiredRow additionally consumes its actual Row/End result.
                let parser = self.request.as_mut().expect("原来源量子仍有唯一 owner");
                let step = parser.advance(document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=cell_drain_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
                self.work = parser.total_work();
                if !matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                    self.cell_navigation = navigation;
                    return Ok(false);
                }
                let ready = Rc::new(
                    parser
                        .take_sources_for(document, authority)?
                        .expect("真实来源出版物只归还一次"),
                );
                if matches!(navigation, Some(CellNavigation::Required { .. })) {
                    ready
                        .required_row_result_for(document, authority)?
                        .expect("原 RequiredRow 必须交回真实 Row 或 End");
                }
                self.ready = Some(ready);
                self.pending = false;
                Ok(true)
            }
        }
    }

    /// Original row tickets validate identity; latest publication authorizes preparation. Repositioned rows cannot start CellInline using old publication.
    pub(super) fn prepare_cell(
        &mut self,
        document: &omni_markdown_core::MarkdownDocumentBuffer,
        source: &MarkdownLayoutTextSource,
        expected: &VerifiedMarkdownLayoutTableRow,
        column: usize,
    ) -> Result<Option<omni_markdown_core::MarkdownTableCellDisplayPreparation>> {
        expected.require_for(document, source)?;
        let Some(row) = self.table_row(document, source, expected.source_range().start)? else {
            return Ok(None);
        };
        if row.source_range() != expected.source_range()
            || row.coordinate() != expected.coordinate()
            || row.columns() != expected.columns()
        {
            // Only the same Source/physical row continues this cell; another queried structure cannot substitute for the original row.
            return Err(super::ViewError::Revoked.into());
        }
        Ok(Some(row.prepare_cell(document, source, column)?))
    }

    /// Retain same-Source glyphs only when actual Plain-prefix or Root Code capability covers all Composer inventory.
    /// Capability borrowing, read-only Composer validation, and old-publication retirement share one pure Core stack, without Native callbacks.
    pub(super) fn reproject_selection_preserving_styles(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        selection: MarkdownTextSelection,
        composer: &MarkdownLineComposer,
        ticket: &PreparationTicket,
    ) -> Result<bool> {
        ticket.require_pure(owner, source)?;
        if source.authority() != self.source {
            return Err(super::ViewError::Revoked.into());
        }
        if self.pending || self.cell_navigation.is_some() {
            return Ok(false); // Original request/cell work is still in flight; the existing relayout path cancels it and waits for actual return.
        }
        let (Some(parser), Some(previous)) = (self.request.as_mut(), self.ready.as_ref()) else {
            return Ok(false);
        };
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf只借原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection,
            cancelled: false,
        };
        {
            // cap is neither a bare Range nor a strong old-publication copy; it borrows original request/Doc/latest publication,
            // certifying a read Plain prefix or selection movement within the same Root Code body. Neither reuses old
            // marked/selection pixels: old Scene/Reader still retire and the successor frame must match complete new Input.
            // Composer checks its own Stage, Store, and active supported range;
            // Native SceneReady or displayed-line counts cannot infer shaping input that Composer has not captured.
            let Some(capability) =
                parser.selection_style_invariant(&document, previous, authority)?
            else {
                return Ok(false);
            };
            if !composer.can_retain_selection_styles(&capability)? {
                return Ok(false);
            }
        } // End the borrow before the sole request changes selection/retires publication; cap never survives Native entry or suspension.
        ticket.require_pure(owner, source)?;
        if !parser.reproject_selection(&document, previous, self.window.clone(), authority)? {
            return Ok(false); // Core rejection preserves the original owner/publication and leaves recovery to the established reset_styles path.
        }
        self.work = parser.total_work();
        self.ready = None; // The old publication permanently retires now, not at another NeedStyle; the target may already lie inside the retained prefix.
        self.pending = true;
        self.selection = Some(selection);
        self.link_repositioned = None;
        self.row_repositioned = None;
        // Successor styles require a new publication from normal advance under new selection; an old Boundary cannot renew new Input authority.
        ticket.require_pure(owner, source)?;
        Ok(true)
    }

    /// Accept new selection after the old glyph graph ended; only the original request that transferred output may attempt same-Source structural reuse.
    /// The successor's first actual StyleRequest determines its window, avoiding guessed ranges that could split a GCB.
    pub(super) fn reset_selection(&mut self, selection: MarkdownTextSelection) {
        if !self.pending
            && self.request.is_some()
            && self.ready.is_some()
            && self.cell_navigation.is_none()
        {
            // pending+ready records reprojection intent in the same slot; the old publication remains authorized by its old complete selection,
            // with all queries blocked by pending. Return the final old publication only after Core accepts or rejects reprojection.
            self.pending = true;
            self.link_repositioned = None;
            self.row_repositioned = None;
            self.cell_navigation = None;
        } else {
            self.clear(); // Active Source/cell work or another selection change follows original cancellation, release, and cold preparation.
        }
        self.selection = Some(selection);
    }

    /// Targets borrow actual parsing facts from original Source/publication; unknown off-window coverage stays Pending, never a URL inferred from LINK bits.
    pub(super) fn link_at(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        index: usize,
    ) -> std::result::Result<Option<omni_markdown_core::MarkdownSourceLink>, super::ViewError> {
        source
            .byte_offset_for_utf16(0)
            .map_err(super::ViewError::Document)?;
        if source.authority() != self.source {
            return Err(super::ViewError::Revoked);
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf链接只绑定原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.pending && self.ready.is_some() {
            if self.selection != Some(authority.selection) {
                return Err(super::ViewError::Revoked);
            }
            return Err(super::ViewError::Pending); // The final original publication gains no new authority merely because Native accepted another selection.
        }
        let ready = self.ready.as_ref().ok_or(super::ViewError::Pending)?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = ready
            .ordinary_semantics_for(&document, authority)
            .map_err(super::ViewError::LinkSource)?
        else {
            return Err(super::ViewError::Pending);
        };
        // The bounded closed block's Parser already admitted complete labels and decoded targets. Positive link facts
        // can cover same-block text outside the style intersection, without expanding font, paint, or negative no-link authority.
        match leaf.link_at_for(&document, authority, index) {
            Ok(link) => Ok(link.cloned()),
            Err(MarkdownTableRequestError::Window(MarkdownTableWindowError::ContextPending))
                if !self.window.contains(&index) =>
            {
                Err(super::ViewError::Pending)
            } // Off-window queries lacking positive evidence remain Pending; the actual click must advance the sole original Source request.
            Err(cause) => Err(super::ViewError::LinkSource(cause)),
        }
    }

    /// Queries from old sealed Scenes reposition only the sole original request; later turns advance/install separately without Source copies or a document-wide URL table.
    pub(super) fn advance_link(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        index: usize,
        ticket: &PreparationTicket,
    ) -> Result<Option<Option<omni_markdown_core::MarkdownSourceLink>>> {
        self.row_repositioned = None; // Link intent takes the sole Source cursor; the previously cancelled row intent ends here.
        ticket.require_pure(owner, source)?;
        if source.authority() != self.source || index >= source.utf16_len() {
            return Err(super::ViewError::Revoked.into());
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("RootLeaf链接恢复只绑定原Body文档")
            .clone();
        let document = document.borrow();
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection: document.selection(),
            cancelled: false,
        };
        if self.selection != Some(authority.selection) {
            return Err(super::ViewError::Revoked.into()); // A click cannot reauthorize the old semantic graph after selection changes.
        }
        if self.pending && self.ready.is_some() {
            return Ok(None); // Await the original Composer's new style range without advancing an old-selection request into failure.
        }
        if self.pending {
            let parser = self.request.as_mut().expect("pending持唯一来源请求");
            let step = parser.advance(&document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=link_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work();
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(&document, authority)?
                        .expect("SourcesAvailable持唯一Ready"),
                ));
                self.pending = false;
            }
            return Ok(None); // One quantum advances only the original Parser, without simultaneously installing another publication or issuing Native effects.
        }
        // First use positive targets already owned by this publication. Window limits constrain unknown/negative evidence,
        // not another label already validated and accounted by Parser; do not discard it or replay its whole Source.
        match self.link_at(owner, source, index) {
            Ok(link) => {
                self.link_repositioned = None;
                return Ok(Some(link));
            }
            Err(super::ViewError::Pending) if !self.window.contains(&index) => {} // Unknown off-window targets use the original one-unit recovery; missing in-window facts retain failure instead of replaying endlessly.
            Err(
                cause @ super::ViewError::LinkSource(MarkdownTableRequestError::Window(
                    MarkdownTableWindowError::ContextPending,
                )),
            ) => {
                if self.link_repositioned == Some(index) {
                    return Err(cause.into()); // The same start finished replay without coverage; preserve typed Pending rather than inventing an empty target or rescanning again.
                }
            }
            Err(cause) => return Err(cause.into()), // I/O, revocation, and original parse failure authorize no fallback replay.
        }
        let ready = self.ready.as_ref().ok_or(super::ViewError::Pending)?;
        // A link queries the original hit, without requesting another 16-KiB tail of tables/blocks into its semantic window.
        // The one-unit range limits query coverage only. Core still uses the same Global's actual closed block for complete Parser,
        // reference, and HTML context, returning complete label/destination without truncating Markdown or creating another Source.
        // The hit index is an actual GCB start; for emoji, the original endpoint locator still certifies the complete scalar.
        let desired = index..source.utf16_len().min(index + 1);
        self.request
            .as_mut()
            .expect("已发布RootLeaf仍持同一来源续体")
            .reposition(&document, ready, desired.clone(), authority)?;
        self.ready = None; // The request owns repositioning; retire old Ready before a later quantum assembles its bounded semantic workspace.
        self.window = desired;
        self.pending = true;
        self.link_repositioned = Some(index);
        Ok(None) // Coverage holes inside the numeric window also recover their actual start through this request, never certifying no link.
    }
    pub(super) fn select(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &MarkdownLineStyleRequest,
        ticket: &PreparationTicket,
    ) -> Result<Option<StyleDecision>> {
        self.select_range(
            owner,
            source,
            request.source(),
            request.source_range(),
            None,
            ticket,
        )
    }

    /// Original LF/CRLF/Tab query the sole publication; only actual line-tree proof permits literal defaults for empty logical lines.
    pub(super) fn select_control(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        request: &omni_markdown_core::MarkdownLineInlineControl,
        ticket: &PreparationTicket,
    ) -> Result<Option<Selection>> {
        match self.select_range(
            owner,
            source,
            request.source(),
            request.source_utf16.clone(),
            Some(request.kind),
            ticket,
        )? {
            None => Ok(None),
            Some(StyleDecision::Font(selection)) => Ok(Some(selection)),
            Some(StyleDecision::Table(_)) => Err(Error::Unsupported),
        }
    }

    fn select_range(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        range: Range<usize>,
        control: Option<omni_markdown_core::MarkdownLineInlineControlKind>,
        ticket: &PreparationTicket,
    ) -> Result<Option<StyleDecision>> {
        ticket.require_pure(owner, source)?;
        self.link_repositioned = None; // Original Composer resumes semantic ownership; a later click is another query and inherits no old gesture recovery record.
        self.row_repositioned = None; // Likewise end the preceding row recovery's single-replay record; later backward scrolling is a new actual request.
        if source.authority() != self.source || authority != &self.source {
            return Err(super::ViewError::Revoked.into());
        }
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("InlinePreview装配只接受Body原Doc")
            .clone();
        let document = document.borrow();
        // Only a control actually lacking semantic coverage below needs proof that its entire logical line is empty.
        // Covered controls, initial construction, and in-flight quanta do not need this fact; avoid repeatedly decoding line-tree Unicode measurements.
        // The deferred query keeps existing CRLF/trailing-empty-line rules, without another line parser or cross-request cache.
        let is_blank_control = || -> Result<bool> {
            Ok(control.is_some()
                && super::producer::source_line_range_at_head(&document, range.start)
                    .map_err(super::ViewError::Document)?
                    == range)
        };
        let line_break = matches!(
            control,
            Some(
                omni_markdown_core::MarkdownLineInlineControlKind::CarriageReturn
                    | omni_markdown_core::MarkdownLineInlineControlKind::LineFeed
            )
        ); // Control kind comes from original Composer nonce; only actual CR/LF queries this Source's fence separator.
        let selection = document.selection();
        if self.selection.is_some_and(|old| old != selection) {
            self.clear(); // Old-selection publication cannot renew authority; acquire no second Source or concurrent parse frontier.
        }
        let authority = MarkdownViewportSourceAuthority {
            context_revision: self.context,
            selection,
            cancelled: false,
        };
        let desired = range.start..range.end.min(range.start.saturating_add(WINDOW_UTF16));
        if self.pending && self.ready.is_some() {
            // Transfer only the original request whose output was consumed and its final publication. Actual Style/Control supplies the new range,
            // and current Doc supplies complete selection. Core validates Source, closed records, and prefix capability without default-style guesses.
            let parser = self.request.as_mut().expect("待重投影仍持原唯一来源请求");
            let previous = self.ready.as_ref().expect("待重投影只借原最后发布");
            let reprojected =
                parser.reproject_selection(&document, previous, desired.clone(), authority)?;
            self.work = parser.total_work(); // Fixed control preparation and actual destruction remain charged to the original cumulative request.
            if reprojected {
                self.ready = None; // Core revoked the old publication; retain no old Ready/Reader or renewed old-font authority.
                self.window = desired;
                return Ok(None); // Reprojection control and Source advance occupy separate turns, without renewed or hidden step allowances.
            }
            // False means only bounded records/proven prefix are insufficient; Core guarantees unchanged ownership.
            // I/O, cancellation, old publication, or complete-selection revocation exits through Err unchanged, never as successful cold preparation.
            self.clear();
            self.selection = Some(selection);
        }
        if self.request.is_none() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_root_source_window stage=style_create requested={range:?} desired={desired:?} selection={selection:?} source_units={}",
                    source.utf16_len(),
                );
            } // Record actual window creation only, not each Font/GCB request.
            self.request = Some(document.viewport_leaf_source_request(
                desired.clone(),
                self.context,
                selection,
            )?);
            self.selection = Some(selection);
            self.window = desired;
            self.pending = true;
            // Construction binds original root and fixed inventory without text reads; below still executes just one actual Source step.
            // Fixed control preparation has its own actual cost; do not claim all turn metadata is bounded by the 4096 scan allowance.
        } else if let Some(parser) = self.request.as_mut()
            && !self.pending
            && !self.window.contains(&range.start)
        {
            let ready = self.ready.as_ref().expect("完成来源持同一publication");
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_root_source_window stage=style_retarget previous={:?} requested={range:?} desired={desired:?} selection={selection:?}",
                    self.window,
                );
            }
            let backward = range.start < self.window.start;
            if backward {
                parser
                    .reposition(&document, ready, desired.clone(), authority)
                    .inspect_err(|_cause| {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_root_source_failure stage=style_reposition previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                            self.window,
                            parser.total_work(),
                        );
                    })?;
            } else {
                parser
                    .continue_forward(&document, ready, desired.clone(), authority)
                    .inspect_err(|_cause| {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_root_source_failure stage=style_forward previous={:?} desired={desired:?} total={:?} cause={_cause:?}",
                            self.window,
                            parser.total_work(),
                        );
                    })?;
            }
            self.ready = None; // A click may move the sole semantic cursor to an old viewport; successor Composer still prepares its needed actual Source window.
            self.window = desired;
            self.pending = true;
            if backward {
                // Backward movement first resets actual Global/old ownership; retain the separate control turn without also constructing a new Leaf.
                return Ok(None);
            }
            // Forward movement transfers only the original publication's bounded directory/latest quota; cumulative fuel never resets,
            // then advances that same request once, avoiding an extra Native scheduling turn for each short style window's control preparation.
        }
        if self.pending {
            let parser = self.request.as_mut().expect("pending持原来源请求");
            let step = parser.advance(&document, authority, QUANTUM).inspect_err(|_cause| {
                // Acceptance diagnostics retain actual request stage, window, and totals only, without text, paths, or link destinations.
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_source_failure stage=style_source window={:?} selection={:?} total={:?} cause={_cause:?}",
                    self.window,
                    authority.selection,
                    parser.total_work(),
                );
            })?;
            self.work = parser.total_work(); // Actual cumulative Source reads/copies/Parser input stay separate from Composer byte accounting.
            if matches!(step, MarkdownViewportSourceStep::SourcesAvailable { .. }) {
                self.ready = Some(Rc::new(
                    parser
                        .take_sources_for(&document, authority)?
                        .expect("SourcesAvailable持唯一Ready"),
                ));
                self.pending = false;
            }
            return Ok(None); // Advance once only; even Ready does not combine style lookup, font work, or Composer advancement into this turn.
        }
        let ready = self.ready.as_ref().expect("完成来源持同一publication");
        if line_break
            && let Some(separator) = ready.root_fence_separator_for(&document, authority, &range)?
        {
            let mut selection = Selection::literal_range(source, separator, 0.0);
            selection.fence_separator = true;
            selection.scope = SelectionScope::Body(ready.clone(), authority);
            return Ok(Some(StyleDecision::Font(selection))); // The complete Parser supplies no code style for the closing separator; its original Root ticket certifies that control separately.
        }
        if control.is_none()
            && let Some(row) = ready.table_row_at_for(&document, authority, range.start)?
        {
            let row = source.verified_table_layout_row_from_source(&document, row, authority)?;
            if row.source_range().start != range.start {
                return Err(Error::Unsupported);
            } // Original NeedStyle must be at the actual block start; already-consumed midline text cannot be skipped retroactively.
            return Ok(Some(StyleDecision::Table(row)));
        } // TABLE bits, equal ranges, and offscreen header context cannot manufacture a paintable row.
        let state = ready.ordinary_semantics_for(&document, authority)?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = state else {
            let blank = is_blank_control()?;
            if blank {
                return Ok(Some(StyleDecision::Font(Selection::literal_range(
                    source, range, 0.0,
                ))));
            } // Preserve control height only for actual empty logical lines; unknown ordinary glyph ranges remain rejected.
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_root_leaf_missing_semantics range={range:?} window={:?} blank={blank} line_break={line_break} state={}",
                self.window,
                match state {
                    MarkdownOrdinarySemanticState::SourcesOnly => "sources-only".into(),
                    MarkdownOrdinarySemanticState::Pending(reason) =>
                        format!("plain-pending:{reason:?}"),
                    MarkdownOrdinarySemanticState::LeafPending(reason) =>
                        format!("leaf-pending:{reason:?}"),
                    MarkdownOrdinarySemanticState::PlainReady(_) => "plain-ready".into(),
                    MarkdownOrdinarySemanticState::RootLeafReady(_) => unreachable!(),
                }
            );
            return Err(Error::Unsupported); // SourcesOnly or unknown inline coverage cannot certify default Body styling.
        };
        let query = match leaf.query_for(&document, authority, range.start)? {
            Some(query) => query,
            None => {
                // Check the complete control line only when uncovered; visible glyphs cannot bypass original Parser with default styling.
                let blank = is_blank_control()?;
                if blank {
                    return Ok(Some(StyleDecision::Font(Selection::literal_range(
                        source, range, 0.0,
                    ))));
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_root_leaf_uncovered_query range={range:?} window={:?} blank={blank} line_break={line_break} coverage={:?}",
                    self.window,
                    leaf.covered_ranges_for(&document, authority)?
                        .collect::<Vec<_>>()
                );
                return Err(Error::Unsupported);
            }
        };
        let end = range.end.min(query.utf16_range.end);
        if end <= range.start {
            return Err(Error::Unsupported);
        }
        Ok(Some(StyleDecision::Font(Selection {
            coverage: range.start..end,
            style: MarkdownLineComposeStyle {
                presentation: query.styles,
                points: 0.0,
                language: None,
                features: Vec::new(),
            },
            role: Role::from_styles(query.styles),
            styles: query.styles,
            fence_separator: false,
            scope: SelectionScope::Body(ready.clone(), authority),
            source: self.source.clone(),
        })))
    }
    #[cfg(all(
        feature = "native-paged-body-probe",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(super) fn publication(&self) -> Option<std::rc::Weak<VerifiedMarkdownViewportSources>> {
        if self.pending {
            None // An old publication awaiting reprojection belongs to old selection and cannot observe the current graph.
        } else {
            self.ready.as_ref().map(Rc::downgrade)
        }
    }

    fn clear(&mut self) {
        // Cancel the original request before releasing its last publication and
        // navigation facts. Drop follows this same ownership path; no callback
        // may later install a result into the retired RootLeaf.
        if let Some(request) = self.request.as_mut() {
            request.cancel();
        }
        self.request = None;
        self.ready = None;
        self.selection = None;
        self.pending = false;
        self.link_repositioned = None;
        self.row_repositioned = None;
        self.cell_navigation = None;
    }
}
impl Drop for RootLeaf {
    fn drop(&mut self) {
        self.clear();
    } // Close returns actual semantic continuation/Ready without waiting for a late Native or worker callback to clear slots.
}

#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
/// Bounded diagnostics from actual sealed parts, without strong Source/font/Reader claims or fabricated line identity.
pub(in crate::native_surface) struct RootLeafPart {
    pub source: Range<usize>,
    /// Total sealed-glyph advance, distinct from CTFont's Native advance; actual Core shaping still requires independent comparison.
    pub advance_26_6: i64,
    pub script: [u8; 4],
    pub direction: omni_markdown_core::MarkdownShapingDirection,
    /// Capture complete actual parts of at most 256 glyphs; larger parts are explicitly unobserved rather than truncated into a complete domain.
    /// These Copy values retain no Reader/Source/font ownership and do not expand the original observation entry's supported part range.
    pub glyphs: Option<Vec<omni_markdown_core::MarkdownShapedGlyph>>,
    pub presentation: MarkdownPresentationStyles,
    pub points: f32,
    pub native_points: f64,
    pub role: &'static str,
    pub traits: objc2_core_text::CTFontSymbolicTraits,
    pub metrics: crate::native_surface::paged_font_geometry::NativeMarkdownPagedFontMetricValues,
    pub font_owner: std::rc::Weak<NativeMarkdownLineFont>,
}

#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
impl Typography {
    pub(super) fn role_at(
        &self,
        index: usize,
        presentation: MarkdownPresentationStyles,
    ) -> &'static str {
        // Body, table header, or code roles may share one actual font; its index has no unique semantic meaning.
        // Select a slot using the original sealed part's style, then verify it actually published this registered font;
        // neither the first equal-index slot nor style alone can misclassify another fallback font as the original role font.
        let role = Role::from_styles(presentation);
        let Some(slot) = role.slot(presentation) else {
            return "body-or-fallback";
        };
        if self.slots[slot] != Some(index) {
            return "body-or-fallback";
        }
        match role {
            Role::Hidden => "hidden",
            Role::Heading(_) => "heading",
            Role::Code => "code",
            Role::Table => "table",
            Role::Body => "body-or-fallback",
        }
    }
}

#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
impl Owner {
    /// Visible-window line observation copies bounded scalar facts, without deriving a geometry oracle from Producer style expectations.
    pub(in crate::native_surface) fn probe_root_leaf_lines(
        &self,
    ) -> Result<Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>> {
        let scene = self.scene.borrow().clone().ok_or(Error::Busy)?;
        scene.probe_line_geometry().map_err(Error::from)
    }
    /// Observe the actual publication through a weak ticket, without deriving diagnostic output from semantic expectations.
    /// The metadata-only weak ticket permits actual Ready-release observation without extending any Source or Native lifetime.
    pub(in crate::native_surface) fn probe_root_leaf_publication(
        &self,
    ) -> Result<std::rc::Weak<VerifiedMarkdownViewportSources>> {
        self.producer
            .borrow()
            .as_ref()
            .and_then(|producer| producer.root_leaf_publication())
            .ok_or(Error::Busy)
    }

    pub(in crate::native_surface) fn probe_root_leaf_parts(&self) -> Result<Vec<RootLeafPart>> {
        self.producer
            .borrow()
            .as_ref()
            .ok_or(Error::Busy)?
            .root_leaf_parts(self)
    }
}
