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

// 一个冻结源胶囊供独立布局算法共用；只捕获资源引用，不触发算法或Native排版。
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

// 脚本分段复用同版字素状态与原冻结Source，不按读取窗口制造新的shaping边界。
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

// 精确字体解析与分页字形仅在显式候选路径编译，AppKit不进入这个纯Core依赖图。
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

// 宽度阶段只交付真实候选与尚需完成的工作，最终软行发布由后继组合器拥有。
#[cfg(feature = "paged-soft-line-measure")]
mod markdown_soft_line_measure;
#[cfg(feature = "paged-soft-line-resolve")]
mod markdown_soft_line_resolve;
// 切片只能消费内部真实测量证明；公开Range不授予原字形范围的交付权限。
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

// 分块字素仅提供同一冻结源的有限边界；默认Native路径不因该模块存在而切换。
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

// 默认UAX14机会独立opt-in；它不代替字素过滤、词典或Native宽度布局。
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

// UBA元数据与有界准备协议；公开查询不会创建整段String、字形或另一个文档模型。
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
    MarkdownWorkspacePager, MarkdownWorkspaceSnapshot, OpenMarkdownNote,
    compare_markdown_workspace_entries,
};

// 原生适配层从crate根命名同一组恢复类型；该导出不建立第二协议或另一份实现。
#[cfg(feature = "native-markdown-candidate")]
pub use markdown_workspace::{
    MarkdownRecoveryHandoff, MarkdownRecoveryPreparation, MarkdownRecoveryRecording,
    MarkdownRecoveryResolutionOutcome,
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

// 有限来源与 Root Plain/Leaf 的 opt-in 出口不切换默认 Native；Pending 不能取得任何 Ready 权限。
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

// 单格草稿只公开独立权限、有限读取和提交收据；不把未提交候选冒充canonical Source。
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
