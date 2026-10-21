//! Attach the authenticated cell to the same Body/Source/paint chain. Large drafts
//! pass through neither NSTextField nor a complete-draft NSString.

use super::*;
use crate::native_surface::paged_input::{
    NativeMarkdownPagedCellSession, NativeMarkdownPagedInput, NativeMarkdownPagedInputHistory,
    NativeMarkdownPagedInputOutcome,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_graphics::CGContext;
use omni_markdown_core::markdown_document_buffer::MarkdownTableCellEditStep;
use omni_markdown_core::{MarkdownCellDraftResourceObserver, MarkdownFormatAction};
use std::io::Write;

const SOURCE: &str = "| H | C |\n| --- | --- |\n| seed | other |\n";
const LARGE: usize = 2 * 1024 * 1024;
const SCRATCH: usize = 64 * 1024;

#[path = "cell/word.rs"]
mod word;

/// The actual cell's canonical owner and original commit entry. This owner keeps
/// the document alive while presentation Input retains only weak identity.
pub(super) struct Original {
    document: Rc<RefCell<MarkdownDocumentBuffer>>,
    canonical: Rc<NativeMarkdownPagedInput>,
    pub(super) session: Rc<NativeMarkdownPagedCellSession>,
    resources: MarkdownCellDraftResourceObserver,
}

impl Original {
    /// Observe actual original-token release. Only completed commit/cancellation
    /// permits skipping normal resignation reads from an ended draft.
    pub(super) fn has_ended(&self) -> bool {
        self.resources.is_released()
    }

    /// Obtain the cell plan through Core's actual incremental request, then begin
    /// the same prepared session production uses; no manufactured cell window is supplied.
    pub(super) fn new(document: Rc<RefCell<MarkdownDocumentBuffer>>) -> Result<Self, String> {
        let canonical = NativeMarkdownPagedInput::new(Rc::downgrade(&document))
            .map_err(|cause| cause.to_string())?;
        let plan = {
            let doc = document.borrow();
            let mut request = doc
                .table_cell_edit_request(0, 1, 0)
                .map_err(|cause| format!("{cause:?}"))?;
            loop {
                // Consume the original cell's actual structural ticket and 4KiB
                // lexical endpoints, without full-row parsing or a fabricated window.
                match request
                    .advance(&doc, false, 4096)
                    .map_err(|cause| format!("{cause:?}"))?
                {
                    MarkdownTableCellEditStep::Progress { .. } => {}
                    MarkdownTableCellEditStep::Ready { .. } => {
                        break request
                            .take_plan_for(&doc)
                            .map_err(|cause| format!("{cause:?}"))?
                            .ok_or("原Cell编辑计划未交付")?;
                    }
                    other => return Err(format!("原Cell编辑计划未Ready：{other:?}")),
                }
            }
        };
        let session =
            NativeMarkdownPagedCellSession::begin_prepared(Rc::downgrade(&document), plan)
                .map_err(|cause| cause.to_string())?;
        let resources = session
            .resource_observer()
            .map_err(|cause| cause.to_string())?;
        Ok(Self {
            document,
            canonical,
            session,
            resources,
        })
    }

    /// Body closure has committed/cancelled and drained the original token. The
    /// parent canonical document must still be alive with a healthy original mapping.
    pub(super) fn verify_closed(&self) -> Result<(), String> {
        if !self.resources.is_released()
            || self
                .document
                .borrow()
                .original_backing_observer()
                .is_none_or(|backing| backing.failed() || backing.mapped_bytes() == 0)
        {
            return Err(format!(
                "Cell显示关闭后原草稿未归还或parent Doc提前释放：{:?}",
                self.resources.snapshot()
            ));
        }
        self.canonical.close().map_err(|cause| cause.to_string())?;
        println!(
            "native_body_cell stage=closed parent_doc_alive=true actual_cell_released=true resources={:?} formal=false whole_product=false",
            self.resources.snapshot()
        );
        Ok(())
    }
}

fn input(rig: &Rig) -> Result<Rc<NativeMarkdownPagedInput>, String> {
    rig.body
        .probe_find_input_weak()
        .upgrade()
        .ok_or("原Cell Input已释放".into())
}

/// Set this actual cell's single selection and consume its commit receipt, never
/// inventing Body multi-selection or offscreen visibility authority.
fn select(rig: &Rig, range: std::ops::Range<usize>) -> Result<(), String> {
    let input = input(rig)?;
    let before = input.state().map_err(|cause| cause.to_string())?;
    let geometry = rig.view.find_geometry_epoch().ok_or("原Cell几何已释放")?;
    let current = |epoch| {
        rig.view
            .find_input_is_current(&input, epoch, &before, Some(geometry))
    };
    let permit = input
        .capture(EPOCH, &current)
        .map_err(|cause| cause.to_string())?;
    match input
        .set_selected_ranges(&permit, &[range], &current)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownPagedInputOutcome::Committed(commit) => rig
            .view
            .apply_find_commit(&input, &commit)
            .map_err(|cause| cause.to_string()),
        NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
    }
}

