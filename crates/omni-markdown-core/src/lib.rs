//! Markdown document, vault, and bounded presentation contracts owned by the plugin.
//!
//! This core has no Host, Tauri, WebView, or AppKit dependency. Explicit foreground
//! sessions own mmap/piece-tree editing history and confined file operations;
//! viewport semantics retain bounded metadata rather than a second document model.

mod immutable_snapshot;
mod original_backing;
pub use original_backing::MarkdownOriginalBackingObserver;

#[cfg(feature = "s2-table-window-prototype")]
mod markdown_table_cell_display;
#[cfg(feature = "s2-table-window-prototype")]
pub use markdown_table_cell_display::{
    MarkdownTableCellDisplayAnswer, MarkdownTableCellDisplayCursor, MarkdownTableCellDisplayInput,
    MarkdownTableCellDisplayScalar, MarkdownTableCellDisplayStep,
};

// Independent layout algorithms share one frozen source capsule; capturing it
// retains resource authority without running analysis or constructing Native layout.
#[cfg(any(
    feature = "paged-graphemes",
    feature = "paged-line-breaks",
    feature = "paged-shaping"
))]
mod markdown_layout_index;
#[cfg(any(
    feature = "paged-graphemes",
    feature = "paged-line-breaks",
    feature = "paged-shaping"
))]
pub use markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES;

// Script runs reuse the original frozen source and grapheme revision; a read
// window is an I/O boundary, not permission to invent another shaping boundary.
#[cfg(feature = "paged-scripts")]
mod markdown_scripts;
#[cfg(feature = "paged-scripts")]
pub use markdown_scripts::{
    MARKDOWN_SCRIPT_INDEX_BYTES, MARKDOWN_SCRIPT_SOURCE_BYTES, MarkdownScript,
    MarkdownScriptAnswer, MarkdownScriptBudget, MarkdownScriptCancellation, MarkdownScriptError,
    MarkdownScriptJob, MarkdownScriptResourceObserver, MarkdownScriptResourceSnapshot,
    MarkdownScriptRun, MarkdownScriptStatus, MarkdownScriptStep, MarkdownScriptStorageProfile,
    MarkdownScriptUnit,
};
#[cfg(any(
    feature = "paged-graphemes",
    feature = "paged-line-breaks",
    feature = "paged-shaping"
))]
mod markdown_layout_source;
#[cfg(any(
    feature = "paged-graphemes",
    feature = "paged-line-breaks",
    feature = "paged-shaping"
))]
pub use markdown_layout_source::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceDomain, MarkdownLayoutTextSourceObserver,
};
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use markdown_layout_source::{
    MarkdownTableCellDisplayPreparation, MarkdownTableCellDisplayPreparationStep,
    MarkdownTableCellDisplayResourceObserver, MarkdownTableCellDisplayResources,
    MarkdownTableCellDisplaySource, MarkdownTableCellVisibleSourceRanges,
    VerifiedMarkdownLayoutTableRow, VerifiedMarkdownRootLeafStyleBoundary,
};

