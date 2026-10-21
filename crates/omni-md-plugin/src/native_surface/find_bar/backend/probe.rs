//! Exercise actual Find through the original Body/Composer assembly and synthetic
//! source files. No substitute delegate, reveal port, or Scene provides results.

use super::*;
use crate::native_surface::find_bar::{
    NativeMarkdownFindExecution, NativeMarkdownFindExecutionError,
    NativeMarkdownPagedFindRevealRequest,
};
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedSceneKey, NativeMarkdownPagedTextScene, NativeMarkdownPagedTextViewDelegate,
    NativeMarkdownPagedTextViewError as ViewError,
};

#[path = "probe/source_body.rs"]
mod source_body;

/// Run the actual Body/Composer sequence, including remote targets and strict
/// release between fixtures. The existing AppKit instance owns the Native run loop.
pub fn run_native_paged_find_body_control(mtm: MainThreadMarker) -> Result<(), String> {
    let application = NSApplication::sharedApplication(mtm);
    if !application.isRunning() {
        // Initialize actual AppKit for the standalone Native test process; no
        // second application loop or alternate body graph is introduced.
        application.finishLaunching();
    }
    source_body::run(mtm)
}

/// Diagnose real edit transactions separately from the full sequence. Completion
/// of this entry alone establishes neither complete UI nor whole-product acceptance.
pub fn run_native_paged_find_body_transaction_control(mtm: MainThreadMarker) -> Result<(), String> {
    let application = NSApplication::sharedApplication(mtm);
    if !application.isRunning() {
        application.finishLaunching();
    }
    source_body::run_transactions(mtm)
}