fn client(rig: &Rig) -> &ProtocolObject<dyn NSTextInputClient> {
    ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view)
}

fn unspecified() -> NSRange {
    NSRange::new(objc2_foundation::NSNotFound as usize, 0)
}

/// Read only the caller's fixed cell-local window through actual Input authority,
/// keeping the draft independent from its parent canonical document.
fn local<const N: usize>(rig: &Rig, offset: usize) -> Result<[u16; N], String> {
    let input = input(rig)?;
    let permit = input
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let mut units = [0; N];
    input
        .copy_utf16_code_units(&permit, offset, &mut units, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    Ok(units)
}

/// Copying from the active Scene returns its temporary reader. Output NSData is
/// independently owned and must remain readable after the original cell ends.
struct Copied {
    board: Retained<NSPasteboard>,
    data: Retained<objc2_foundation::NSData>,
}
impl Drop for Copied {
    fn drop(&mut self) {
        release_board(&self.board); // Later failure clears only this case's board; output NSData then drops through its owner.
    }
}

/// Copy through the actual Body to a unique private board, verifying exact bytes
/// and unchanged Input/Scene/reader facts before transferring output ownership.
fn copy(rig: &Rig, large: bool) -> Result<Copied, String> {
    let original = rig.cell.as_ref().ok_or("缺少原Cell owner")?;
    let input = input(rig)?;
    let before = input.state().map_err(|cause| cause.to_string())?;
    let resources = original.resources.snapshot();
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    // Record this owned name before publication so interrupted controls remain
    // cleanable; Copied keeps the normal lifetime and exact-output checks below.
    let board = private_pasteboard()?;
    let result = (|| {
        rig.body
            .copy_source_selection_to(&key.request, &board)
            .map_err(|cause| cause.to_string())?;
        // SAFETY: Use production's same static plain-text type on this private
        // pasteboard, never the user's general board.
        let data = board
            .dataForType(unsafe { NSPasteboardTypeString })
            .ok_or("原Cell复制没有NSData")?;
        verify_output(&data, large)?;
        if input.state().map_err(|cause| cause.to_string())? != before
            || original.resources.snapshot().read_roots != resources.read_roots
            || !rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?
                .same(&key)
        {
            return Err("Cell复制改变了原选择/Scene或遗留临时reader".into());
        }
        Ok(data)
    })();
    match result {
        Ok(data) => Ok(Copied { board, data }),
        Err(cause) => {
            release_board(&board);
            Err(cause)
        }
    }
}

fn verify_output(data: &objc2_foundation::NSData, large: bool) -> Result<(), String> {
    let expected = if large { LARGE } else { 3 }; // The short selection is one Chinese scalar; the large draft is actual 2MiB ASCII.
    if data.length() != expected {
        return Err("原Cell输出UTF8长度不符".into());
    }
    // SAFETY: The caller's retained NSData outlives this synchronous read; send
    // no Native message that could mutate its bytes.
    let bytes = unsafe { data.as_bytes_unchecked() };
    if large {
        for chunk in bytes.chunks(SCRATCH) {
            if chunk.iter().any(|byte| *byte != b'x') {
                return Err("原Cell大草稿输出字节不符".into());
            }
        } // Fixed chunk comparison creates no second complete-cell expected String/NSString.
    } else if bytes != "你".as_bytes() {
        return Err("原Cell部分复制没有保持Unicode".into());
    }
    Ok(())
}

fn release_board(board: &NSPasteboard) {
    board.clearContents();
    // SAFETY: Release only this case's uniquely named owned board, never the user's general board.
    unsafe {
        let _: () = objc2::msg_send![board, releaseGlobally];
    }
}

/// Cell-local edits must leave the fixed parent document and its character history
/// untouched until the explicit canonical commit.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.cell.as_ref().ok_or("缺少原Cell owner")?;
    let doc = original.document.borrow();
    let mut bytes = [0_u16; 64];
    let len = doc.utf16_len();
    if len > bytes.len() {
        return Err("Cell夹具canonical意外变长".into());
    }
    doc.copy_utf16_code_units(0, &mut bytes[..len])
        .map_err(|cause| cause.to_string())?;
    if doc.generation() != 0
        || doc.can_undo()
        || doc.can_redo()
        || bytes[..len].iter().copied().ne(SOURCE.encode_utf16())
    {
        return Err("局部Cell输入提前改变canonical或历史".into());
    }
    Ok(())
}