// The native plugin enables these paged font/shaping contracts in production.
// Core keeps them feature-selectable and never acquires an AppKit dependency.
#[cfg(feature = "paged-shaping")]
mod markdown_layout_font;
#[cfg(feature = "paged-shaping")]
pub use markdown_layout_font::{
    MarkdownLayoutFont, MarkdownLayoutFontAuthority, MarkdownLayoutFontAxis,
    MarkdownLayoutFontBytes, MarkdownLayoutFontBytesError, MarkdownLayoutFontBytesResourceObserver,
    MarkdownLayoutFontError, MarkdownLayoutFontMetadata, MarkdownLayoutFontResourceObserver,
};
#[cfg(feature = "paged-shaping")]
mod markdown_glyph_clusters;
#[cfg(feature = "paged-shaping")]
mod markdown_shaping;
#[cfg(feature = "paged-shaping")]
pub use markdown_glyph_clusters::{
    MARKDOWN_GLYPH_CLUSTER_STEP, MarkdownGlyphCluster, MarkdownGlyphClusterAnswer,
    MarkdownGlyphClusterError, MarkdownGlyphClusterIndex, MarkdownGlyphClusterResourceObserver,
    MarkdownGlyphClusterResources, MarkdownGlyphClusterStatus, MarkdownGlyphClusterStep,
};
#[cfg(feature = "paged-shaping")]
pub use markdown_shaping::{
    MARKDOWN_SHAPED_SPOOL_STEP, MARKDOWN_SHAPING_MAX_FEATURES, MarkdownShapedGlyph,
    MarkdownShapedSpool, MarkdownShapedSpoolAuthority, MarkdownShapedSpoolBuilder,
    MarkdownShapedSpoolError, MarkdownShapedSpoolIoError, MarkdownShapedSpoolIoOperation,
    MarkdownShapedSpoolResourceObserver, MarkdownShapedSpoolResources, MarkdownShapedSpoolStatus,
    MarkdownShapedSpoolStep, MarkdownShapingBudget, MarkdownShapingCancellation,
    MarkdownShapingDirection, MarkdownShapingError, MarkdownShapingFeature, MarkdownShapingIoError,
    MarkdownShapingIoOperation, MarkdownShapingJob, MarkdownShapingOptions, MarkdownShapingProfile,
    MarkdownShapingResourceObserver, MarkdownShapingResourceSnapshot, MarkdownShapingStatus,
    MarkdownShapingStep,
};

#[cfg(feature = "paged-layout-breaks")]
mod markdown_layout_breaks;
#[cfg(feature = "paged-layout-breaks")]
pub use markdown_layout_breaks::{
    MARKDOWN_LAYOUT_BREAK_CANDIDATES, MarkdownLayoutBreakAnswer, MarkdownLayoutBreakCursor,
    MarkdownLayoutBreakError, MarkdownLayoutBreakJob, MarkdownLayoutBreakQuery,
    MarkdownLayoutBreakResourceObserver, MarkdownLayoutBreakStep, MarkdownLayoutBreakUnit,
};

