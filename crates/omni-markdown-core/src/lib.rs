//! Markdown document, vault, and bounded presentation contracts owned by the plugin.
//!
//! This core has no Host, Tauri, WebView, or AppKit dependency. Explicit foreground
//! sessions own mmap/piece-tree editing history and confined file operations;
//! viewport semantics retain bounded metadata rather than a second document model.

mod immutable_snapshot;

#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_brackets;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_document_buffer;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_folding;
#[cfg(feature = "native-markdown-candidate")]
pub mod markdown_viewport_presentation;
pub mod markdown_workspace;

pub use markdown_workspace::{
    MARKDOWN_ROOT_DIRECTORY_NAME, MarkdownAttachmentImportReceipt, MarkdownNoteMutationReceipt,
    MarkdownNoteSummary, MarkdownPendingAttachmentImport, MarkdownSaveReceipt,
    MarkdownSearchResult, MarkdownSessionAccess, MarkdownWorkspace, MarkdownWorkspaceError,
    MarkdownWorkspacePage, MarkdownWorkspacePager, MarkdownWorkspaceSnapshot, OpenMarkdownNote,
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
    MarkdownLineEnding, MarkdownTextSelection, MarkdownViewportSemanticStatus,
    MaterializedMarkdownViewport,
};

#[cfg(feature = "native-markdown-candidate")]
pub use markdown_viewport_presentation::{
    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS, MarkdownPresentationMode,
    MarkdownPresentationQuery, MarkdownPresentationRun, MarkdownPresentationStyles,
    MarkdownTableAlignment, MarkdownTableCellPrototype, MarkdownTablePrototype,
    MarkdownTableRowPrototype, MarkdownViewportPresentation, MarkdownViewportPresentationError,
};
