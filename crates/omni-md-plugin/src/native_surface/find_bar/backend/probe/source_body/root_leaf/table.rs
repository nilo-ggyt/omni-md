//! Paint/search a fixed short table through the production Body/cell paging chain.
//! Independent single-cell primitives cannot substitute for actual TableRow integration.

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownPagedInputOutcome, paged_text_view::NativeMarkdownPagedHeadAffinity,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use std::io::Read;

#[path = "table/alignment.rs"]
mod alignment;
#[path = "table/insertion.rs"]
mod insertion;

#[path = "table/cell_find.rs"]
mod cell_find;
#[path = "table/cell_word.rs"]
mod cell_word;

const SOURCE: &str = concat!(
    "before-table\r\n\r\n",
    "| 名称 | 内容 |\r\n",
    "| --- | --- |\r\n",
    "| **BoldAlpha** *ItalicBeta* | [链接中文](#omni-table) `a\\|b` |\r\n",
    "| 行二中文 | wrap-start 中文自动换行 keeps bounded text in its cell 中文仍在同格 wrap-end |\r\n",
    "\r\nafter-table\r\n",
);

/// Fixed literals independently define canonical UTF-16 positions, never inferred
/// from the display projection under test.
fn canonical(label: &str) -> usize {
    SOURCE[..SOURCE.find(label).expect("固定表格夹具标签唯一存在")]
        .encode_utf16()
        .count()
}

/// Verify source/disk through fixed small chunks. Display projection, Find, and
/// pointer actions have no authority to change original Markdown or CRLF bytes.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("表格原Doc已提前释放")?;
    let document = document.borrow();
    if document.utf16_len() != SOURCE.encode_utf16().count() || document.generation() != 0 {
        return Err("表格只读操作改变了原Doc长度或generation".into());
    }
    let mut expected = SOURCE.encode_utf16();
    let mut offset = 0;
    let mut units = [0_u16; 128];
    while offset < document.utf16_len() {
        let used = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        for unit in &units[..used] {
            if expected.next() != Some(*unit) {
                return Err("表格原语法、转义管道或CRLF已被显示流程改写".into());
            }
        }
        offset += used;
    }
    drop(document);
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for expected in SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..expected.len()] != expected {
            return Err("表格原固定文件字节已被只读流程改写".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("表格原文件末尾出现额外内容".into());
    }
    Ok(())
}

