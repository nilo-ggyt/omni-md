#![cfg(target_os = "macos")]
//! Native AppKit/TextKit 2 surface for the statically linked Omni MD plugin.
//!
//! MarkdownDocumentBuffer is the sole text owner. Objective-C strings and storage
//! provide main-thread UTF-16 adapters; no full-document mutable mirror is retained.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::io::{self, Write};
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::payload_use::PayloadUseGuard;
use dispatch2::DispatchQueue;
use dispatch2::MainThreadBound;
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertStyle, NSApplication, NSAutoresizingMaskOptions,
    NSBackgroundColorAttributeName, NSBackingStoreType, NSBaselineOffsetAttributeName,
    NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlTextEditingDelegate,
    NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSFocusRingType, NSFont, NSFontAttributeName,
    NSFontDescriptorSymbolicTraits, NSFontWeightRegular, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSGlyphInfo, NSGlyphInfoAttributeName, NSGraphicsContext,
    NSMenu, NSMenuItem, NSMutableParagraphStyle, NSPanel, NSParagraphStyleAttributeName,
    NSPasteboard, NSPasteboardTypeString, NSPopUpButton, NSScrollView,
    NSStrikethroughStyleAttributeName, NSTextAlignment, NSTextContainer, NSTextDelegate,
    NSTextElementProvider, NSTextField, NSTextFieldDelegate, NSTextFinderBarContainer,
    NSTextInputClient, NSTextLayoutManager, NSTextRange, NSTextStorage, NSTextStorageEditActions,
    NSTextStorageObserving, NSTextView, NSTextViewDelegate, NSTitlePosition, NSUnderlineStyle,
    NSUnderlineStyleAttributeName, NSView, NSWindowAnimationBehavior, NSWindowCollectionBehavior,
    NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFRange, CFString};
use objc2_core_text::CTFont;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSCharacterSet,
    NSDictionary, NSInsetRect, NSIntersectsRect, NSMutableAttributedString, NSNotFound,
    NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSPointInRect, NSRange, NSRect,
    NSSize, NSString, NSUInteger, NSURL, ns_string, unichar,
};
use omni_markdown_core::{
    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS, MarkdownBulletMarker, MarkdownDocumentBuffer,
    MarkdownEditingCommand, MarkdownEditingOutcome, MarkdownFormatAction, MarkdownHistoryTextEdit,
    MarkdownNoteSummary, MarkdownPresentationMode, MarkdownPresentationStyles,
    MarkdownPreviewListMarker, MarkdownPreviewListMarkerKind, MarkdownSessionAccess,
    MarkdownTableAlignment, MarkdownTablePrototype, MarkdownTextSelection,
    MarkdownViewportPresentation, MarkdownViewportPresentationSnapshot,
    MarkdownViewportSemanticStatus, MarkdownWorkspaceSnapshot,
};
use omni_plugin_sdk::{
    OmniBarHostContext, PluginCloseRequest, PluginCloseScope, PluginHostLifecycle, PluginNativeView,
};
use thiserror::Error;

mod text_attributes;
use text_attributes::*;
// 候选字体适配器只供精确机制验收，默认编辑图不因模块存在而分配字体快照。
mod list_markers;
#[cfg(feature = "native-paged-body-probe")]
pub mod paged_body;
#[cfg(feature = "native-paged-font-probe")]
pub mod paged_font_cascade;
#[cfg(feature = "native-paged-font-fallback-probe")]
pub mod paged_font_fallback;
#[cfg(feature = "native-paged-font-probe")]
pub mod paged_font_geometry;
#[cfg(feature = "native-paged-font-probe")]
pub mod paged_fonts;
#[cfg(feature = "native-paged-compose-probe")]
pub mod paged_line_fonts;
#[cfg(feature = "native-paged-compose-probe")]
pub mod paged_line_geometry;
#[cfg(feature = "native-paged-compose-probe")]
pub mod paged_line_viewport;
#[cfg(feature = "native-paged-compose-probe")]
pub mod paged_paragraph_style;
mod text_input_policy;
use list_markers::*;
mod preview_blocks;
use preview_blocks::*;
mod table_geometry;
use table_geometry::*;
// 默认消费者切换前，用真实 Core Ready 验证分段覆盖、缺格与分隔行的读取权限。
#[cfg(feature = "native-table-row-append")]
mod table_navigation;
#[cfg(test)]
mod table_semantics;
#[cfg(feature = "native-table-row-append")]
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
    NativeMarkdownHostedStep, NativeMarkdownHostedWorkload, NativeMarkdownMemoryWorkload,
    NativeMarkdownRuntimeProbeCompletion, NativeMarkdownRuntimeProbeSession,
    run_native_markdown_runtime_probe,
};
#[cfg(feature = "native-markdown-runtime-probe")]
pub use probe::{read_native_markdown_clipboard_control, run_native_markdown_clipboard_control};
use sidebar::*;
use window::*;