/// Verify the same canonical document after accepting the 2MiB draft. Fixed 4KiB
/// scratch reads compare all contents without a complete-cell/document expected copy.
fn verify_large_canonical(original: &Original, generation: u64) -> Result<(), String> {
    let (prefix, suffix) = SOURCE.split_once("seed").expect("固定夹具含唯一原格");
    let prefix_units = prefix.encode_utf16().count();
    let suffix_units = suffix.encode_utf16().count();
    let document = original.document.borrow();
    if document.generation() != generation
        || document.byte_len() != prefix.len() + LARGE + suffix.len()
        || document.utf16_len() != prefix_units + LARGE + suffix_units
        || document.selection() != MarkdownTextSelection::caret(prefix_units + LARGE)
        || !document.can_undo()
        || document.can_redo()
    {
        return Err("两MiBCell的原canonical长度、代次、选择或单次历史状态不符".into());
    }
    let mut units = [0_u16; 2048];
    for (offset, expected) in [(0, prefix), (prefix_units + LARGE, suffix)] {
        // Compare both ends unit for unit, so checking middle x characters cannot
        // conceal damaged neighboring cells, separators, or original line endings.
        let length = expected.encode_utf16().count();
        document
            .copy_utf16_code_units(offset, &mut units[..length])
            .map_err(|cause| cause.to_string())?;
        if units[..length].iter().copied().ne(expected.encode_utf16()) {
            return Err("两MiBCell提交改变了原表头、邻格或行分隔符".into());
        }
    }
    for offset in (0..LARGE).step_by(units.len()) {
        // Every block, including start/middle/end, reads the same canonical source;
        // preserve read failure instead of downgrading full verification to sampling.
        let length = units.len().min(LARGE - offset);
        document
            .copy_utf16_code_units(prefix_units + offset, &mut units[..length])
            .map_err(|cause| cause.to_string())?;
        if units[..length].iter().any(|unit| *unit != u16::from(b'x')) {
            return Err("两MiBCell提交没有完整保存原草稿字节".into());
        }
    }
    Ok(())
}