/// Actual cell rectangles must map back to their independently known canonical
/// characters, never a pipe, hidden marker, or neighboring cell.
fn character(scene: &dyn NativeMarkdownPagedTextScene, offset: usize) -> Result<NSRect, String> {
    let first = scene
        .first_rect(NSRange::new(offset, 1))
        .map_err(|cause| format!("table canonical={offset}: {cause}"))?;
    if first.actual_range != NSRange::new(offset, 1)
        || first.rect.size.width <= 0.0
        || first.rect.size.height <= 1.0
    {
        return Err(format!(
            "表格原字符{offset}没有真实非空字符框或canonical映射错误：range={:?}, rect={:?}",
            first.actual_range, first.rect
        ));
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.1,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let hit = scene.hit(local).map_err(|cause| cause.to_string())?;
    if hit != offset {
        return Err(format!("表格实际点击落在{hit}，应为原字符{offset}"));
    }
    Ok(first.rect)
}

/// Check both columns, wrapping, and actual CPU ink in the current three-row
/// window, without manufacturing expected layout from candidate row heights.
fn geometry_and_paint(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let columns = [
        (
            character(&*scene, canonical("名称"))?,
            character(&*scene, canonical("内容"))?,
        ),
        (
            character(&*scene, canonical("BoldAlpha"))?,
            character(&*scene, canonical("链接中文"))?,
        ),
        (
            character(&*scene, canonical("行二中文"))?,
            character(&*scene, canonical("wrap-start"))?,
        ),
    ];
    let tolerance = 1.0 / 64.0;
    for (index, (left, right)) in columns.iter().enumerate() {
        if left.origin.x >= right.origin.x
            || (left.origin.x - columns[0].0.origin.x).abs() > tolerance
            || (right.origin.x - columns[0].1.origin.x).abs() > tolerance
            || (left.origin.y + left.size.height - right.origin.y - right.size.height).abs()
                > tolerance
        {
            return Err(format!(
                "真实表格第{index}行列框错位：left={left:?}, right={right:?}"
            ));
        }
        if index != 0 && left.origin.y + left.size.height >= columns[index - 1].0.origin.y {
            return Err(format!("真实表格第{index}行与前行重叠"));
        }
    }
    let wrapped = character(&*scene, canonical("wrap-end"))?;
    if wrapped.origin.y >= columns[2].1.origin.y {
        return Err("表格中英文长格没有在实际最终列宽下软换行".into());
    }
    character(&*scene, canonical("ItalicBeta"))?;
    character(&*scene, canonical("a\\|b"))?;
    character(&*scene, canonical("after-table"))?;
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // Compare the same actual Grid's synchronous frame with item-budgeted painting;
    // borders, code backgrounds, and selection must all match exactly in RGBA.
    selection::exact_scene_rgba(
        scene.clone(),
        &key,
        rig.panel.backingScaleFactor(),
        &current,
    )?;
    let canvas = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    let steps = selection::paint_scene(scene.clone(), &key, &canvas, &current)?;
    let mask = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    for (left, right) in columns {
        CGContext::fill_rect(Some(&mask.context), left);
        CGContext::fill_rect(Some(&mask.context), right);
    }
    CGContext::flush(Some(&canvas.context));
    CGContext::flush(Some(&mask.context));
    let ink = canvas
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(pixel, mask)| {
            **mask == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    if ink == 0
        || rig.view.cpu_paint_pending() != Some(false)
        || rig.view.cpu_paints().is_none_or(|paints| paints == 0)
    {
        return Err("真实格只有几何或背景，没有实际完整CPU字符帧".into());
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    println!(
        "native_body_table_row actual_grid_frame=true rows=3 columns=2 actual_soft_wrap=true canonical_rect_and_hit=true actual_text_ink={ink} bounded_paint_steps={steps} whole_product=false formal=false"
    );
    Ok(())
}

/// Original FindBar/Body/cell visibility proofs cover cross-row and following text.
/// Selection alone without an actual complete frame cannot satisfy this scenario.
fn find(
    rig: &mut Rig,
    query: &str,
    offset: usize,
    complete: bool,
    action: MarkdownFindAction,
    stage: &str,
) -> Result<(), String> {
    let old = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.bar.query.setStringValue(&NSString::from_str(query));
    let frame = observe(&rig.body)?;
    let pending = match rig.action(action).map_err(|cause| cause.to_string())? {
        NativeMarkdownFindExecution::Complete => None,
        NativeMarkdownFindExecution::Pending(request) => {
            rig.requests.push(request.clone());
            Some(request)
        }
    };
    if complete {
        frame::wait_complete_frame(stage, &rig.body, frame)?;
    } else {
        // Preserve the long table's actual unprepared tail; target-frame success
        // cannot depend on preparing the complete document.
        frame::wait_frame(stage, &rig.body, frame)?;
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let range = NSRange::new(offset, query.encode_utf16().count());
    if key.source != old.source || key.request.input.primary_range != range {
        return Err(format!(
            "表格Find丢失原Source或canonical范围：query={query}, actual={:?}, expected={range:?}",
            key.request.input.primary_range
        ));
    }
    if let Some(request) = pending
        && (request.pending()
            || !request.completed_visible()
            || request.cancelled()
            || request.take_failure().is_some()
            || request.range() != range)
    {
        return Err(format!("表格Find未交付原请求的真实可见证明：{query}"));
    }
    rig.view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    rig.remember_receipt()?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    selection::exact_scene_rgba(scene, &key, rig.panel.backingScaleFactor(), &current)?;
    Ok(())
}

/// Express wrap affinity through the sole actual Input's public selection intent,
/// then let the view notify the original Body for a complete frame. This tests
/// upstream/downstream behavior without private document mutation or moving the
/// 240-row fixture's start caret into a cell to evade hidden-pipe geometry.
fn table_caret_intent(
    rig: &mut Rig,
    position: usize,
    affinity: NativeMarkdownPagedHeadAffinity,
    stage: &str,
) -> Result<(), String> {
    let frame = observe(&rig.body)?;
    {
        let input = rig
            .body
            .probe_find_input_weak()
            .upgrade()
            .ok_or("表格原Input已释放")?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let geometry = rig.view.find_geometry_epoch().ok_or("表格原几何已撤销")?;
        let current = |epoch| {
            epoch == EPOCH
                && rig
                    .view
                    .find_input_is_current(&input, epoch, &before, Some(geometry))
                && native_markdown_controller_has_foreground_authority(&rig.controller)
        };
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let outcome = input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(position),
                Some(affinity.is_upstream()),
                None,
                &current,
            )
            .map_err(|cause| cause.to_string())?;
        let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
            return Err("表格caret意图没有发布原唯一选择收据".into());
        };
        if commit.state.generation != before.generation
            || commit.state.primary_range != NSRange::new(position, 0)
        {
            return Err("表格caret空间投影改变了原文或canonical请求位置".into());
        }
        rig.view
            .apply_find_commit(&input, &commit)
            .map_err(|cause| cause.to_string())?;
    } // Return permit/Input claims before actual frame waiting and strict close verification.
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    {
        // Explicit affinity belongs to the original selection intent. Same-source
        // style reflow may invalidate glyph coordinates, but cannot discard that
        // affinity. Read public Input facts rather than Scene caches so a failure
        // distinguishes lost selection affinity from later geometry publication.
        let input = rig
            .body
            .probe_find_input_weak()
            .upgrade()
            .ok_or("表格原Input已释放")?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let current = |epoch| {
            epoch == EPOCH
                && rig
                    .view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(&key))
        };
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let (actual, _) = input
            .navigation_selection_facts(&permit, &current)
            .map_err(|cause| cause.to_string())?;
        if actual != Some(affinity.is_upstream()) {
            return Err(format!(
                "{stage}: 本次显式caret亲和在真实帧发布时丢失：canonical={position}, actual_upstream={actual:?}, expected_upstream={}",
                affinity.is_upstream(),
            ));
        }
    } // Return Input/permit/key claims before resource observations, without changing text or reauthorizing selection.
    rig.remember_receipt()
}

/// Pipes/delimiters have no displayed text, but empty-range geometry must come
/// from actual first/last/neighboring-cell carets. Preserve original canonical
/// positions while comparing public geometry; a zero-size TableRow part supplies no rectangle.
fn syntax_carets(rig: &mut Rig) -> Result<(), String> {
    use NativeMarkdownPagedHeadAffinity::{Downstream, Upstream};
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let header_pipe = canonical("| 名称");
    let middle_pipe = canonical("| 内容");
    let delimiter = canonical("| ---");
    let row_end = canonical("| **BoldAlpha");
    // Independent literals specify syntax positions and neighboring-cell
    // boundaries; source CRLF spans do not become displayed characters.
    for (stage, position, affinity, expected) in [
        (
            "table_header_pipe_caret",
            header_pipe,
            Downstream,
            canonical("名称"),
        ),
        (
            "table_middle_pipe_upstream",
            middle_pipe,
            Upstream,
            canonical("名称") + 2,
        ),
        (
            "table_middle_pipe_downstream",
            middle_pipe,
            Downstream,
            canonical("内容"),
        ),
        (
            "table_delimiter_caret",
            delimiter,
            Downstream,
            canonical("内容") + 2,
        ),
        (
            "table_row_end_upstream",
            row_end,
            Upstream,
            canonical("内容") + 2,
        ),
        (
            "table_row_end_downstream",
            row_end,
            Downstream,
            canonical("BoldAlpha"),
        ),
    ] {
        table_caret_intent(rig, position, affinity, stage)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let projected = scene
            .first_rect(NSRange::new(position, 0))
            .map_err(|cause| cause.to_string())?;
        let actual = scene
            .first_rect(NSRange::new(expected, 0))
            .map_err(|cause| cause.to_string())?;
        if key.source != initial.source
            || key.request.input.primary_range != NSRange::new(position, 0)
            || projected.actual_range != NSRange::new(position, 0)
            || projected.rect != actual.rect
        {
            return Err(format!(
                "{stage}: 原语法caret没有保canonical位置或没有使用真实格boundary：canonical={position}, affinity={affinity:?}, expected_boundary={expected}, same_source={}, selection={:?}, projected_range={:?}, projected_rect={:?}, boundary_range={:?}, boundary_rect={:?}",
                key.source == initial.source,
                key.request.input.primary_range,
                projected.actual_range,
                projected.rect,
                actual.actual_range,
                actual.rect,
            ));
        }
        let current = || {
            rig.view
                .capture_ready_scene_key()
                .is_ok_and(|value| value.same(&key))
        };
        selection::exact_scene_rgba(
            scene.clone(),
            &key,
            rig.panel.backingScaleFactor(),
            &current,
        )?;
        // A nonempty pipe request still has no character. Caret projection cannot
        // expand into searchable/copyable neighboring text.
        if !matches!(
            scene.first_rect(NSRange::new(position, 1)),
            Err(ViewError::Pending)
        ) {
            return Err("隐藏管道被错误当作邻格的可见字符".into());
        }
    }
    table_caret_intent(rig, 0, Downstream, "table_syntax_caret_restore")?;
    unchanged(rig)
}

/// Activate only on actual Command mouse-up, retaining typed rejection for the
/// fixed missing fragment. This scenario opens no browser.
fn link(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    rig.panel.makeFirstResponder(Some(&**rig.view));
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let window_point = links::point(rig, canonical("链接中文"))?;
    let local = rig.view.convertPoint_fromView(window_point, None);
    if scene
        .link_hit(local)
        .map_err(|cause| cause.to_string())?
        .is_none()
        || scene
            .link_target(local)
            .map_err(|cause| cause.to_string())?
            .as_deref()
            != Some("#omni-table")
    {
        return Err("真实格链接没有命中可见标签或丢失原Parser目标".into());
    }
    drop(scene);
    links::command_mouse(rig, window_point, NSEventType::LeftMouseDown)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != key.request.input {
        return Err("表格链接mouseDown提前改写了选择或吞掉原拖动入口".into());
    }
    links::command_mouse(rig, window_point, NSEventType::LeftMouseUp)?;
    if !matches!(links::settle(rig)?, Some(ViewError::LinkAnchorNotFound)) {
        return Err("表格链接真实mouseUp没有明确拒绝不存在的标题目标".into());
    }
    unchanged(rig)
}

/// Failure also completes original strict Native/font/Source/pixel cleanup; a
/// later case's closure cannot clean up this scenario's leaked graph.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            SOURCE.encode_utf16().count(),
            SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| {
            unchanged(&rig)?;
            geometry_and_paint(&rig)?;
            syntax_carets(&mut rig)?;
            link(&mut rig)?;
            for query in ["链接中文", "行二中文", "after-table", "BoldAlpha"] {
                find(
                    &mut rig,
                    query,
                    canonical(query),
                    true,
                    MarkdownFindAction::Next,
                    "table_row_find",
                )?;
                unchanged(&rig)?;
            }
            rig.remember_receipt()
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    if result.is_ok() {
        println!(
            "native_body_table_row source_and_disk_unchanged=true actual_find_and_link=true strict_original_release=true offscreen_row_restore_tested=false cross_cell_edit_tested=false physical_ime_tested=false whole_product=false formal=false"
        );
    }
    // Each following scenario owns its document and strict close boundary, sharing
    // no failed state. Retain every actual failure while checking the other user
    // actions, so cross-cell defects cannot conceal alignment, insertion, long-
    // table, or horizontal-scrolling failures.
    let mut failures = Vec::new();
    for (name, result) in [
        ("row", result),
        ("active", active_table_ui(mtm)),
        ("word", cell_word::run(mtm)),
        ("alignment", alignment::run(mtm)),
        ("insertion", insertion::run(mtm)),
        ("long", long_table(mtm)),
        ("wide", wide_table(mtm)),
    ] {
        if let Err(cause) = result {
            eprintln!("native_root_table_case role={name} outcome=failed reason={cause}");
            failures.push(format!("{name}: {cause}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

const LONG_HEADER: &str = "| 普通 | 粗体 | 斜体 | 行内代码 |\r\n| --- | --- | --- | --- |\r\n";
const LONG_ROWS: usize = 240;
const LONG_TAIL: &str = "\r\n表后尾部保持按视口读取\r\n";
const LONG_TAIL_REPEATS: usize = 128;

/// Keep independent input positions for the first/last rows only. Four source
/// endpoints come from generated literals, not the parser/display mapping under test.
struct LongRowTarget {
    query: String,
    canonical: [usize; 4],
}

/// Construct one fixed short row at a time. Generation and chunked verification
/// never aggregate a complete table or document string.
fn long_row(row: usize) -> String {
    format!(
        "| 中文普通{row:04} | **中文粗体{row:04}** | *中文斜体{row:04}* | `中文代码{row:04}` |\r\n"
    )
}

/// Independently derive first/last targets from the fixed row template's UTF-16
/// prefix. Product content still comes only from mmap and its original structural ticket.
fn long_target(row: usize, offset: usize) -> LongRowTarget {
    let text = long_row(row);
    let labels = [
        format!("中文普通{row:04}"),
        format!("中文粗体{row:04}"),
        format!("中文斜体{row:04}"),
        format!("中文代码{row:04}"),
    ];
    let canonical = labels.each_ref().map(|label| {
        let byte = text.find(label.as_str()).expect("固定输入每列标签唯一存在");
        offset + text[..byte].encode_utf16().count()
    });
    LongRowTarget {
        query: labels[0].clone(),
        canonical,
    }
}

/// Verify actual document/disk contents with one row template and 128-unit stack
/// chunks, without writing to the original fixture's saved location.
fn long_unchanged(rig: &Rig) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("长表原Doc已提前释放")?;
    let document = owner.borrow();
    if document.generation() != 0 {
        return Err("长表只读交互改变了原文稿代次".into());
    }
    let mut disk = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut offset = 0;
    let mut units = [0_u16; 128];
    let mut bytes = [0_u8; 128];
    let pieces = std::iter::once(LONG_HEADER.to_owned())
        .chain((1..=LONG_ROWS).map(long_row))
        .chain((0..LONG_TAIL_REPEATS).map(|_| LONG_TAIL.to_owned()));
    for piece in pieces {
        let mut expected = piece.encode_utf16();
        loop {
            // Compare even one row in fixed stack chunks, without treating
            // multibyte Chinese bytes as UTF-16 offsets.
            let mut used = 0;
            for _ in 0..units.len() {
                let Some(unit) = expected.next() else {
                    break;
                };
                units[used] = unit;
                used += 1;
            }
            if used == 0 {
                break;
            }
            let mut actual = [0_u16; 128];
            document
                .copy_utf16_code_units(offset, &mut actual[..used])
                .map_err(|cause| cause.to_string())?;
            if actual[..used] != units[..used] {
                return Err(format!("长表原 canonical 在{offset}被显示或Find改写"));
            }
            offset += used;
        }
        for expected in piece.as_bytes().chunks(bytes.len()) {
            disk.read_exact(&mut bytes[..expected.len()])
                .map_err(|cause| cause.to_string())?;
            if &bytes[..expected.len()] != expected {
                return Err("长表原磁盘 Markdown、样式标记或 CRLF 被只读流程改写".into());
            }
        }
    }
    if offset != document.utf16_len()
        || disk
            .read(&mut bytes[..1])
            .map_err(|cause| cause.to_string())?
            != 0
    {
        return Err("长表原文稿或磁盘出现遗漏/额外尾部".into());
    }
    Ok(())
}

/// Every column requires actual geometry/hits, bounded paint, and complete CPU
/// pixels; a last-row selection receipt alone is insufficient.
fn long_frame(rig: &Rig, target: &LongRowTarget) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let mut rectangles = [NSRect::new(NSPoint::ZERO, NSSize::ZERO); 4];
    for (column, offset) in target.canonical.iter().enumerate() {
        rectangles[column] = character(&*scene, *offset)?;
        if column > 0 && rectangles[column].origin.x <= rectangles[column - 1].origin.x {
            return Err("长表四列真实字符框没有保持原列顺序".into());
        }
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // Synchronous and item-budgeted frames still match exactly; long tables use
    // no separate painting entry that bypasses the production protocol.
    selection::exact_scene_rgba(
        scene.clone(),
        &key,
        rig.panel.backingScaleFactor(),
        &current,
    )?;
    let canvas = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    selection::paint_scene(scene.clone(), &key, &canvas, &current)?;
    CGContext::flush(Some(&canvas.context));
    for (column, rectangle) in rectangles.iter().enumerate() {
        // Independent per-column masks each need actual glyph ink. Correct other
        // columns cannot conceal blank bold, italic, or inline-code cells.
        let mask = selection::Canvas::scaled(key.request.viewport, 1.0)?;
        CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
        CGContext::fill_rect(Some(&mask.context), *rectangle);
        CGContext::flush(Some(&mask.context));
        let ink = canvas
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(mask.pixels.as_chunks::<4>().0)
            .any(|(pixel, mask)| {
                *mask == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
            });
        if !ink {
            return Err(format!("长表第{column}列没有实际字符墨迹"));
        }
    }
    if rig.view.cpu_paint_pending() != Some(false)
        || rig.view.cpu_paints().is_none_or(|paints| paints == 0)
    {
        return Err("长表目标没有真实完成的 CPU 文字帧".into());
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    Ok(())
}

/// The 240-row fixture starts at pipe zero; its first frame must preserve that
/// selection and project geometry through the actual first-cell caret. A nonempty
/// pipe still has no glyph; assigning artificial font size to TableRow structure cannot pass.
fn long_initial_caret(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.primary_range != NSRange::new(0, 0) {
        return Err("240行首帧把原文首光标挪进格以绕过隐藏管道".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let original = scene
        .first_rect(NSRange::new(0, 0))
        .map_err(|cause| cause.to_string())?;
    let first_cell = scene
        .first_rect(NSRange::new(2, 0))
        .map_err(|cause| cause.to_string())?;
    if original.actual_range != NSRange::new(0, 0)
        || first_cell.actual_range != NSRange::new(2, 0)
        || original.rect != first_cell.rect
        || !matches!(
            scene.first_rect(NSRange::new(0, 1)),
            Err(ViewError::Pending)
        )
    {
        return Err("240行文首语法没有按真实首格边界保留原canonical语义".into());
    }
    Ok(())
}

/// Actual Find crosses 240 rows to the last row and returns to the first. Keep the
/// original three-second frame gate and close this actual graph even after failure.
fn long_table(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(LONG_HEADER.as_bytes())
            .map_err(|cause| cause.to_string())?;
        let mut utf16 = LONG_HEADER.encode_utf16().count();
        let first = long_target(1, utf16);
        let mut last = None;
        for row in 1..=LONG_ROWS {
            if row == LONG_ROWS {
                last = Some(long_target(row, utf16));
            }
            let text = long_row(row);
            file.write_all(text.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += text.encode_utf16().count();
        }
        // A real unread tail prevents whole-document completion from standing in
        // for last-row Find's viewport readiness.
        for _ in 0..LONG_TAIL_REPEATS {
            file.write_all(LONG_TAIL.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += LONG_TAIL.encode_utf16().count();
        }
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            utf16,
            2 + LONG_ROWS + LONG_TAIL_REPEATS * 2 + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let last = last.expect("固定240行包含真实末行");
        let result = (|| {
            long_unchanged(&rig)?;
            long_initial_caret(&rig)?;
            long_frame(&rig, &first)?;
            for (stage, target) in [
                ("table_240_last_find", &last),
                ("table_240_first_restore", &first),
            ] {
                find(
                    &mut rig,
                    &target.query,
                    target.canonical[0],
                    false,
                    MarkdownFindAction::Next,
                    stage,
                )?;
                long_frame(&rig, target)?;
                long_unchanged(&rig)?;
            }
            rig.remember_receipt()
        })();
        // Original release checks each actual font payload/mmap/FD/worker;
        // resource reuse grants no relaxation of release assertions.
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    result?;
    println!(
        "native_body_table_240 rows=240 columns=4 cjk_plain_strong_emphasis_code=true actual_canonical_rect_hit=true actual_cpu_frame=true actual_last_find_first_restore=true source_and_disk_unchanged=true strict_original_font_release=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const WIDE_COLUMNS: usize = 20;
const WIDE_ROWS: usize = 3;
const WIDE_LEAD: &str = "宽表前的独立正文保持原布局\r\n\r\n";
const WIDE_LEAD_REPEATS: usize = 32;
const WIDE_LEFT: &str = "左端首列目标";
const WIDE_RIGHT: &str = "右端末列目标";
const WIDE_TAIL: &str = "宽表后仍未显示的固定尾段\r\n\r\n";
const WIDE_TAIL_REPEATS: usize = (1024 * 1024) / WIDE_TAIL.len();

/// Vary the fixed 20-column header lengths so actual natural widths must come from
/// production layout rather than an equal-width assumption shared by both sides.
/// Keep one bounded input row, without reading product widths or building full table/tail text.
fn wide_header() -> String {
    let mut header = String::from("|");
    // Only the fixture recipe varies label lengths; actual fonts/table layout own
    // resulting widths. Test/product cannot both obtain matching wrong endpoints
    // from a shared uniform-column assumption.
    for column in 1..=WIDE_COLUMNS {
        let varying = "宽".repeat(column % 4 + 1);
        header.push_str(&format!(" 表头{column:02}{varying}中文 |"));
    }
    header.push_str("\r\n|");
    for _ in 0..WIDE_COLUMNS {
        header.push_str(" --- |"); // Each actual column has its own GFM delimiter cell, independent of visible-column counts.
    }
    header.push_str("\r\n");
    header
}

/// Independent first-row text targets occupy actual columns one/twenty. Other
/// cells also contain real original Markdown, never empty placeholders.
fn wide_row(row: usize) -> String {
    let mut text = String::from("|");
    for column in 1..=WIDE_COLUMNS {
        text.push(' ');
        if row == 1 && column == 1 {
            text.push_str(WIDE_LEFT);
        } else if row == 1 && column == WIDE_COLUMNS {
            text.push_str(WIDE_RIGHT);
        } else {
            text.push_str(&format!("真实第{row:02}行第{column:02}列"));
        }
        text.push_str(" |");
    }
    text.push_str("\r\n");
    text
}

/// Verify canonical/disk contents in bounded chunks after complete-frame acceptance.
/// Integrity checks may stream the tail, while production target preparation must
/// not prepare it all; independent generation rules and actual progress prove these separately.
fn wide_unchanged(rig: &Rig) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("宽表原Doc已提前释放")?;
    if document.borrow().generation() != 0 {
        return Err("宽表只读Find改变了原文稿代次".into());
    }
    let mut disk = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    // Reconstruct source order from the independent input recipe, using at most
    // one row and fixed stack chunks rather than deserializing expected text from
    // display indexes. Verify the tail too, exposing unexpected persistence/truncation
    // that comparing only the two target labels would miss.
    let pieces = (0..WIDE_LEAD_REPEATS)
        .map(|_| WIDE_LEAD.to_owned())
        .chain(std::iter::once(wide_header()))
        .chain((1..=WIDE_ROWS).map(wide_row))
        .chain((0..WIDE_TAIL_REPEATS).map(|_| WIDE_TAIL.to_owned()));
    let mut utf16 = 0;
    let mut byte_length = 0;
    let mut units = [0_u16; 128];
    let mut actual = [0_u16; 128];
    let mut bytes = [0_u8; 128];
    for piece in pieces {
        let mut expected = piece.encode_utf16();
        loop {
            let mut used = 0;
            for unit in &mut units {
                let Some(value) = expected.next() else { break };
                *unit = value;
                used += 1;
            }
            if used == 0 {
                break;
            }
            // Borrow this same canonical source for one stack chunk only. Disk
            // I/O and later Native operations hold no live RefCell lease.
            document
                .borrow()
                .copy_utf16_code_units(utf16, &mut actual[..used])
                .map_err(|cause| cause.to_string())?;
            if actual[..used] != units[..used] {
                return Err(format!("宽表canonical在{utf16}被只读流程改写"));
            }
            utf16 += used;
        }
        for expected in piece.as_bytes().chunks(bytes.len()) {
            disk.read_exact(&mut bytes[..expected.len()])
                .map_err(|cause| cause.to_string())?;
            if &bytes[..expected.len()] != expected {
                return Err("宽表原磁盘的列、文字或CRLF发生变化".into());
            }
            byte_length += expected.len();
        }
    }
    if document.borrow().utf16_len() != utf16
        || document.borrow().byte_len() != byte_length
        || disk
            .read(&mut bytes[..1])
            .map_err(|cause| cause.to_string())?
            != 0
    {
        return Err("宽表原文稿或磁盘出现遗漏/额外尾部".into());
    }
    Ok(())
}

/// Find already compared same-ticket synchronous/stepped RGBA. Now inspect the
/// actually published target ink plus canonical geometry/hits. Return the actual
/// rectangle for clip-coordinate comparison, without private Grid fields or estimated widths.
fn wide_frame(rig: &Rig, offset: usize) -> Result<NSRect, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let rectangle = character(&*scene, offset)?;
    let current = || {
        rig.view.cpu_paint_pending() == Some(false)
            && rig
                .view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
    };
    // Find already verified the same frame through its actual three-second event
    // and independent RGBA comparison. Inspect the mounted CPU image here; both
    // canvases cover only the current viewport, never an offscreen image of all 20 columns.
    let scale = rig.panel.backingScaleFactor();
    let canvas = selection::Canvas::scaled(key.request.viewport, scale)?;
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("宽表正文没有真实backing")?;
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    // SAFETY: The original view/main-thread backing remain alive on this stack;
    // inspect actual mounted children without constructing substitute layers.
    let children = unsafe { backing.sublayers() }.ok_or("宽表CPU帧没有实际挂载点")?;
    if children.len() != 1 {
        return Err("宽表有限正文没有保持唯一实际CPU挂载点".into());
    }
    let mount = children.objectAtIndex(0);
    if mount.frame() != bounds || mount.bounds() != NSRect::new(NSPoint::ZERO, bounds.size) {
        return Err("宽表CPU挂载几何偏离当前有限正文".into());
    }
    // Mount coordinates begin at zero. Restore current view bounds before mapping
    // the same scale, without rendering full document width into memory.
    CGContext::translate_ctm(Some(&canvas.context), bounds.origin.x, bounds.origin.y);
    mount.renderInContext(&canvas.context);
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&mask.context), rectangle);
    CGContext::flush(Some(&canvas.context));
    CGContext::flush(Some(&mask.context));
    // Mask the actual first-character rectangle only; white background, selection,
    // or another column cannot supply its black ink. Explicit RGBA8 bytes inspect
    // actual published pixels rather than mere font/layer existence.
    let ink = canvas
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(pixel, marker)| {
            **marker == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    if ink == 0 || !current() {
        return Err("宽表目标只有选择/几何，当前已发布CPU图没有同一字符墨迹".into());
    }
    // A complete frame certifies this viewport only. The original composer must
    // remain incomplete, with layout/semantic reads below full-source coverage.
    // The approximately 1MiB unprepared tail is real source, not artificial height
    // or fake rows; later scrolling still obtains actual data through mmap/indexes.
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.complete
        || progress.frontier_source_bytes >= progress.source_total_bytes
        || progress.semantic_work.source_bytes >= progress.source_total_bytes
    {
        return Err("宽表显示目标以准备全部尾部为代价，或原实际读取已遍历整篇".into());
    }
    println!(
        "native_body_table_wide target={offset} actual_canonical_rect_hit=true actual_published_cpu_ink={ink} exact_sync_async_rgba=true actual_prefix_only=true frontier_bytes={} semantic_source_bytes={} source_total_bytes={} formal=false whole_product=false",
        progress.frontier_source_bytes,
        progress.semantic_work.source_bytes,
        progress.source_total_bytes,
    );
    Ok(rectangle)
}

/// Begin in ordinary text, then use actual Find to reach rightmost/first columns.
/// The original scroll's x/flag must truly change. Return temporary Native claims
/// before strict release; a flag observation cannot retain the complete scroll graph.
fn wide_table(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        let mut utf16 = 0;
        // Fixed preceding text starts outside the table, allowing observation of
        // this same scroll before its horizontal range changes. Do not manufacture
        // that premise through scroll flags, clip origins, or Body-height setters.
        for _ in 0..WIDE_LEAD_REPEATS {
            file.write_all(WIDE_LEAD.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += WIDE_LEAD.encode_utf16().count();
        }
        let header = wide_header();
        file.write_all(header.as_bytes())
            .map_err(|cause| cause.to_string())?;
        utf16 += header.encode_utf16().count();
        let first = wide_row(1);
        // Find/input use canonical UTF-16 units, not Chinese UTF-8 byte lengths.
        // Derive both positions from this fixed 20-short-cell row only, independent
        // of parsing, display projection, actual column geometry, or the later long tail.
        let left = utf16
            + first[..first.find(WIDE_LEFT).expect("首列固定唯一目标")]
                .encode_utf16()
                .count();
        let right = utf16
            + first[..first.find(WIDE_RIGHT).expect("末列固定唯一目标")]
                .encode_utf16()
                .count();
        file.write_all(first.as_bytes())
            .map_err(|cause| cause.to_string())?;
        utf16 += first.encode_utf16().count();
        for row in 2..=WIDE_ROWS {
            let text = wide_row(row);
            file.write_all(text.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += text.encode_utf16().count();
        }
        // Stream the approximately 1MiB tail in fixed fragments. Preparing these
        // table targets requires no layout of that tail.
        for _ in 0..WIDE_TAIL_REPEATS {
            file.write_all(WIDE_TAIL.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += WIDE_TAIL.encode_utf16().count();
        }
        file.flush().map_err(|cause| cause.to_string())?;
        drop((header, first)); // Even header/first-row generation fragments do not survive across the actual Rig lifetime.
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            utf16,
            WIDE_LEAD_REPEATS * 2 + 2 + WIDE_ROWS + WIDE_TAIL_REPEATS * 2 + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let original_horizontal = rig.scroll.hasHorizontalScroller();
        let result = (|| {
            // Initial visible ordinary text needs no horizontal range. Record
            // the actual public flag before table entry for close-time restoration checks.
            if original_horizontal {
                return Err("宽表前的原正文已经出现无内容需要的横向滚动条".into());
            }
            let initial = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let initial_clip_x = rig.scroll.contentView().bounds().origin.x;
            wide_unchanged(&rig)?;
            find(
                &mut rig,
                WIDE_RIGHT,
                right,
                false,
                MarkdownFindAction::Next,
                "table_wide_right_find",
            )?;
            let right_rectangle = wide_frame(&rig, right)?;
            let right_key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let right_clip_x = rig.scroll.contentView().bounds().origin.x;
            if !rig.scroll.hasHorizontalScroller()
                || right_clip_x <= initial_clip_x
                || right_key.request.virtual_viewport.document_origin.x
                    <= initial.request.virtual_viewport.document_origin.x
            {
                return Err("宽表最右列Find未通过原NSScrollView真实横向滚动交付".into());
            }
            // Compare actual document origin plus local character position, never
            // a presumed width for column twenty. The clip must actually move;
            // changed Scene coordinates/selection alone cannot satisfy this round trip.
            let right_document_x =
                right_key.request.virtual_viewport.document_origin.x + right_rectangle.origin.x;
            wide_unchanged(&rig)?;
            // Actual Previous returns to the first column. No manual clip or
            // selection setter fabricates the return trip.
            find(
                &mut rig,
                WIDE_LEFT,
                left,
                false,
                MarkdownFindAction::Previous,
                "table_wide_left_return",
            )?;
            let left_rectangle = wide_frame(&rig, left)?;
            let left_key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let left_clip_x = rig.scroll.contentView().bounds().origin.x;
            let left_document_x =
                left_key.request.virtual_viewport.document_origin.x + left_rectangle.origin.x;
            if !rig.scroll.hasHorizontalScroller()
                || left_clip_x >= right_clip_x
                || left_key.request.virtual_viewport.document_origin.x
                    >= right_key.request.virtual_viewport.document_origin.x
                || right_document_x - left_document_x
                    <= left_key.request.virtual_viewport.layout_width
            {
                return Err("宽表首列返回没有真实反向滚动，或固定20列未形成超视口真实宽度".into());
            }
            wide_unchanged(&rig)?;
            rig.remember_receipt()?;
            println!(
                "native_body_table_wide columns=20 rows=3 actual_right_find_left_previous=true initial_clip_x={initial_clip_x} right_clip_x={right_clip_x} returned_clip_x={left_clip_x} actual_horizontal_scroller=true source_and_disk_unchanged=true original_three_second_frame_gate=true formal=false whole_product=false"
            );
            Ok(())
        })();
        // Retain this short claim only to inspect the same scroll's post-close
        // public flag. Drop it before pool exit; Retirement still checks actual
        // Native weak identities, Source/mmap/FD/worker/font/pixel release without exemptions.
        let scroll = Retained::clone(&rig.scroll);
        let retirement = release::close(rig)?;
        // Product viewport cleanup must already restore this same scroll's public
        // flag. The test neither sets its original value itself nor relies on
        // later window destruction to conceal a missing restoration.
        let restored = if scroll.hasHorizontalScroller() == original_horizontal {
            Ok(())
        } else {
            Err("宽表关闭没有归还原scroll的horizontal flag".to_owned())
        };
        drop(scroll);
        // Close after success or failure, preserving the primary interaction
        // failure; original verify separately reports actual cleanup errors.
        Ok((retirement, result.and(restored)))
    })?;
    retirement.verify()?;
    result?;
    println!(
        "native_body_table_wide horizontal_flag_restored=true strict_original_source_fd_worker_font_native_release=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const UI_SOURCE: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| seed | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_INSERTED: &str = "中文|a\r\nb";
const UI_FAST_COMMITTED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| sEed | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_COMMITTED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| 中文\\|a b | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_APPENDED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| 中文\\|a b | other |\r\n",
    "| | |\r\n",
    "\r\nafter-ui\r\n",
);

/// This short fixture covers actual cell UI transactions. Fixed independent
/// expectations include CRLF, escaped pipes, and added rows, never a long-document
/// oracle. No save occurred; verification has no authority to write or repair text.
fn ui_document(rig: &Rig, expected: &str) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("格UI原Doc已释放")?;
    let document = owner.borrow();
    if document.utf16_len() != expected.encode_utf16().count() {
        return Err(format!(
            "格UI canonical长度不符：actual={}, expected={}",
            document.utf16_len(),
            expected.encode_utf16().count()
        ));
    }
    let mut utf16 = expected.encode_utf16();
    let mut offset = 0;
    let mut units = [0_u16; 128];
    while offset < document.utf16_len() {
        let count = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..count])
            .map_err(|cause| cause.to_string())?;
        for unit in &units[..count] {
            if utf16.next() != Some(*unit) {
                return Err("格UI事务改变了独立期望之外的正文或CRLF".into());
            }
        }
        offset += count;
    }
    drop(document);
    drop(owner);
    // Open the actual file independently of production mmap and read fixed chunks;
    // a complete memory copy cannot conceal unintended persistence.
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for original in UI_SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..original.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..original.len()] != original {
            return Err("格UI局部事务提前改写了原磁盘文件".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("格UI原磁盘尾部出现新增字节".into());
    }
    Ok(())
}

/// The actual system firstResponder identifies the child, and its published CPU
/// key proves complete pixels. active_cell_body is also the real menu consumer;
/// do not manufacture a scene from private slots or register another paint observer
/// over the Body's actual activation work.
fn ui_active(
    rig: &mut Rig,
    previous: Option<&Rc<NativeMarkdownPagedBody>>,
    started: Instant,
    stage: &str,
) -> Result<Rc<NativeMarkdownPagedBody>, String> {
    loop {
        if let Some(cause) = rig.body.take_failure() {
            return Err(format!("{stage}: 父Body失败：{cause}"));
        }
        if let Some(cause) = rig.view.take_failure()
            && !matches!(cause, ViewError::Pending)
        {
            return Err(format!("{stage}: 父View失败：{cause}"));
        }
        if let Some(body) = rig.body.active_cell_body() {
            if let Some(cause) = body.take_failure() {
                return Err(format!("{stage}: 子Body失败：{cause}"));
            }
            let view = body.view().map_err(|cause| cause.to_string())?;
            if let Some(cause) = view.take_failure()
                && !matches!(cause, ViewError::Pending)
            {
                return Err(format!("{stage}: 子View失败：{cause}"));
            }
            let focused = rig.panel.firstResponder().is_some_and(|responder| {
                let actual: &objc2::runtime::AnyObject = responder.as_ref();
                actual
                    .downcast_ref::<NativeMarkdownPagedTextView>()
                    .is_some_and(|actual| std::ptr::eq(actual, &**view))
            });
            let changed = previous.is_none_or(|previous| !Rc::ptr_eq(previous, &body));
            if focused
                && changed
                && !body.is_preparing()
                && let Ok(key) = view.capture_ready_scene_key()
                && let Some((published, paints)) = view.probe_cpu_published_frame()
                && published.same(&key)
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
            {
                if key.request.target.is_body(&rig.document)
                    || view
                        .cell_history_facts()
                        .map_err(|cause| cause.to_string())?
                        .is_none()
                {
                    return Err(format!("{stage}: 系统焦点没有进入真实Cell输入域"));
                }
                // Add actual child Native roles to the original Rig ledger.
                // Retired cells have no production cache; retain only weak
                // observations. End temporary upgrades here so local strong claims
                // cannot conceal later cell-switch or close-time retention defects.
                for (role, weak) in body.native_weaks().map_err(|cause| cause.to_string())? {
                    if let Some(object) = weak.load() {
                        rig.ledger.include(role, &object)?;
                    }
                }
                if let Some(scroll) = view.enclosingScrollView() {
                    rig.ledger.include("table-ui.child-scroll", &scroll)?;
                }
                rig.receipts.push(
                    body.resource_receipts()
                        .map_err(|cause| cause.to_string())?,
                );
                println!(
                    "native_body_table_cell_ui stage={stage} actual_focused_cell=true actual_cpu_paints={paints} actual_elapsed_ms={:.3} deadline_seconds=3 whole_product=false formal=false",
                    started.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(body);
            }
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or_else(|| format!("{stage}: 三秒内没有当前系统焦点中的真实Cell完整帧"))?;
        // SAFETY: Service the original main run loop only for this action's
        // remaining deadline; actual layout/paint events permit rechecking public results.
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// Read the actual NSTextInputClient's short draft using independently fixed
/// expected ranges, without private Input state or a second test-captured Source.
fn ui_local(view: &NativeMarkdownPagedTextView, expected: &str) -> Result<(), String> {
    let range = NSRange::new(0, expected.encode_utf16().count());
    let mut actual = NSRange::new(objc2_foundation::NSNotFound as usize, 0);
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: The actual main-thread client and writable output parameter live
    // through this call; the independent short fixture supplies a bounded range.
    let value =
        unsafe { client.attributedSubstringForProposedRange_actualRange(range, &mut actual) }
            .ok_or("真实Cell client没有交付当前有限草稿")?;
    if actual != range
        || value.string().to_string() != expected
        || view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .utf16_len
            != range.length
    {
        return Err(format!(
            "真实Cell client内容/长度不符：actual_range={actual:?}, expected_range={range:?}"
        ));
    }
    Ok(())
}

/// Enter a displayed cell through actual mouse events, then edit through the same
/// system-focused NSTextInputClient. This component Rig proves no workspace or whole-product acceptance.
fn ui_flow(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("格UI合成窗口没有取得真实keyWindow".into());
    }
    let seed = UI_SOURCE[..UI_SOURCE.find("seed").expect("固定格UI seed存在")]
        .encode_utf16()
        .count();
    let point = links::point(rig, seed)?;
    let started = Instant::now();
    links::mouse(rig, point, NSEventType::LeftMouseDown)?;
    links::mouse(rig, point, NSEventType::LeftMouseUp)?; // Actual up before Ready still ends down and cannot revive dragging.
    let first = ui_active(rig, None, started, "table_ui_mouse_cell")?;
    let view = first.view().map_err(|cause| cause.to_string())?;
    let before = view.input_state().map_err(|cause| cause.to_string())?;
    ui_local(&view, "seed")?;
    ui_document(rig, UI_SOURCE)?;
    let original_generation = rig
        .document
        .upgrade()
        .ok_or("格UI原Doc已释放")?
        .borrow()
        .generation();
    let frame = observe(&first)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
    // SAFETY: Use the actual Native input protocol with fixed new text. This
    // direct protocol call is no physical Chinese IME candidate-window acceptance.
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(UI_INSERTED), NSRange::new(0, 4));
    }
    frame::wait_complete_frame("table_ui_native_edit", &first, frame)?;
    ui_local(&view, UI_INSERTED)?;
    ui_document(rig, UI_SOURCE)?;
    for (selector, expected, stage) in [
        (sel!(undo:), "seed", "table_ui_local_undo"),
        (sel!(redo:), UI_INSERTED, "table_ui_local_redo"),
    ] {
        // Standard commands affect the current cell's local history only; after
        // actual complete frames, canonical generation/text and disk remain unchanged.
        let frame = observe(&first)?;
        view.command(selector).map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &first, frame)?;
        ui_local(&view, expected)?;
        ui_document(rig, UI_SOURCE)?;
        if rig
            .document
            .upgrade()
            .ok_or("格UI原Doc已释放")?
            .borrow()
            .generation()
            != original_generation
        {
            return Err("格UI局部历史误提交了父文档".into());
        }
        if selector == sel!(undo:)
            && view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .selection
                != before.selection
        {
            return Err("格UI局部Undo没有恢复实际点按前的有向选择".into());
        }
    }
    rig.receipts.push(
        first
            .resource_receipts()
            .map_err(|cause| cause.to_string())?,
    );
    let started = Instant::now();
    view.command(sel!(insertTab:))
        .map_err(|cause| cause.to_string())?;
    let second = ui_active(rig, Some(&first), started, "table_ui_tab_next_cell")?;
    let second_view = second.view().map_err(|cause| cause.to_string())?;
    if second_view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .primary_range
        != NSRange::new(0, 5)
    {
        return Err("真实Tab没有进入下一格并全选原other".into());
    }
    ui_local(&second_view, "other")?;
    ui_document(rig, UI_COMMITTED)?;
    // Tab from the actual final row/cell appends once only after Core proves End;
    // the viewport's last visible row cannot stand in for the table endpoint.
    let started = Instant::now();
    second_view
        .command(sel!(insertTab:))
        .map_err(|cause| cause.to_string())?;
    let empty = ui_active(rig, Some(&second), started, "table_ui_terminal_append")?;
    let empty_view = empty.view().map_err(|cause| cause.to_string())?;
    if empty_view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .utf16_len
        != 0
    {
        return Err("末格Tab没有进入真实新增行首个空格".into());
    }
    ui_document(rig, UI_APPENDED)?;
    // The new empty cell is unchanged; Escape cancels it only. Actual document
    // Undo then removes the appended row once and restores the first cell once.
    // Keep standard selector routes instead of direct document writes or private
    // active-slot clearing that would artificially assist resource verification.
    empty_view
        .command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    let started = Instant::now();
    while rig.body.active_cell_body().is_some() {
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("取消空格超过三秒")?;
        // SAFETY: The original main thread services only this cancellation's
        // remaining deadline, without another thread, sleep, or substituted focus.
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
    for (expected, stage) in [
        (UI_COMMITTED, "table_ui_append_one_undo"),
        (UI_SOURCE, "table_ui_cell_one_undo"),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        ui_document(rig, expected)?;
        rig.remember_receipt()?;
    }
    // Return temporary strong claims first. Original strict close still verifies
    // retired child Native/FD/mmap/worker/font/pixel resources.
    drop((empty_view, empty, second_view, second, view, first));
    Ok(())
}

/// Deliver actual events to the system-focused child. Cocoa retains the original
/// receiver for drag/up after crossing outside it; do not redirect through parent
/// mouse entries or transfer selection/gesture ownership through test-only methods.
fn ui_child_mouse(
    rig: &Rig,
    view: &NativeMarkdownPagedTextView,
    point: NSPoint,
    kind: NSEventType,
) -> Result<(), String> {
    // A detached child can remain Cocoa's receiver for this gesture in the same
    // original window. Obtain that live window number from the Rig's actual parent
    // view, never remount a closed child merely to supply a window.
    let window = rig.view.window().ok_or("格拖选真实窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, 1, 1.0,
    ).ok_or("无法构造真实格拖选鼠标事件")?;
    // SAFETY: Use the same actual main-thread responder and real window
    // coordinates, delivering each synchronous live event once.
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![view, mouseUp: &*event];
        } else {
            unreachable!("格拖选只交原 Down/Drag/Up");
        }
    }
    Ok(())
}

/// Cross from an actually edited child into its parent. Down, successive drag,
/// and up arrive without loop delivery, so asynchronous handoff must retain the
/// endpoint. The old parent Scene may legitimately remain Ready before commit;
/// verify actual selection, one-entry history, and source rather than internal stages.
fn ui_fast_pointer(rig: &mut Rig) -> Result<(), String> {
    let seed = UI_SOURCE[..UI_SOURCE.find("seed").expect("固定格 seed")]
        .encode_utf16()
        .count();
    let after = UI_SOURCE[..UI_SOURCE.find("after-ui").expect("固定正文尾")]
        .encode_utf16()
        .count();
    for (replace_up, stage) in [
        (false, "table_ui_fast_drag_queued_up"),
        (true, "table_ui_fast_drag_new_down_revokes_up"),
    ] {
        // Obtain each case's points from actual original layout after prior Undo.
        // Equal-length local changes preserve independent canonical target offsets.
        let first_point = links::point(rig, after + 1)?;
        let second_point = links::point(rig, after + 3)?;
        let up_point = links::point(rig, after + 6)?;
        let new_point = links::point(rig, 2)?;
        let seed_point = links::point(rig, seed)?;
        let started = Instant::now();
        links::mouse(rig, seed_point, NSEventType::LeftMouseDown)?;
        links::mouse(rig, seed_point, NSEventType::LeftMouseUp)?;
        let cell = ui_active(rig, None, started, stage)?;
        let child = cell.view().map_err(|cause| cause.to_string())?;
        let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
        {
            let document = rig.document.upgrade().ok_or("快拖原Doc已归还")?;
            if document.borrow().can_undo() {
                return Err("快拖开始前上一流程仍有未撤销的正文历史".into());
            }
        }
        // Actual NSTextInputClient changes only the local draft. Parent generation,
        // text, and character history must remain untouched before handoff commit.
        let edit = observe(&cell)?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**child);
        // SAFETY: The actual system-focused child contains the fixed seed's second
        // ASCII character, and the original replacement range is valid.
        unsafe {
            client.insertText_replacementRange(&NSString::from_str("E"), NSRange::new(1, 1));
        }
        frame::wait_complete_frame("table_ui_fast_drag_local_edit", &cell, edit)?;
        ui_local(&child, "sEed")?;
        ui_document(rig, UI_SOURCE)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != original {
            return Err("快拖前局部输入已经改动父文档的原输入事实".into());
        }
        let key = child
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*cell, &key.request)
            .map_err(|cause| cause.to_string())?;
        let first = scene
            .first_rect(NSRange::new(1, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(1, 1) || first.rect.size.width <= 0.0 {
            return Err("编辑后的真实子格首击没有独立local字符框".into());
        }
        let down_point = child.convertPoint_toView(
            NSPoint::new(
                first.rect.origin.x + first.rect.size.width * 0.1,
                first.rect.origin.y + first.rect.size.height * 0.5,
            ),
            None,
        );
        drop((scene, key)); // Local retention of an old Scene must not conceal precommit reader retirement or strict close.
        ui_child_mouse(rig, &child, down_point, NSEventType::LeftMouseDown)?;
        let frame = observe(&rig.body)?;
        ui_child_mouse(rig, &child, first_point, NSEventType::LeftMouseDragged)?;
        ui_child_mouse(rig, &child, second_point, NSEventType::LeftMouseDragged)?;
        ui_child_mouse(rig, &child, up_point, NSEventType::LeftMouseUp)?;
        if replace_up {
            // Wait for no commit/new frame. A new actual parent down revokes the
            // old up, while its real click still commits the dirty draft only once.
            links::mouse(rig, new_point, NSEventType::LeftMouseDown)?;
            links::mouse(rig, new_point, NSEventType::LeftMouseUp)?;
            ui_child_mouse(rig, &child, second_point, NSEventType::LeftMouseDragged)?;
        }
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let expected = if replace_up {
            MarkdownTextSelection::caret(2)
        } else {
            MarkdownTextSelection {
                anchor_utf16: seed + 1,
                head_utf16: after + 6,
            }
        };
        let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if actual.selection != expected || rig.body.active_cell_body().is_some() {
            return Err(format!(
                "{stage}: 真实排队交接没有保最后一次手势端点：actual={:?}, expected={expected:?}",
                actual.selection
            ));
        }
        ui_document(rig, UI_FAST_COMMITTED)?;
        {
            let document = rig.document.upgrade().ok_or("快拖提交后原Doc已归还")?;
            let document = document.borrow();
            if original.generation.checked_add(1) != Some(document.generation())
                || !document.can_undo()
                || document.can_redo()
            {
                return Err("快拖没有只提交一笔真实修改，或仍保留已被新编辑替代的Redo".into());
            }
        }
        // Cocoa may deliver late drag to the old child after up; it cannot change
        // already completed parent Input facts.
        ui_child_mouse(rig, &child, first_point, NSEventType::LeftMouseDragged)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != actual {
            return Err("真实Up后旧child的迟到Drag复活了原手势".into());
        }
        rig.remember_receipt()?;
        drop((child, cell));
        // Selection handoff adds no character history. One actual Undo restores
        // complete original bytes, CRLF, and the pre-edit canonical selection together.
        let undo = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame("table_ui_fast_drag_one_undo", &rig.body, undo)?;
        ui_document(rig, UI_SOURCE)?;
        let restored = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let document = rig.document.upgrade().ok_or("快拖Undo后原Doc已归还")?;
        let document = document.borrow();
        if restored.selection != original.selection || document.can_undo() || !document.can_redo() {
            return Err(format!(
                "{stage}: 一次Undo没有恢复原选择或只有一笔正文历史：actual={:?}, expected={:?}",
                restored.selection, original.selection
            ));
        }
        drop(document);
        rig.remember_receipt()?;
    }
    println!(
        "native_body_table_cell_ui fast_caret_drag_queued_handoff=true actual_dirty_cell_commit=true one_document_undo=true original_disk_unchanged=true actual_old_child_drag_and_up=true late_drag_revoked=true new_down_revokes_accepted_up=true word_cross_grid_tested=false physical_ime_tested=false formal=false whole_product=false"
    );
    Ok(())
}

/// Run actual cell focus/edit/history/navigation and cross-boundary pointer cases
/// in one synthetic document, then strictly release it before reporting results.
fn active_table_ui(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(UI_SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            UI_SOURCE.encode_utf16().count(),
            UI_SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = ui_flow(&mut rig)
            .and_then(|()| ui_fast_pointer(&mut rig))
            .and_then(|()| cell_find::run(&mut rig));
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    result?;
    println!(
        "native_body_table_cell_ui actual_mouse_and_native_client=true local_history_parent_unchanged=true required_row_navigation=true terminal_append_one_undo=true strict_original_release=true fast_caret_drag_tested=true cross_grid_word_tested=false physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}