#[cfg(all(
    feature = "native-markdown-runtime-probe",
    feature = "native-table-row-append"
))]
pub use probe::{NativeMarkdownTableAppendCase, NativeMarkdownTableAppendWorkload};
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
use crate::native_input_trace::{NativeInputTrace, SharedNativeInputTrace, TraceScope};
#[cfg(feature = "native-input-trace")]
use crate::{NativeInputTraceDocument, NativeInputTraceRejection, NativeInputTraceSelector};
#[cfg(feature = "native-input-trace")]
use input_trace::*;
#[cfg(feature = "native-input-trace")]
pub use input_trace::{arm_native_markdown_input_trace, drain_native_markdown_input_trace};

mod text_storage;
use text_storage::*;
mod cache_owner;
#[cfg(feature = "native-markdown-runtime-probe")]
mod content_storage_probe;
#[cfg(feature = "native-paged-layout-probe")]
pub mod paged_glyph_paint;
#[cfg(feature = "native-paged-layout-probe")]
pub mod paged_input;
#[cfg(feature = "native-paged-layout-probe")]
pub mod paged_locations;
#[cfg(feature = "native-paged-layout-probe")]
pub mod paged_text_view;
#[cfg(feature = "native-paged-layout-probe")]
pub mod paged_viewport;
#[cfg(feature = "native-markdown-runtime-probe")]
mod viewport_reentry_diagnostic;
// 光栅路径尚在真实编辑器中验收；普通构建不包含该试验模块。
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
// 普通Native路径没有冻结值构造者；仅真实诊断图与同源单测需要其观察合同。
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
mod text_view;
#[cfg(any(
    feature = "native-raster-surface-probe",
    feature = "native-paged-layout-probe"
))]
mod viewport_pixels;
use text_view::*;
mod source_decorations;
mod table_editor;
#[cfg(feature = "native-table-row-append")]
use table_editor::NativeMarkdownTableFieldOperation;
use table_editor::{
    NativeMarkdownPendingTableCell, NativeMarkdownTableCellAuthority, NativeMarkdownTableCellPoll,
    NativeMarkdownTableFieldExpectation,
};
#[cfg(feature = "native-markdown-runtime-probe")]
use table_editor::{
    NativeMarkdownTableCellOutcome, NativeMarkdownTableCellReceipt,
    NativeMarkdownTableInsertionObservation,
};
mod line_source_metadata;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use line_source_metadata::{
    NativeMarkdownLineMetadataError, NativeMarkdownLineMetadataRequest,
    verify_native_markdown_line_metadata,
};
mod table_layout;
use source_decorations::*;
mod fold_state;
use fold_state::*;
mod selection_state;
use selection_state::*;
mod selection_navigation;
use selection_navigation::*;
mod selection_mouse;
use selection_mouse::*;
mod selection_expansion;
use selection_expansion::*;
mod selection_actions;

mod gutter;
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
#[cfg(feature = "native-markdown-runtime-probe")]
mod resource_probe;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(crate) use resource_probe::{
    NATIVE_MARKDOWN_RESOURCE_PROBE_CASES, NativeMarkdownResourceProbe,
};
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
// 留出常驻保存位置与分页控件的空间，剩余笔记仍由同一游标逐页可达。
const MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES: usize = sidebar_geometry::MINIMUM_PAGE_ROWS;
const MAX_SCANNED_NATIVE_MARKDOWN_WORKSPACE_ENTRIES: usize = 512;
const MAX_NATIVE_MARKDOWN_STATUS_CHARACTERS: usize = 160;
const MAX_NATIVE_MARKDOWN_ATTRIBUTE_CACHE_ENTRIES: usize = 128;
const MAX_NATIVE_MARKDOWN_STYLE_FONT_CACHE_ENTRIES: usize = 128;
const MAX_NATIVE_MARKDOWN_FALLBACK_FONT_CACHE_ENTRIES: usize = 16;
// TextKit asks for attributes one index at a time while laying out. Returning
// a small, scalar-aligned window keeps those calls bounded without creating a
// document-sized attribute run or retaining a second text representation.
const NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS: usize = 4 * 1_024;
const NATIVE_MARKDOWN_TABLE_ROW_HEIGHT: f64 = 38.0;
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
    // Dispatch 持有闭包至本次主队列事务执行；其中的 Native 临时对象必须在同一事务结束时
    // 归还，不能依赖外围应用池。调用方仍只捕获原弱身份，不增加事件或延长关闭会话。
    DispatchQueue::main().exec_async(move || autoreleasepool(|_| work()));
}

type SharedMarkdownDocument = Rc<RefCell<MarkdownDocumentBuffer>>;
type WeakMarkdownDocument = std::rc::Weak<RefCell<MarkdownDocumentBuffer>>;
type MarkdownTextAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;