/// The active head must actually be visible. Compare insertion-line pixels for a
/// caret or highlight pixels for a nonempty selection against the same actual CPU mount.
fn selection_pixels(
    rig: &Rig,
    stage: &str,
    selected: MarkdownTextSelection,
) -> Result<NativeMarkdownPagedSceneKey, String> {
    let input = input(rig)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let head = selected.head_utf16;
    let range = NSRange::new(head, 0);
    let primary = NSRange::new(
        selected.anchor_utf16.min(head),
        selected.anchor_utf16.abs_diff(head),
    );
    let empty_document = key.request.document.upgrade().is_none();
    let same_domain = input
        .target_domain()
        .map_err(|cause| cause.to_string())?
        .same(&key.request.target);
    println!(
        "native_body_cell stage={stage} actual_state={:?} expected_selection={selected:?} expected_primary={primary:?} canonical_weak_empty={empty_document} actual_domain_matches={same_domain}",
        key.request.input
    );
    if key.request.input.primary_range != primary
        || key.request.input.directed_selection() != Some(selected)
        || !empty_document
        || !same_domain
    {
        return Err(format!("{stage}: 原Cell caret或空DocWeak/真实domain不符"));
    }
    let proof = rig
        .view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    if !proof.key().same(&key) || proof.range() != range {
        return Err("原Cell可见证明未绑定同一Scene/范围".into());
    }
    let paints = rig.view.cpu_paints().ok_or("原Cell没有CPU绘制owner")?;
    let current = || {
        NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &key)
            && rig.view.cpu_paints() == Some(paints)
            && rig.view.cpu_paint_pending() == Some(false)
    };
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    if !scene.key().same(&key) || !current() {
        return Err("原Cell不是已发布的同一完整帧".into());
    }
    let scale = rig.panel.backingScaleFactor();
    let actual = selection::Canvas::scaled(key.request.viewport, scale)?;
    let rendered = selection::Canvas::scaled(key.request.viewport, scale)?;
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    let steps = selection::paint_scene(scene.clone(), &key, &actual, &current)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    if primary.length == 0 {
        let mut rect = proof.first_rect();
        rect.size.width = 1.0; // Actual caret geometry permits zero width; production paints its insertion line at 1pt.
        CGContext::fill_rect(Some(&mask.context), rect);
    } else {
        let end = primary.location + primary.length;
        let mut cursor = primary.location;
        while cursor < end {
            // first_rect returns only the first grapheme, not geometry for the
            // entire requested selection; this seed has at most four graphemes.
            let first = scene
                .first_rect(NSRange::new(cursor, end - cursor))
                .map_err(|cause| cause.to_string())?;
            let next = first.actual_range.location + first.actual_range.length;
            if first.actual_range.location != cursor || next <= cursor || next > end {
                return Err("原Cell非空选择缺少同Source的完整GCB前进范围".into());
            }
            CGContext::fill_rect(Some(&mask.context), first.rect);
            cursor = next;
        }
    }
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("原Cell缺少实际CPU backing")?;
    // SAFETY: This stack keeps the actual main-thread CALayer alive. Observe its
    // existing view-mounted children without constructing a shadow product graph.
    let children = unsafe { backing.sublayers() }.ok_or("原Cell没有CPU挂载")?;
    if children.len() != 1 {
        return Err("原Cell不是唯一CPU挂载".into());
    }
    let mount = children.objectAtIndex(0);
    if mount.frame() != bounds
        || mount.bounds() != NSRect::new(NSPoint::ZERO, bounds.size)
        || !current()
    {
        return Err("原Cell实际挂载/坐标已改变".into());
    }
    CGContext::translate_ctm(Some(&rendered.context), bounds.origin.x, bounds.origin.y);
    mount.renderInContext(&rendered.context);
    for canvas in [&actual, &rendered, &mask] {
        CGContext::flush(Some(&canvas.context));
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    let selection_color = selection::selection_pixel()?; // Independent actual Native calibration, not guessed integer colors.
    let mut locations = 0;
    let mut ink = 0;
    let mut mismatch = None;
    for (index, ((expected, output), marker)) in actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rendered.pixels.as_chunks::<4>().0)
        .zip(mask.pixels.as_chunks::<4>().0)
        .enumerate()
    {
        if *marker != [255, 0, 0, 255] {
            continue;
        } // Compare only actual visible caret/selection pixels within the independent mask, never a full-document image.
        locations += 1;
        ink += usize::from(if primary.length == 0 {
            expected[3] == 255 && expected[..3].iter().any(|value| *value < 255)
        } else {
            *expected == selection_color
        });
        if expected != output && mismatch.is_none() {
            mismatch = Some((index, *expected, *output));
        }
    }
    if locations == 0 || ink == 0 || mismatch.is_some() || !current() {
        return Err(format!(
            "{stage}: 原Cell caret像素不符 mask={locations} ink={ink} first_mismatch={mismatch:?}"
        ));
    }
    println!(
        "native_body_cell stage={stage} actual_head_utf16={head} actual_selected_units={} actual_source_bytes={} actual_document_y={} cpu_scene_layer_rgba_equal=true visible_proof=true mask_pixels={locations} caret_ink_pixels={ink} paint_steps={steps} formal=false whole_product=false",
        primary.length,
        rig.body
            .progress()
            .map_err(|cause| cause.to_string())?
            .source_total_bytes,
        key.request.virtual_viewport.document_origin.y
    );
    Ok(key)
}

