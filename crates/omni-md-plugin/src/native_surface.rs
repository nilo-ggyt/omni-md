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

use dispatch2::DispatchQueue;
use dispatch2::MainThreadBound;
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication,
    NSAttributedStringNSExtendedStringDrawing, NSAutoresizingMaskOptions,
    NSBackgroundColorAttributeName, NSBackingStoreType, NSBaselineOffsetAttributeName,
    NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlTextEditingDelegate,
    NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSFocusRingType, NSFont, NSFontAttributeName,
    NSFontDescriptorSymbolicTraits, NSFontWeightRegular, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSGraphicsContext, NSMenu, NSMenuItem, NSMutableParagraphStyle,
    NSPanel, NSParagraphStyleAttributeName, NSPasteboard, NSPasteboardTypeString, NSPopUpButton,
    NSScrollView, NSStrikethroughStyleAttributeName, NSStringDrawingOptions, NSTextAlignment,
    NSTextBlock, NSTextBlockLayer, NSTextBlockValueType, NSTextContainer, NSTextDelegate,
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
    NSDictionary, NSInsetRect, NSIntersectsRect, NSMutableAttributedString, NSMutableString,
    NSNotFound, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSPointInRect,
    NSRange, NSRect, NSRectEdge, NSSize, NSString, NSUInteger, NSURL, ns_string, unichar,
};
use omni_markdown_core::{
    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS, MarkdownDocumentBuffer, MarkdownEditingCommand,
    MarkdownEditingOutcome, MarkdownFormatAction, MarkdownHistoryTextEdit, MarkdownNoteSummary,
    MarkdownPresentationMode, MarkdownPresentationStyles, MarkdownSessionAccess,
    MarkdownTableAlignment, MarkdownTablePrototype, MarkdownTextSelection,
    MarkdownViewportPresentation, MarkdownViewportSemanticStatus, MarkdownWorkspaceSnapshot,
};
use omni_plugin_sdk::{
    OmniBarHostContext, PluginCloseRequest, PluginCloseScope, PluginHostLifecycle, PluginNativeView,
};
use thiserror::Error;

mod text_attributes;
use text_attributes::*;
mod table_geometry;
use table_geometry::*;
mod workspace_actions;
use workspace_actions::*;
#[cfg(feature = "native-markdown-runtime-probe")]
mod probe;
mod sidebar;
mod window;
#[cfg(feature = "native-markdown-runtime-probe")]
pub use probe::{
    NativeMarkdownRuntimeProbeCompletion, NativeMarkdownRuntimeProbeSession,
    run_native_markdown_runtime_probe,
};
use sidebar::*;
use window::*;

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
mod text_view;
use text_view::*;
mod source_decorations;
mod table_editor;
mod table_layout;
use source_decorations::*;
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
mod deallocation;
use deallocation::*;
pub(crate) use lifecycle::{
    active_native_markdown_view, request_native_markdown_close, show_native_markdown_window,
};
mod workspace_commands;
use workspace_commands::*;

const NATIVE_MARKDOWN_WIDTH: f64 = 1_120.0;
const NATIVE_MARKDOWN_HEIGHT: f64 = 720.0;
const NATIVE_MARKDOWN_HEADER_HEIGHT: f64 = 54.0;
const NATIVE_MARKDOWN_SIDEBAR_WIDTH: f64 = 248.0;
const MAX_VISIBLE_NATIVE_MARKDOWN_WORKSPACE_ENTRIES: usize = 18;
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
    // Dispatch owns the closure until the next main-queue turn; callers carry weak native
    // identities when work must not extend a foreground session's lifetime.
    DispatchQueue::main().exec_async(work);
}

type SharedMarkdownDocument = Rc<RefCell<MarkdownDocumentBuffer>>;
type WeakMarkdownDocument = std::rc::Weak<RefCell<MarkdownDocumentBuffer>>;
type MarkdownTextAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;