// Width preparation reports real candidates and remaining work; the composer
// alone owns publication of a completed soft line.
#[cfg(feature = "paged-soft-line-measure")]
mod markdown_soft_line_measure;
#[cfg(feature = "paged-soft-line-resolve")]
mod markdown_soft_line_resolve;
// Glyph slices consume actual internal measurement proofs. A public Range does
// not grant authority to publish glyphs from an unrelated or unfinished run.
#[cfg(feature = "paged-safe-glyph-slices")]
mod markdown_safe_glyph_slice;
#[cfg(feature = "paged-safe-glyph-slices")]
pub use markdown_safe_glyph_slice::{
    MARKDOWN_SAFE_SPOOL_SLICE_GLYPHS, MarkdownMeasuredItemSpoolProof, MarkdownSafeGlyphSlice,
    MarkdownSafeGlyphSliceError, MarkdownSafeGlyphSliceFacts, MarkdownSafeGlyphSlices,
    MarkdownSafeSpoolSlice, MarkdownSafeSpoolSliceError, MarkdownSafeSpoolSliceJob,
    MarkdownSafeSpoolSliceStep, MarkdownSafeSpoolSliceView,
};
#[cfg(feature = "paged-line-compose")]
mod markdown_line_composer;
#[cfg(feature = "paged-line-compose")]
pub use markdown_line_composer::{
    MARKDOWN_LINE_FONT_SLOTS, MarkdownComposedCaretAssociation, MarkdownComposedCaretSide,
    MarkdownComposedGraphemeOrdinalAnswer, MarkdownComposedLine, MarkdownComposedLineAffinity,
    MarkdownComposedLineFacts, MarkdownComposedLineKind, MarkdownComposedLineLookup,
    MarkdownComposedLineLookupAnswer, MarkdownComposedLineLookupStep,
    MarkdownComposedLineLookupTarget, MarkdownComposedLinePart, MarkdownComposedLinePartFacts,
    MarkdownComposedLinePartKind, MarkdownComposedLineResourceObserver,
    MarkdownComposedLineResources, MarkdownComposedLineTrailingWhitespace,
    MarkdownComposedLineTrailingWhitespaceStep, MarkdownComposedLines,
    MarkdownComposedVisualAnswer, MarkdownComposedVisualGraphemeAnswer,
    MarkdownComposedVisualGraphemeQuery, MarkdownComposedVisualGraphemeStep,
    MarkdownComposedVisualQuery, MarkdownComposedVisualStep, MarkdownLineComposeAnswer,
    MarkdownLineComposeBudget, MarkdownLineComposeCancellation, MarkdownLineComposeError,
    MarkdownLineComposeStep, MarkdownLineComposeStyle, MarkdownLineComposer,
    MarkdownLineComposerClusterResources, MarkdownLineComposerResolveResources,
    MarkdownLineComposerShapeResources, MarkdownLineComposerWorkerResourceObserver,
    MarkdownLineComposerWorkerResources, MarkdownLineControlResponse, MarkdownLineFontCatalog,
    MarkdownLineFontProbe, MarkdownLineFontProbeSegmentAnswer, MarkdownLineFontProbeSegmentStep,
    MarkdownLineFontRegistration, MarkdownLineInlineControl, MarkdownLineInlineControlKind,
    MarkdownLineItem, MarkdownLineItemAnswer, MarkdownLineItemCoordinator, MarkdownLineItemFacts,
    MarkdownLineItemStep, MarkdownLineParagraphStyle, MarkdownLineRegisteredFont,
    MarkdownLineStyleRequest, MarkdownLineStyleResponse, MarkdownLineTabAlignment,
    MarkdownLineTabPlacement, MarkdownLineTabStop,
};
#[cfg(feature = "paged-shaping-frontier")]
pub use markdown_line_composer::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep, MarkdownLineFrontierReason, MarkdownLineFrontierWork,
    MarkdownWordDeleteAnswer, MarkdownWordDeleteDirection, MarkdownWordDeleteStep,
    MarkdownWordGroupAction, MarkdownWordGroupAnswer, MarkdownWordGroupStep,
    MarkdownWordMovePreparation, MarkdownWordMoveScope,
};
#[cfg(feature = "paged-soft-line-measure")]
pub use markdown_soft_line_measure::{
    MARKDOWN_SOFT_LINE_BREAK_QUERIES, MARKDOWN_SOFT_LINE_CLUSTERS, MarkdownSoftLineMeasureAnswer,
    MarkdownSoftLineMeasureBudget, MarkdownSoftLineMeasureCancellation,
    MarkdownSoftLineMeasureChoice, MarkdownSoftLineMeasureError, MarkdownSoftLineMeasureJob,
    MarkdownSoftLineMeasureRequest, MarkdownSoftLineMeasureStep, MarkdownSoftLineMeasuredBreak,
    MarkdownSoftLineReshapeReason,
};
#[cfg(feature = "paged-soft-line-resolve")]
pub use markdown_soft_line_resolve::{
    MARKDOWN_SOFT_LINE_RESOLVE_CANDIDATES, MARKDOWN_SOFT_LINE_RESOLVE_GLYPHS,
    MarkdownSoftLineResolveAnswer, MarkdownSoftLineResolveBudget,
    MarkdownSoftLineResolveCancellation, MarkdownSoftLineResolveError, MarkdownSoftLineResolveJob,
    MarkdownSoftLineResolvePending, MarkdownSoftLineResolveResourceObserver,
    MarkdownSoftLineResolveResources, MarkdownSoftLineResolveStep,
    MarkdownSoftLineResolvedBoundary, MarkdownSoftLineResolvedKind, MarkdownSoftLineResolvedParts,
    MarkdownSoftLineResolvedShape, MarkdownSoftLineResolvedSide,
};
#[cfg(feature = "paged-safe-glyph-slices")]
pub use markdown_soft_line_resolve::{
    MarkdownEmergencyGlyphSliceAnswer, MarkdownEmergencyGlyphSliceBoundary,
    MarkdownEmergencyGlyphSliceJob, MarkdownEmergencyGlyphSliceLine,
    MarkdownEmergencyGlyphSliceStep, MarkdownEmergencyGlyphSliceWork,
    MarkdownEmergencyShapedPrefixBoundary,
};