/// Format through the actual view entry, then verify local selection/history in
/// the new Source's complete CPU frame; bare Input cannot substitute for the product action.
fn format_selection(rig: &Rig) -> Result<(), String> {
    let selected = observe(&rig.body)?;
    select(rig, 0..1)?;
    frame::wait_complete_frame("cell_format_selected", &rig.body, selected)?;
    let before = selection_pixels(
        rig,
        "cell_format_selected",
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 1,
        },
    )?;
    let viewport = before.request.virtual_viewport.document_origin;
    let formatted = observe(&rig.body)?;
    rig.view
        .apply_format(MarkdownFormatAction::Bold)
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_format_bold", &rig.body, formatted)?;
    let after = selection_pixels(
        rig,
        "cell_format_bold",
        MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 3,
        },
    )?;
    if local::<5>(rig, 0)? != [42, 42, 0x4f60, 42, 42]
        || after.request.input.utf16_len != 5
        || after.request.input.generation == before.request.input.generation
        || after.source == before.source
        || after.request.virtual_viewport.document_origin != viewport
    {
        return Err("View格式没有形成原Cell新Source/选中inner，或无故改变原视口".into());
    }
    unchanged(rig)?; // Formatting affects cell-local history only, never parent canonical text or neighboring cells early.
    for (selector, stage, formatted) in [
        (sel!(undo:), "cell_format_undo", false),
        (sel!(redo:), "cell_format_redo", true),
        (sel!(undo:), "cell_format_restore", false),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(selector)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let selection = if formatted {
            if local::<5>(rig, 0)? != [42, 42, 0x4f60, 42, 42]
                || rig
                    .view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .utf16_len
                    != 5
            {
                return Err("View格式Redo没有恢复真实局部标记".into());
            }
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 3,
            }
        } else {
            if local::<1>(rig, 0)? != [0x4f60]
                || rig
                    .view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .utf16_len
                    != 1
            {
                return Err("View格式单次Undo没有只还原原中文选择".into());
            }
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            }
        };
        selection_pixels(rig, stage, selection)?;
        unchanged(rig)?;
    }
    // Other inline actions also use the same actual view/frame. Core marker
    // tests alone cannot establish rendering and selection behavior.
    format_inline_variants(rig)?;
    // Restore the short fixture's caret before existing composition-protocol and
    // 2MiB scenarios, preserving subsequent input/EOF/close requirements.
    let restored = observe(&rig.body)?;
    select(rig, 1..1)?;
    frame::wait_complete_frame("cell_format_restored_caret", &rig.body, restored)?;
    selection_pixels(
        rig,
        "cell_format_restored_caret",
        MarkdownTextSelection::caret(1),
    )?;
    unchanged(rig)
}

/// Cover remaining everyday inline formats with the fixed short fixture. One Undo
/// restores original Chinese text without copying or shrinking the later large draft.
fn format_inline_variants(rig: &Rig) -> Result<(), String> {
    use MarkdownFormatAction as Action;
    for (action, expected, opening_units, stage) in [
        (Action::Italic, "*你*", 1, "cell_format_italic"),
        (Action::Underline, "<u>你</u>", 3, "cell_format_underline"),
        (Action::Strikethrough, "~~你~~", 2, "cell_format_strike"),
        (Action::InlineCode, "`你`", 1, "cell_format_inline_code"),
        (Action::Link, "[你](https://)", 1, "cell_format_link"),
    ] {
        // Previous actual Undo must retain selection of the original Chinese
        // character; acquire each action's authority from that state, not an old Scene.
        let before = selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            },
        )?;
        let rendered = observe(&rig.body)?;
        rig.view
            .apply_format(action)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, rendered)?;
        let after = selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: opening_units,
                head_utf16: opening_units + 1,
            },
        )?;
        // Read the fixed short result into 16 stack units; neither pixels nor the
        // formatter under test can supply expected text, neighboring cells, or markers.
        let input = input(rig)?;
        let current = |epoch| epoch == EPOCH;
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let length = expected.encode_utf16().count();
        let mut actual = [0_u16; 16];
        input
            .copy_utf16_code_units(&permit, 0, &mut actual[..length], &current)
            .map_err(|cause| cause.to_string())?;
        if actual[..length].iter().copied().ne(expected.encode_utf16())
            || after.request.input.utf16_len != length
            || after.source == before.source
            || after.request.input.generation == before.request.input.generation
            || after.request.virtual_viewport.document_origin
                != before.request.virtual_viewport.document_origin
        {
            return Err(format!("{stage}: 行内格式的实际源、选区或视口不符"));
        }
        unchanged(rig)?; // Local formatting cannot alter parent canonical text/history before commit.

        // Actual Undo restores all opening/closing markers in one action, followed
        // by Chinese-selection pixels in the same CPU mount, not just model length.
        let undone = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, undone)?;
        selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            },
        )?;
        if local::<1>(rig, 0)? != [0x4f60]
            || rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .utf16_len
                != 1
        {
            return Err(format!("{stage}: 行内格式没有在一次Undo后还原中文原文"));
        }
        unchanged(rig)?;
    }
    Ok(())
}

