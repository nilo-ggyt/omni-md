#![cfg(target_os = "macos")]
//! AppKit assembly for the statically registered Omni MD plugin.
//!
//! Source and Preview share one paged editor and canonical MarkdownDocumentBuffer.
//! Core owns mmap-backed text, history, and confined persistence; these modules
//! own Native input, presentation, controls, and the corresponding close barrier.
//! TextKit editor/storage graphs are explicit reference probes, never fallback
//! production editors. Shared weak NSString and clipboard adapters remain live
//! production dependencies for regular-expression Find and explicit copy.

use std::cell::{Cell, OnceCell, RefCell};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use std::collections::VecDeque;
use std::ffi::c_void;
use std::io::{self, Write};
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use crate::payload_use::PayloadUseGuard;
use dispatch2::DispatchQueue;
use dispatch2::MainThreadBound;
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_app_kit::NSFontDescriptorSymbolicTraits;
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertStyle, NSApplication, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSControl,
    NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSFocusRingType, NSFont, NSFontWeightRegular, NSFontWeightSemibold,
    NSMenu, NSMenuItem, NSPanel, NSPasteboard, NSPasteboardTypeString, NSPopUpButton, NSScrollView,
    NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextFinderBarContainer, NSTextInputClient,
    NSTextView, NSTextViewDelegate, NSTitlePosition, NSView, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_core_foundation::{CFRange, CFString};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_core_text::CTFont;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNotFound, NSNotification, NSObject, NSObjectProtocol, NSPoint,
    NSRange, NSRect, NSSize, NSString, NSUInteger, NSURL, ns_string, unichar,
};
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownFormatAction, MarkdownNoteSummary, MarkdownPresentationMode,
    MarkdownPresentationStyles, MarkdownSessionAccess, MarkdownTextSelection,
    MarkdownWorkspaceSnapshot,
};
use omni_plugin_sdk::{
    OmniBarHostContext, PluginCloseRequest, PluginCloseScope, PluginHostLifecycle, PluginNativeView,
};
use thiserror::Error;

mod text_attributes;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use text_attributes::*;
pub use text_attributes::{native_markdown_document_fixed_font, native_markdown_document_font};
// GlyphInfo/TextKit presentation belongs to explicit reference fixtures; the
// production paged graph neither constructs it nor falls back to it on failure.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod list_markers;
#[cfg(feature = "native-paged-production")]
pub mod paged_body;
#[cfg(feature = "native-paged-production")]
pub mod paged_font_cascade;
#[cfg(feature = "native-paged-production")]
pub mod paged_font_fallback;
#[cfg(feature = "native-paged-production")]
pub mod paged_font_geometry;
#[cfg(feature = "native-paged-production")]
pub mod paged_fonts;
#[cfg(feature = "native-paged-production")]
pub mod paged_line_fonts;
#[cfg(feature = "native-paged-production")]
pub mod paged_line_geometry;
#[cfg(feature = "native-paged-production")]
pub mod paged_line_viewport;
#[cfg(feature = "native-paged-production")]
pub mod paged_paragraph_style;
mod text_input_policy;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use list_markers::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod preview_blocks;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use preview_blocks::*;
#[cfg(feature = "native-paged-production")]
mod preview_paragraph;
mod table_geometry;
use table_geometry::*;
// The reference table-navigation probe consumes actual Core Ready coverage;
// it cannot grant missing cells or delimiters authority to publish content.
#[cfg(all(
    feature = "native-table-row-append",
    any(test, feature = "native-markdown-runtime-probe")
))]
mod table_navigation;
#[cfg(test)]
mod table_semantics;
#[cfg(all(
    feature = "native-table-row-append",
    any(test, feature = "native-markdown-runtime-probe")
))]
use table_navigation::*;
mod conflict_bar;
mod format_strip;
mod header_layout;
mod information_panel;
use header_layout::*;
mod workspace_menu;
use workspace_menu::*;
mod workspace_menu_intent;
use workspace_menu_intent::*;
mod workspace_actions;
use workspace_actions::*;
mod modal_session;
use modal_session::*;
#[cfg(feature = "native-markdown-runtime-probe")]
mod probe;
#[cfg(all(
    feature = "native-markdown-runtime-probe",
    feature = "native-block-fragment-diagnostic"
))]
pub use probe::run_native_markdown_long_regex_control;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(crate) mod probe_call_diagnostic;
mod sidebar;
mod sidebar_geometry;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
pub(crate) mod string_read_diagnostic;
mod window;
#[cfg(all(
    feature = "native-paged-find-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use find_bar::run_native_paged_find_backend_control;