// Grapheme queries expose bounded facts from the same frozen source. Core does
// not choose an editor; the plugin's production features select the paged backend.
#[cfg(feature = "paged-graphemes")]
mod markdown_graphemes;
#[cfg(feature = "paged-graphemes")]
pub use markdown_graphemes::{
    MARKDOWN_GRAPHEME_INDEX_BYTES, MARKDOWN_GRAPHEME_SOURCE_BYTES, MarkdownGraphemeAnswer,
    MarkdownGraphemeBoundary, MarkdownGraphemeBudget, MarkdownGraphemeError, MarkdownGraphemeJob,
    MarkdownGraphemeOrdinalDomain, MarkdownGraphemeQuery, MarkdownGraphemeResourceObserver,
    MarkdownGraphemeResourceSnapshot, MarkdownGraphemeStatus, MarkdownGraphemeStep,
    MarkdownGraphemeStorageProfile, MarkdownGraphemeUnit,
};

// UAX14 opportunities remain an independent algorithm contract, not a substitute
// for grapheme admission, dictionary handling, or measured font/layout authority.
#[cfg(feature = "paged-line-breaks")]
mod markdown_line_breaks;
#[cfg(feature = "paged-line-breaks")]
pub use markdown_line_breaks::{
    MARKDOWN_LINE_BREAK_INDEX_BYTES, MARKDOWN_LINE_BREAK_SOURCE_BYTES, MarkdownLineBreakAnswer,
    MarkdownLineBreakBoundary, MarkdownLineBreakBudget, MarkdownLineBreakError,
    MarkdownLineBreakJob, MarkdownLineBreakKind, MarkdownLineBreakQuery,
    MarkdownLineBreakResourceObserver, MarkdownLineBreakResourceSnapshot, MarkdownLineBreakStatus,
    MarkdownLineBreakStep, MarkdownLineBreakStorageProfile, MarkdownLineBreakUnit,
};

#[cfg(feature = "native-markdown-candidate")]
mod markdown_bidi;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_brackets;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_document_buffer;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_find;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_folding;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_viewport_presentation;
pub mod markdown_workspace;
#[cfg(feature = "native-markdown-candidate")]
#[path = "markdown_bidi/unicode_data.rs"]
mod unicode_bidi_data;

// Bidi queries prepare bounded metadata without creating a paragraph-sized
// String, glyph array, or another canonical document model.
#[cfg(feature = "native-markdown-candidate")]
pub use markdown_bidi::{
    BidiBaseDirection, BidiError, BidiMemoryBudget, MarkdownBidiCancellation,
    MarkdownBidiParagraph, MarkdownBidiPrepareBudget, MarkdownBidiPrepareJob,
    MarkdownBidiPrepareStatus, MarkdownBidiPrepareStep, MarkdownBidiResourceObserver,
    MarkdownBidiResourceSnapshot, MarkdownBidiRun, MarkdownBidiStorageProfile,
};

pub use markdown_workspace::{
    MARKDOWN_ROOT_DIRECTORY_NAME, MarkdownAttachmentImportReceipt, MarkdownNoteMutationReceipt,
    MarkdownNoteSummary, MarkdownPendingAttachmentImport, MarkdownSaveReceipt,
    MarkdownSearchAuthority, MarkdownSearchCursor, MarkdownSearchPage, MarkdownSearchResult,
    MarkdownSessionAccess, MarkdownWorkspace, MarkdownWorkspaceEntryKind, MarkdownWorkspaceError,
    MarkdownWorkspacePage, MarkdownWorkspacePageAuthority, MarkdownWorkspacePageProgress,
    MarkdownWorkspacePager, MarkdownWorkspaceSnapshot, compare_markdown_workspace_entries,
};

#[cfg(feature = "native-markdown-candidate")]
pub use markdown_brackets::{
    MARKDOWN_BRACKET_SCAN_UTF16_UNITS, MarkdownBracketMatch, MarkdownBracketQuery,
    markdown_bracket_match,
};