/// Exercise the actual cell protocol, local formatting/history, selected copy,
/// and large-draft commit/cancellation. Direct marked-text protocol delivery does
/// not independently verify a system IME candidate window or physical typing.
fn exercise(rig: &mut Rig, cancel: bool) -> Result<Option<Copied>, String> {
    let first = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if first.request.input.utf16_len != 4 || local::<4>(rig, 0)? != [115, 101, 101, 100] {
        return Err("原Cell首次真实完整帧没有读取seed单格".into());
    }
    // Deliver successive marked values and then committed Chinese through the
    // actual NSTextInputClient, using fixed payloads without writing parent canonical text.
    select(rig, 0..4)?;
    let before_ime = rig.view.input_state().map_err(|cause| cause.to_string())?;
    for candidate in ["n", "ni"] {
        // SAFETY: Actual view/Input remain alive. The marked selection is within
        // its bounded payload; NSNotFound consumes the original cell selection.
        unsafe {
            client(rig).setMarkedText_selectedRange_replacementRange(
                &NSString::from_str(candidate),
                NSRange::new(candidate.len(), 0),
                unspecified(),
            );
        }
    }
    let ime = observe(&rig.body)?;
    // SAFETY: Commit one Chinese scalar through the same actual client used above.
    unsafe {
        client(rig).insertText_replacementRange(&NSString::from_str("你"), unspecified());
    }
    frame::wait_complete_frame("cell_ime_frame", &rig.body, ime)?;
    if local::<1>(rig, 0)? != [0x4f60]
        || rig
            .view
            .input_state()
            .map_err(|c| c.to_string())?
            .marked
            .is_some()
    {
        return Err("Cell真实IME确认没有替换原marked候选".into());
    }
    selection_pixels(rig, "cell_ime_caret", MarkdownTextSelection::caret(1))?;
    unchanged(rig)?;
    // Standard Native commands operate only local history, without bubbling into
    // parent canonical history or constructing another undo manager.
    for (selector, stage) in [
        (sel!(undo:), "cell_local_undo"),
        (sel!(redo:), "cell_local_redo"),
    ] {
        let frame = observe(&rig.body)?;
        // SAFETY: Actual NSTextInputClient delivers a standard selector to this
        // same view's command entry, adding no shortcut.
        unsafe {
            client(rig).doCommandBySelector(selector);
        }
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if (selector == sel!(undo:)
            && (actual.utf16_len != 4 || local::<4>(rig, 0)? != [115, 101, 101, 100]))
            || (selector == sel!(redo:)
                && (actual.utf16_len != 1 || local::<1>(rig, 0)? != [0x4f60]))
        {
            return Err("Cell原生Undo/Redo没有恢复实际局部内容".into());
        }
        let restored = if selector == sel!(undo:) {
            before_ime.selection
        } else {
            MarkdownTextSelection::caret(1)
        };
        selection_pixels(rig, stage, restored)?;
        unchanged(rig)?;
    }
    if local::<1>(rig, 0)? != [0x4f60] {
        return Err("Cell本地Redo没有恢复中文草稿".into());
    }
    format_selection(rig)?;
    let marked = observe(&rig.body)?;
    // SAFETY: This bounded marked-text value uses the actual input protocol.
    // The following cancelOperation must only unmark, not close the cell session.
    unsafe {
        client(rig).setMarkedText_selectedRange_replacementRange(
            &NSString::from_str("你"),
            NSRange::new(1, 0),
            NSRange::new(0, 1),
        );
    }
    frame::wait_complete_frame("cell_marked_caret", &rig.body, marked)?;
    selection_pixels(rig, "cell_marked_caret", MarkdownTextSelection::caret(1))?;
    let unmarked = observe(&rig.body)?;
    rig.view
        .command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_unmarked_caret", &rig.body, unmarked)?;
    selection_pixels(rig, "cell_unmarked_caret", MarkdownTextSelection::caret(1))?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .marked
        .is_some()
    {
        return Err("原Cell unmark未结束组合".into());
    }
    let context = observe(&rig.body)?;
    select(rig, 0..1)?;
    // SAFETY: The actual Native entry creates a bounded draft with surrounding
    // text, so copying the whole cell could not satisfy partial-copy expectations.
    unsafe {
        client(rig).insertText_replacementRange(&NSString::from_str("前你后"), unspecified());
    }
    frame::wait_complete_frame("cell_partial_context", &rig.body, context)?;
    select(rig, 2..3)?;
    let deleted = observe(&rig.body)?;
    rig.view
        .command(sel!(deleteBackward:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_deleted_caret", &rig.body, deleted)?;
    selection_pixels(rig, "cell_deleted_caret", MarkdownTextSelection::caret(2))?;
    if local::<2>(rig, 0)? != [0x524d, 0x4f60] {
        return Err("原Cell删除没有只删除选中的后字".into());
    }
    let restored = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_delete_undo_caret", &rig.body, restored)?;
    if local::<3>(rig, 0)? != [0x524d, 0x4f60, 0x540e] {
        return Err("原Cell删除Undo没有恢复前你后".into());
    }
    // Undo restores an actual nonempty selection. Reveal its active head through
    // the same receipt rather than inventing a collapsed caret.
    let restored_key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.view
        .verify_find_target_visible(
            &restored_key,
            NSRange::new(restored_key.request.input.selection.head_utf16, 0),
        )
        .map_err(|cause| cause.to_string())?;
    let selected = observe(&rig.body)?;
    select(rig, 1..2)?;
    frame::wait_complete_frame("cell_partial_selected", &rig.body, selected)?;
    drop(copy(rig, false)?);

    let large = observe(&rig.body)?;
    select(rig, 0..3)?; // Replace the complete bounded old draft, preserving the later 2MiB fixture's exact all-x expectation.
    let payload = NSString::from_str(&"x".repeat(SCRATCH)); // One fixed 64KiB new-input payload, with no old-draft read/copy.
    for _ in 0..LARGE / SCRATCH {
        // SAFETY: The original client consumes its actual selection each time;
        // first replace Chinese, then append the same fixed payload.
        unsafe {
            client(rig).insertText_replacementRange(&payload, unspecified());
        }
    }
    drop(payload); // Return the input payload before frame waiting; product storage retains only its actual delta segments.
    frame::wait_complete_frame("cell_two_mebibyte_eof_frame", &rig.body, large)?;
    let eof = selection_pixels(
        rig,
        "cell_two_mebibyte_input_eof",
        MarkdownTextSelection::caret(LARGE),
    )?;
    let state = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if state.utf16_len != LARGE {
        return Err("真实Cell没有完整接受两MiB合法草稿".into());
    }
    for offset in [0, LARGE / 2, LARGE - 4096] {
        if local::<4096>(rig, offset)?
            .iter()
            .any(|unit| *unit != u16::from(b'x'))
        {
            return Err("大Cell有限首中尾读取不符".into());
        }
    }
    unchanged(rig)?;
    // Standard endpoint commands travel through the same large-draft Source.
    // A whole-paragraph reread or document-start prefix cannot stand in for EOF.
    for (selector, stage, head) in [
        (sel!(moveToBeginningOfDocument:), "cell_document_start", 0),
        (sel!(moveToEndOfDocument:), "cell_document_end", LARGE),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(selector)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let key = selection_pixels(rig, stage, MarkdownTextSelection::caret(head))?;
        if key.source != eof.source || key.request.input.generation != eof.request.input.generation
        {
            return Err("Cell端点往返没有沿原大草稿Source与内容版本".into());
        }
    }
    rig.remember_receipt()?;
    let original = rig.cell.as_ref().ok_or("原Cell owner已释放")?;
    unchanged(rig)?;
    let selected = observe(&rig.body)?;
    select(rig, 0..LARGE)?;
    frame::wait_complete_frame("cell_two_mebibyte_selected", &rig.body, selected)?;
    let output = copy(rig, true)?;
    println!(
        "native_body_cell stage=two_mebibyte actual_utf8_bytes={LARGE} input=NstextInputClient fixed_payload_bytes={SCRATCH} canonical_unchanged=true temporary_copy_reader_returned=true resources={:?} formal=false whole_product=false",
        original.resources.snapshot()
    );
    if cancel {
        // Actual cancellation ends only the original cell. The shared Rig verifies
        // final closure without requiring the user to quit an application.
        rig.view
            .command(sel!(cancelOperation:))
            .map_err(|cause| cause.to_string())?;
        if !original.resources.is_released() {
            return Err("原生Cell取消没有归还实际Scene/Producer/Font读取者和草稿".into());
        }
        unchanged(rig)?;
        return Ok(Some(output));
    }
    // Commit the intact large draft. Shrinking it first would not establish that
    // the actual large cell can be committed.
    rig.remember_receipt()?;
    let original = rig.cell.as_ref().ok_or("原Cell owner已释放")?;
    let before = original
        .canonical
        .state()
        .map_err(|cause| cause.to_string())?;
    let permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let outcome = rig
        .body
        .commit_cell_to_body(&original.canonical, &permit, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome.outcome else {
        return Err("两MiBCell没有沿原canonical Input交付真实提交".into());
    };
    commit
        .check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    verify_large_canonical(original, 1)?;
    if !original.resources.is_released() {
        return Err("两MiB提交后原草稿、读取者或准入候选未实际归还".into());
    }
    let undo_permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let undo = original
        .canonical
        .history(
            &undo_permit,
            NativeMarkdownPagedInputHistory::Undo,
            &|epoch| epoch == EPOCH,
        )
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(undo) = undo else {
        return Err("原canonical observed Undo没有真实提交".into());
    };
    undo.check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    {
        let doc = original.document.borrow();
        let mut restored = [0_u16; 64];
        let len = doc.utf16_len();
        if len != SOURCE.encode_utf16().count() {
            return Err("一次Undo没有恢复原有限canonical长度".into());
        } // Verify length before the fixed original-text buffer; never read out of bounds or allocate a long copy on failure.
        doc.copy_utf16_code_units(0, &mut restored[..len])
            .map_err(|cause| cause.to_string())?;
        if doc.generation() != 2
            || doc.byte_len() != SOURCE.len()
            || doc.selection() != before.selection
            || doc.can_undo()
            || !doc.can_redo()
            || restored[..len].iter().copied().ne(SOURCE.encode_utf16())
        {
            return Err("局部Cell历史被错误发布为多次canonical Undo或未还原原正文".into());
        }
    }
    let redo_permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let redo = original
        .canonical
        .history(
            &redo_permit,
            NativeMarkdownPagedInputHistory::Redo,
            &|epoch| epoch == EPOCH,
        )
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(redo) = redo else {
        return Err("原canonical observed Redo没有真实提交".into());
    };
    redo.check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    verify_large_canonical(original, 3)?;
    if !original.resources.is_released() {
        return Err("canonical Undo/Redo重新保活了已归还的Cell草稿或准入候选".into());
    }
    println!(
        "native_body_cell stage=two_mebibyte_committed actual_utf8_bytes={LARGE} canonical_bounded_exact=true canonical_single_undo_redo=true parent_doc_alive=true actual_cell_released=true resources={:?} formal=false whole_product=false",
        original.resources.snapshot(),
    );
    Ok(Some(output))
}

/// Run actual commit and cancel variants in sequence. Keep the independent copy
/// output through strict resource release, then verify it remains readable before clearing it.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    for cancel in [false, true] {
        let (retirement, result, output) = autoreleasepool(|_| -> Result<_, String> {
            let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
            file.write_all(SOURCE.as_bytes())
                .map_err(|cause| cause.to_string())?;
            let mut rig = Rig::attach_target(mtm, file, SOURCE.encode_utf16().count(), 4, true)?;
            let result = word::run(&mut rig).and_then(|()| exercise(&mut rig, cancel));
            let (result, output) = match result {
                Ok(output) => (Ok(()), output),
                Err(cause) => (Err(cause), None),
            };
            let retirement = release::close(rig)?;
            Ok((retirement, result, output))
        })?;
        retirement.verify()?;
        if let Some(output) = output {
            // Original document/cell/readers/Native graph have actually released;
            // the explicitly copied output must still be complete and readable.
            let retained = verify_output(&output.data, true);
            drop(output);
            retained?;
        }
        result?;
    }
    Ok(())
}