#[cfg(all(
    feature = "native-paged-body-probe",
    feature = "native-markdown-runtime-probe"
))]
pub use find_bar::run_native_paged_find_body_transaction_control;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use probe::run_native_markdown_cell_history_oracle;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use probe::{
    NativeMarkdownHostedFlow, NativeMarkdownHostedImeProbe, NativeMarkdownHostedImeStep,
    NativeMarkdownHostedStep, NativeMarkdownHostedWorkload, NativeMarkdownRuntimeProbeCompletion,
    NativeMarkdownRuntimeProbeSession, run_native_markdown_runtime_probe,
    write_native_markdown_giant_paragraph_fixture, write_native_markdown_ime_fixture,
    write_native_markdown_memory_fixture,
};
#[cfg(feature = "native-markdown-runtime-probe")]
pub use probe::{read_native_markdown_clipboard_control, run_native_markdown_clipboard_control};
use sidebar::*;
use window::*;

#[cfg(all(
    feature = "native-markdown-runtime-probe",
    feature = "native-table-row-append"
))]
pub use probe::{
    NativeMarkdownHostedTableCase, NativeMarkdownHostedTableProbe, NativeMarkdownHostedTableStep,
};
/// Emits opt-in synthetic/native diagnostics without scheduling work or retaining source text.
macro_rules! native_markdown_trace {
    ($($body:tt)*) => {
        if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
            // Diagnostics run only on explicit request; normal layout performs no formatting or output.
            $($body)*
        }
    };
}

#[cfg(feature = "native-input-trace")]
mod input_trace;
#[cfg(feature = "native-input-trace")]
pub use input_trace::{arm_native_markdown_input_trace, drain_native_markdown_input_trace};

// This shared adapter module is also used by production regex Find and copy.
// Its NSTextStorage implementation is gated internally; do not gate the whole
// module merely because its reference editor is absent from the default graph.
mod text_storage;
use text_storage::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod cache_owner;
#[cfg(feature = "native-markdown-runtime-probe")]
mod content_storage_probe;
#[cfg(feature = "native-paged-production")]
pub mod paged_glyph_paint;
#[cfg(feature = "native-paged-production")]
pub mod paged_input;
#[cfg(feature = "native-paged-production")]
pub mod paged_locations;
#[cfg(feature = "native-paged-production")]
pub mod paged_text_view;
#[cfg(feature = "native-paged-production")]
pub mod paged_viewport;
#[cfg(feature = "native-markdown-runtime-probe")]
mod viewport_reentry_diagnostic;
// Font-run scratch belongs to independent reference probes. Production paged
// painting uses its own bounded glyph and viewport-pixel owners below.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod font_run_scratch;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
mod frozen_annotation;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
mod frozen_annotation_capture;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
mod frozen_live_values;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
mod frozen_native_source;
#[cfg(all(
    feature = "native-block-fragment-diagnostic",
    feature = "native-markdown-runtime-probe"
))]
mod frozen_native_string;
// Frozen-value experiments are absent from the production editor; only explicit
// diagnostic graphs and source-preserving unit tests consume these contracts.
#[cfg(any(
    test,
    all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    )
))]
mod frozen_value_family;
#[cfg(feature = "native-raster-surface-probe")]
mod raster_surface;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod text_view;
#[cfg(any(
    feature = "native-raster-surface-probe",
    feature = "native-paged-production"
))]
mod viewport_pixels;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use text_view::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod source_decorations;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod table_editor;
#[cfg(all(
    feature = "native-table-row-append",
    any(test, feature = "native-markdown-runtime-probe")
))]
use table_editor::NativeMarkdownTableFieldOperation;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use table_editor::{
    NativeMarkdownPendingTableCell, NativeMarkdownTableCellAuthority, NativeMarkdownTableCellPoll,
    NativeMarkdownTableFieldExpectation,
};
#[cfg(feature = "native-markdown-runtime-probe")]
use table_editor::{NativeMarkdownTableCellOutcome, NativeMarkdownTableCellReceipt};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod line_source_metadata;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use line_source_metadata::{
    NativeMarkdownLineMetadataError, NativeMarkdownLineMetadataRequest,
    verify_native_markdown_line_metadata,
};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod table_layout;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use source_decorations::*;
mod fold_state;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use fold_state::*;
mod selection_state;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use selection_state::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod selection_navigation;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use selection_navigation::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod selection_mouse;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use selection_mouse::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod selection_expansion;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use selection_expansion::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
mod selection_actions;