#[cfg(feature = "native-markdown-candidate")]
pub use markdown_document_buffer::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownDocumentRevision,
    MarkdownEditingCommand, MarkdownEditingOutcome, MarkdownFormatAction, MarkdownHistoryTextEdit,
    MarkdownLineEnding, MarkdownPreparedTextInput, MarkdownReplacement, MarkdownReplacementPart,
    MarkdownTextSelection, MarkdownTextSnapshot, MarkdownViewportSemanticStatus,
    MaterializedMarkdownViewport,
};

// Production consumers use the same bounded Root Plain/Leaf source contracts.
// Feature exposure does not grant Pending work any authority reserved for Ready.
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
pub use markdown_document_buffer::MarkdownSelectionStyleInvariant;
#[cfg(feature = "s2-table-window-prototype")]
pub use markdown_document_buffer::{
    MarkdownHeadingAnchorError, MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep,
    MarkdownHeadingAnchorTarget, MarkdownHeadingAnchorWork, MarkdownOrdinaryPendingReason,
    MarkdownOrdinarySemanticState, MarkdownRootLeafPendingReason, MarkdownViewportSourceAuthority,
    MarkdownViewportSourceRequest, MarkdownViewportSourceRequiredRowQuery,
    MarkdownViewportSourceRequiredRowResult, MarkdownViewportSourceStep,
    VerifiedMarkdownOrdinaryLeaf, VerifiedMarkdownOrdinaryLine, VerifiedMarkdownPlainViewport,
    VerifiedMarkdownRootLeafViewport, VerifiedMarkdownViewportSources,
};

// Cell drafts expose separate authority, bounded reads, and commit receipts;
// an unpublished candidate must never masquerade as the canonical Source.
#[cfg(feature = "s2-table-window-prototype")]
pub use markdown_document_buffer::{
    MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS, MARKDOWN_CELL_DRAFT_MAX_DELTA_BYTES, MarkdownCellDraft,
    MarkdownCellDraftCommit, MarkdownCellDraftCommitError, MarkdownCellDraftCommitReceipt,
    MarkdownCellDraftCommitResult, MarkdownCellDraftEditCapsule, MarkdownCellDraftError,
    MarkdownCellDraftFacts, MarkdownCellDraftHistoryDirection, MarkdownCellDraftHistoryEdit,
    MarkdownCellDraftHistoryFacts, MarkdownCellDraftInputIntent, MarkdownCellDraftInputKind,
    MarkdownCellDraftPreparedCommit, MarkdownCellDraftReadAuthority, MarkdownCellDraftReadCapsule,
    MarkdownCellDraftResourceObserver, MarkdownCellDraftResourceSnapshot,
    MarkdownCellDraftSourceAuthority, MarkdownCellDraftWordAnchor,
};

#[cfg(feature = "native-markdown-candidate")]
pub use markdown_document_buffer::changes::{
    MarkdownChangeJournalError, MarkdownChangeJournalIoFailure, MarkdownObservedEditError,
    MarkdownObservedHistoryResult, MarkdownObservedReplacementsResult, MarkdownPreparedText,
    MarkdownTextChange, MarkdownTextChangeObserver,
};

#[cfg(feature = "native-markdown-candidate")]
pub use markdown_viewport_presentation::{
    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS, MarkdownBulletMarker, MarkdownLinkDestination,
    MarkdownPresentationMode, MarkdownPresentationQuery, MarkdownPresentationRun,
    MarkdownPresentationStyles, MarkdownPreviewListMarker, MarkdownPreviewListMarkerKind,
    MarkdownSourceLink, MarkdownTableAlignment, MarkdownTableCellPrototype, MarkdownTablePrototype,
    MarkdownTableRowPrototype, MarkdownViewportPresentation, MarkdownViewportPresentationError,
    MarkdownViewportPresentationSnapshot,
};

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub use markdown_line_composer::{MarkdownLineTableRowRequest, MarkdownLineTableRowResponse};

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
pub use markdown_line_composer::MarkdownLineComposerTableResources;