mod gutter;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use gutter::*;
mod editor;
use editor::*;
mod panel;
use panel::*;
mod controller;
use controller::*;
mod lifecycle;
use lifecycle::*;
mod resource_ledger;
use resource_ledger::*;
#[cfg(all(
    feature = "native-markdown-runtime-probe",
    feature = "native-table-row-append"
))]
mod resource_probe;
#[cfg(all(
    feature = "native-markdown-runtime-probe",
    feature = "native-table-row-append"
))]
pub use resource_probe::{NativeMarkdownHostedResourceCase, NativeMarkdownHostedResourceProbe};
mod borrowed_editor;
use borrowed_editor::*;
mod find_bar;
use find_bar::*;
mod deallocation;
use deallocation::*;
pub(crate) use lifecycle::{
    active_native_markdown_view, request_native_markdown_close, show_native_markdown_window,
};
mod workspace_browse;
use workspace_browse::*;
mod worker_start_error;
mod workspace_browse_policy;
use worker_start_error::{NativeMarkdownWorker, native_markdown_worker_start_error};
mod workspace_commands;
use workspace_commands::*;

const NATIVE_MARKDOWN_WIDTH: f64 = 1_120.0;
const NATIVE_MARKDOWN_HEIGHT: f64 = 720.0;
const NATIVE_MARKDOWN_HEADER_HEIGHT: f64 = sidebar_geometry::HEADER_HEIGHT;
const NATIVE_MARKDOWN_SIDEBAR_WIDTH: f64 = 248.0;
// Reserve space for the persistent storage location and paging controls; the
// same bounded cursor keeps the remaining notes reachable without a full listing.
const MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES: usize = sidebar_geometry::MINIMUM_PAGE_ROWS;
const MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES: usize = 512;
const MAX_NATIVE_MARKDOWN_STATUS_CHARACTERS: usize = 160;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES: usize = 128;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const MAX_NATIVE_MARKDOWN_STYLE_FONT_CACHE_ENTRIES: usize = 128;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const MAX_NATIVE_MARKDOWN_FALLBACK_FONT_CACHE_ENTRIES: usize = 16;
// TextKit asks for attributes one index at a time while laying out. Returning
// a small, scalar-aligned window keeps those calls bounded without creating a
// document-sized attribute run or retaining a second text representation.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS: usize = 4 * 1_024;
const NATIVE_MARKDOWN_TABLE_ROW_HEIGHT: f64 = 38.0;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT: f64 = 1.0;
const NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING: f64 = 10.0;
const NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING: f64 = 8.0;
const NATIVE_MARKDOWN_TABLE_MINIMUM_COLUMN_WIDTH: f64 = 84.0;
const NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH: f64 = 280.0;

/// Queues the same main-thread lifecycle/layout transaction in production and native probes.
fn enqueue_native_markdown_main_task<F>(work: F)
where
    F: Send + FnOnce() + 'static,
{
    // Dispatch owns the closure until this main-queue transaction runs. Drain
    // Native temporaries in that same transaction instead of the outer app pool;
    // callers retain original weak identities rather than keeping closed sessions alive.
    DispatchQueue::main().exec_async(move || autoreleasepool(|_| work()));
}

type SharedMarkdownDocument = Rc<RefCell<MarkdownDocumentBuffer>>;
type WeakMarkdownDocument = std::rc::Weak<RefCell<MarkdownDocumentBuffer>>;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
type MarkdownTextAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_app_kit::{
    NSBackgroundColorAttributeName, NSBaselineOffsetAttributeName, NSBezierPath,
    NSFontAttributeName, NSForegroundColorAttributeName, NSGlyphInfo, NSGlyphInfoAttributeName,
    NSGraphicsContext, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSStrikethroughStyleAttributeName, NSTextContainer, NSTextDelegate, NSTextElementProvider,
    NSTextLayoutManager, NSTextRange, NSTextStorage, NSTextStorageEditActions,
    NSTextStorageObserving, NSUnderlineStyle, NSUnderlineStyleAttributeName,
};

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSCharacterSet, NSDictionary, NSInsetRect,
    NSIntersectsRect, NSMutableAttributedString, NSNumber, NSPointInRect,
};

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use omni_markdown_core::{
    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS, MarkdownBulletMarker, MarkdownEditingCommand,
    MarkdownEditingOutcome, MarkdownHistoryTextEdit, MarkdownPreviewListMarker,
    MarkdownPreviewListMarkerKind, MarkdownTableAlignment, MarkdownTablePrototype,
    MarkdownViewportPresentation, MarkdownViewportPresentationSnapshot,
    MarkdownViewportSemanticStatus,
};
