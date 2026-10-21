//! Observe actual column alignment, soft lines, trailing spaces, and active-cell
//! geometry in the same short table. Construct independent TextKit references
//! only after the product graph has strictly closed.

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;
use objc2_foundation::{NSIntersectionRect, NSIntersectsRect, NSPointInRect};
use omni_markdown_core::MarkdownTableAlignment;

const SOURCE: &str = concat!(
    "before\r\n\r\n",
    "| Left | Center | Right |\r\n",
    "| :--- | :---: | ---: |\r\n",
    "| left alpha beta gamma delta epsilon zeta eta theta iota kappa | center alpha beta gamma delta epsilon zeta eta theta iota kappa | right alpha beta gamma delta epsilon zeta eta theta iota kappa |\r\n",
    "| `L  ` | `C  ` | `R  ` |\r\n",
    "\r\nafter\r\n",
);
const CELLS: [(&str, MarkdownTableAlignment); 6] = [
    (
        "left alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Left,
    ),
    (
        "center alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Center,
    ),
    (
        "right alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Right,
    ),
    ("L  ", MarkdownTableAlignment::Left),
    ("C  ", MarkdownTableAlignment::Center),
    ("R  ", MarkdownTableAlignment::Right),
];

// This fixed Latin LTR fixture has one byte per UTF-16 unit; its indexing is no
// product Unicode decoder. Every alignment includes actual wrapped lines, while
// code cells retain two trailing spaces. Less than one page of fixture observations
// grants no permission for product long-document or whole-cell materialization.

/// Retain only bounded displayed character rectangles after close. Fixed Markdown
/// literals determine alignment independently of candidate presentation styles.
struct ObservedCell {
    text: &'static str,
    alignment: MarkdownTableAlignment,
    content: NSRect,
    clip: NSRect,
    // None records actual Pending only. An independent Native glyph rectangle
    // decides whether it was justified after product closure. Keep every fixed
    // character in order, without filtering whitespace, line ends, or failed candidates.
    characters: Vec<Option<NSRect>>,
    active: bool,
}

fn canonical(text: &str) -> usize {
    SOURCE[..SOURCE.find(text).expect("固定对齐格唯一存在")]
        .encode_utf16()
        .count()
}

/// Natural column widths may exceed the clip. Reveal the right-aligned last soft
/// line through its original target before observing it. Request only fixed ASCII
/// first/last graphemes, changing no selection, size, hit point, or scrolling policy.
fn reveal_sample(rig: &Rig, text: &str) -> Result<(), String> {
    let start = canonical(text);
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    // The fixed R-plus-two-spaces sample keeps trailing spaces outside alignment
    // width; SourceTarget cannot certify offscreen glyphs as visible. Reveal R,
    // then compare every space's rectangle/Pending/caret endpoints independently.
    // Other samples retain both actual first/last targets, including the wrapped tail letter.
    let tail = (text != "R  ").then_some(start + text.len() - 1);
    for position in std::iter::once(start).chain(tail) {
        let range = NSRange::new(position, 1);
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        match rig.view.verify_find_target_visible(&key, range) {
            Ok(_) => continue, // Existing actual visibility needs no extra test-induced scrolling/publication.
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending,
            ) => {}
            Err(cause) => return Err(cause.to_string()),
        }
        // Only failed actual visibility coverage authorizes SourceTarget. Product
        // scrolling/layout/complete CPU publication create the new key within the
        // original three seconds; never clamp offscreen Pending into a fake first-page glyph.
        let frame = observe(&rig.body)?;
        let target = rig
            .body
            .request_source_target(range)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame("table_aligned_sample_reveal", &rig.body, frame)?;
        let proof = rig
            .body
            .take_source_target_visible(&target)
            .map_err(|cause| cause.to_string())?
            .ok_or("对齐 sample 没有原目标的真实可见证明")?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let (published, paints) = rig
            .view
            .probe_cpu_published_frame()
            .ok_or("对齐目标没有真实完整 CPU 发布")?;
        if proof.range() != range
            || !proof.key().same(&key)
            || !published.same(&key)
            || paints == 0
            || rig.view.cpu_paints() != Some(paints)
            || rig.view.cpu_paint_pending() != Some(false)
            || rig.view.input_state().map_err(|cause| cause.to_string())? != before
        {
            return Err("对齐目标的原 proof、CPU 帧或完整输入已经改变".into());
        }
        rig.view
            .verify_find_target_visible(&key, range)
            .map_err(|cause| cause.to_string())?;
    }
    Ok(())
}

/// Each actual character rectangle preserves its canonical range. The left side
/// of nonspace glyphs must also hit the same original source position.
fn capture(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    text: &'static str,
    alignment: MarkdownTableAlignment,
    active: bool,
) -> Result<ObservedCell, String> {
    let key = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let start = if active { 0 } else { canonical(text) };
    // Active Source coordinates start at local zero; read-only preview keeps
    // original Markdown canonical endpoints. Both require the current key; parent
    // source offsets cannot interpret a child's raw draft.
    let first = scene.first_rect(NSRange::new(start, 1)).map_err(|cause| {
        format!("对齐 sample 首字：text={text:?} canonical={start} active={active}: {cause}")
    })?;
    let point = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let content = if active {
        // Obtain the actual child's layout lane/local coordinates through its
        // Native transform, not the parent clip width as a guessed cell size.
        NSRect::new(
            NSPoint::new(
                key.request.virtual_viewport.layout_origin_x
                    - key.request.virtual_viewport.document_origin.x,
                key.request.viewport.origin.y,
            ),
            NSSize::new(
                key.request.virtual_viewport.layout_width,
                key.request.viewport.size.height,
            ),
        )
    } else {
        // Content geometry supplies only the actual layout lane; fixed literals
        // still own text/alignment expectations. If the aligned glyph cannot hit
        // its actual cell, fail rather than searching a neighboring column for a passing frame.
        let hit = scene
            .cell_hit(point, false)
            .map_err(|cause| cause.to_string())?
            .ok_or("对齐字符没有落在真实格内容中")?;
        if hit.head_utf16 != start {
            return Err(format!(
                "对齐原格点击来源不符：actual={} expected={start}",
                hit.head_utf16
            ));
        }
        hit.content_frame
    };
    let clip = NSIntersectionRect(content, key.request.viewport);
    let mut characters = Vec::with_capacity(text.len());
    // Record scalar geometry for six fixed short strings, retaining no Scene,
    // reader, font, or product paragraph directory. Per-character canonical
    // identity also prevents hidden backticks from mapping code-tail spaces beyond the closing marker.
    for (local, byte) in text.bytes().enumerate() {
        let at = start + local;
        let value = match scene.first_rect(NSRange::new(at, 1)) {
            Ok(value) => value,
            Err(ViewError::Pending) => {
                // Record the public result only. After close, an independently
                // visible Native rectangle still makes this a product failure.
                // Check both original caret endpoints without shifting an offscreen
                // character into a neighbor or splitting its grapheme.
                for boundary in [at, at + 1] {
                    let caret = scene
                        .first_rect(NSRange::new(boundary, 0))
                        .map_err(|cause| {
                            format!(
                                "对齐离屏字符原端点：text={text:?} canonical={boundary} active={active}: {cause}"
                            )
                        })?;
                    if caret.actual_range != NSRange::new(boundary, 0) {
                        return Err("离屏对齐字符没有保留原 canonical 光标端点".into());
                    }
                }
                characters.push(None);
                continue;
            }
            Err(cause) => {
                return Err(format!(
                    "对齐 sample 字符：text={text:?} canonical={at} active={active}: {cause}"
                ));
            }
        };
        if value.actual_range != NSRange::new(at, 1)
            || value.rect.size.width <= 0.0
            || value.rect.size.height <= 0.0
        {
            return Err(format!(
                "对齐格原字符框错误：active={active} canonical={at} actual={:?} rect={:?}",
                value.actual_range, value.rect
            ));
        }
        if byte != b' ' {
            // Space endpoints can belong to adjacent soft lines. Keep a fixed
            // side for nonspace glyphs rather than moving points to obtain a passing hit.
            let point = NSPoint::new(
                value.rect.origin.x + value.rect.size.width * 0.125,
                value.rect.origin.y + value.rect.size.height * 0.5,
            );
            let actual = NSPointInRect(point, clip)
                .then(|| scene.hit(point))
                .transpose()
                .map_err(|cause| cause.to_string())?;
            if actual.is_some_and(|actual| actual != at) {
                return Err(format!(
                    "对齐后绘图和命中不一致：active={active} actual={actual:?} expected={at} rect={:?}",
                    value.rect
                ));
            }
        }
        characters.push(Some(value.rect));
    }
    let current = || {
        view.capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // Synchronous and item-budgeted frames of this actual key must match exactly.
    // A paint-only baseline patch cannot satisfy the geometry/hit round trips
    // above, nor can one completed frame authorize a successor Scene's painting.
    selection::exact_scene_rgba(
        scene,
        &key,
        view.window().ok_or("对齐窗口已归还")?.backingScaleFactor(),
        &current,
    )?;
    Ok(ObservedCell {
        text,
        alignment,
        content,
        clip,
        characters,
        active,
    })
}

/// Read-only preview, activation, and cancellation preserve canonical text,
/// history, and disk. Complete integrity checks are limited to this fixed short fixture.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("对齐原 Doc 已归还")?;
    let document = original.borrow();
    if document.generation() != 0
        || document.is_dirty()
        || document.can_undo()
        || document.can_redo()
    {
        return Err("对齐显示/活动格取消产生了正文或历史".into());
    }
    let mut actual = [0_u16; 128];
    // Fixed small chunks inspect this synthetic fixture only. User notes never
    // pass through this helper, which assembles no complete string.
    let mut expected = SOURCE.encode_utf16();
    for start in (0..document.utf16_len()).step_by(actual.len()) {
        let used = actual.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut actual[..used])
            .map_err(|cause| cause.to_string())?;
        if actual[..used]
            .iter()
            .any(|unit| expected.next() != Some(*unit))
        {
            return Err("对齐流程改写了原 Markdown 或 CRLF".into());
        }
    }
    if expected.next().is_some() || document.utf16_len() != SOURCE.encode_utf16().count() {
        return Err("对齐流程改变了原长度".into());
    }
    drop(document);
    drop(original);
    // End every document lease before independent disk verification. An unsaved
    // active cell must not silently persist its draft.
    if std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())? != SOURCE.as_bytes() {
        return Err("对齐流程写入了原磁盘".into());
    }
    Ok(())
}

/// Standard short-table references use downward container coordinates. Actual
/// content frames provide the shared origin, without fitting reference glyph widths/heights.
fn original_frame(frame: NSRect, content: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            content.origin.x + frame.origin.x,
            content.origin.y + content.size.height - frame.origin.y - frame.size.height,
        ),
        frame.size,
    )
}

/// Lay out independent Native short strings only after product closure. Actual
/// NSParagraphStyle determines reference alignment and trailing-space behavior.
fn original(observed: &[ObservedCell]) -> Result<(), String> {
    let font = crate::native_surface::text_attributes::native_markdown_document_fixed_font(14.0)
        .map_err(|cause| cause.to_string())?;
    for cell in observed {
        // Six fixed ASCII letter/space strings have one byte/scalar/grapheme each.
        // Derive endpoints from those literals alone, never product geometry,
        // selection state, or Core grapheme output.
        if !cell.text.is_ascii() {
            return Err("固定 ASCII 对齐参考出现了非 ASCII 字面".into());
        }
        let boundaries: Vec<_> = (0..=cell.text.len()).collect();
        let lines = super::super::paragraph_oracle::measure_table_cell_graphemes(
            cell.text,
            cell.content.size.width,
            &font,
            cell.alignment,
            &boundaries,
        )?;
        if cell.text.len() > 10 && lines.len() < 2 {
            return Err("独立原对齐夹具没有真实软换行".into());
        }
        // Every soft line uses actual Native source ranges, retaining trailing
        // spaces in reference text. Right alignment may place spaces beyond the
        // content edge; only an independently offscreen rectangle justifies Pending.
        let original_characters = lines.iter().map(|line| line.graphemes.len()).sum::<usize>();
        if original_characters != cell.characters.len() {
            return Err("ASCII 对齐原 Native GCB 没有覆盖全部固定字符".into());
        }
        for line in &lines {
            if line.source.is_empty() {
                continue;
            }
            let actual_line = cell
                .characters
                .get(line.source.clone())
                .ok_or("原 Native 软行范围越界")?;
            if let Some(first) = actual_line.iter().flatten().next()
                && actual_line
                    .iter()
                    .flatten()
                    .any(|character| character.origin.y != first.origin.y)
            {
                // Characters in the same actual Native soft line need one
                // candidate y; aligning only the first line cannot satisfy later rows.
                return Err(format!(
                    "对齐格与原 Native 真实软行端点不同：alignment={:?} active={} source={:?}",
                    cell.alignment, cell.active, line.source
                ));
            }
            for grapheme in &line.graphemes {
                let [actual] = cell
                    .characters
                    .get(grapheme.source.clone())
                    .ok_or("原 Native 字符越出固定 ASCII 字面")?
                else {
                    return Err("固定 ASCII 字面不是原 Native 的单单元 GCB".into());
                };
                let positioned = grapheme
                    .positioned_frame
                    .ok_or("独立 Native GCB 没有完成逐glyph26.6定位证明")?;
                let expected = original_frame(positioned, cell.content);
                let visible = NSIntersectsRect(expected, cell.clip);
                match (visible, actual) {
                    (false, None) => {} // The actual Native rectangle proves offscreen placement; whitespace gets no blanket exemption.
                    (true, Some(actual))
                        if (actual.origin.x - expected.origin.x).abs() <= 1.0 / 64.0
                            && (actual.size.width - expected.size.width).abs() <= 1.0 / 64.0 => {}
                    _ => {
                        return Err(format!(
                            "原 Native 对齐字符与实际 clip/几何不同：alignment={:?} active={} source={:?} actual={actual:?} expected={expected:?} visible={visible} clip={:?} content={:?} original_line={:?} original_baseline={:?} raw_native={:?}",
                            cell.alignment,
                            cell.active,
                            grapheme.source,
                            cell.clip,
                            cell.content,
                            line.source,
                            line.baseline,
                            grapheme.frame,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn run_ascii(mtm: MainThreadMarker) -> Result<(), String> {
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
        let result = (|| -> Result<Vec<ObservedCell>, String> {
            let mut observed = Vec::with_capacity(8);
            for (text, alignment) in CELLS {
                reveal_sample(&rig, text)?;
                observed.push(capture(&rig.body, &rig.view, text, alignment, false)?);
            }
            rig.panel.makeKeyAndOrderFront(None);
            // Mount the sole child through actual parent down/up, never directly
            // creating CellSession or changing its active slot. Up precedes Ready;
            // ui_active still requires actual focus/full pixels within three seconds,
            // rather than treating owner existence as completion.
            for (text, alignment) in &CELLS[1..3] {
                reveal_sample(&rig, text)?;
                let point = links::point(&rig, canonical(text))?;
                let started = Instant::now();
                links::mouse(&rig, point, NSEventType::LeftMouseDown)?;
                links::mouse(&rig, point, NSEventType::LeftMouseUp)?;
                let child = ui_active(&mut rig, None, started, "table_aligned_active_cell")?;
                let view = child.view().map_err(|cause| cause.to_string())?;
                ui_local(&view, text)?;
                observed.push(capture(&child, &view, text, *alignment, true)?);
                // Existing Escape cancels only this draft. No app quit command or
                // added shortcut assists teardown.
                let frame = observe(&rig.body)?;
                view.command(sel!(cancelOperation:))
                    .map_err(|cause| cause.to_string())?;
                frame::wait_complete_frame("table_aligned_cell_cancel", &rig.body, frame)?;
                if rig.body.active_cell_body().is_some() {
                    return Err("对齐格取消后仍保活动 owner".into());
                }
                drop(view);
                drop(child);
                // Return temporary claims before the next cell. Final strict
                // closure still checks actual Native/FD/mmap/font/worker resources.
                unchanged(&rig)?;
            }
            rig.remember_receipt()?;
            unchanged(&rig)?;
            Ok(observed)
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    // Even interaction failure first completes actual closure above. Allocate
    // the standard reference only afterward; scalar rectangles/alignment remain,
    // but simultaneous graphs cannot conceal delayed product-resource release.
    original(&result?)?;
    println!(
        "native_body_table_alignment actual_left_center_right=true actual_soft_lines_and_trailing_spaces=true active_cell_geometry=true canonical_hit_and_zero_rgba=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}

const UNICODE_SOURCE: &str = concat!(
    "before\r\n\r\n",
    "| Center | Right |\r\n",
    "| :---: | ---: |\r\n",
    "| `אבג  ` | `דהו  ` |\r\n",
    "| `x\u{0600}  ` | `y\u{0600}  ` |\r\n",
    "\r\nafter\r\n",
);

/// Fixed Hebrew/Prepend literals independently specify grapheme endpoints without
/// copying product Unicode decoding/segmentation. U+0600 joins its first following
/// space; the next separate space may lie outside alignment width. Hits cannot split either cluster.
const UNICODE_CELLS: [(&str, MarkdownTableAlignment, &[usize]); 4] = [
    ("אבג  ", MarkdownTableAlignment::Center, &[0, 1, 2, 3, 4, 5]),
    ("דהו  ", MarkdownTableAlignment::Right, &[0, 1, 2, 3, 4, 5]),
    ("x\u{0600}  ", MarkdownTableAlignment::Center, &[0, 1, 3, 4]),
    ("y\u{0600}  ", MarkdownTableAlignment::Right, &[0, 1, 3, 4]),
];

struct UnicodeCell {
    text: &'static str,
    alignment: MarkdownTableAlignment,
    content: NSRect,
    clip: NSRect,
    graphemes: Vec<(std::ops::Range<usize>, Option<NSRect>)>,
}

/// Reveal the first scalar through production SourceTarget. An internal UBA seam
/// must not break the original complete grapheme's actual visibility proof.
fn reveal_unicode_grapheme(rig: &Rig, text: &str) -> Result<(), String> {
    let Some(within) = text.find('\u{0600}') else {
        return Ok(()); // Ordinary Hebrew cases verify existing geometry without an extra target action.
    };
    let canonical = UNICODE_SOURCE[..UNICODE_SOURCE.find(text).expect("固定 Unicode 格存在")]
        .encode_utf16()
        .count()
        + text[..within].encode_utf16().count();
    let range = NSRange::new(canonical, 1);
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(range)
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_prepend_original_find_target", &rig.body, frame)?;
    let proof = rig
        .body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("Prepend 原首标量没有真实可见证明")?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let (published, paints) = rig
        .view
        .probe_cpu_published_frame()
        .ok_or("Prepend 原目标没有完整 CPU 帧")?;
    if proof.range() != range
        || !proof.key().same(&key)
        || !published.same(&key)
        || paints == 0
        || rig.view.cpu_paints() != Some(paints)
        || rig.view.cpu_paint_pending() != Some(false)
        || rig.view.input_state().map_err(|cause| cause.to_string())? != before
    {
        return Err("Prepend 原目标与完整输入、实际 CPU 发布不一致".into());
    }
    rig.view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    Ok(())
}

/// Observe the same actual cell through public Scene character/caret geometry,
/// hit testing, and directed selection, without reading internal layout fields.
fn unicode_cell(
    rig: &Rig,
    text: &'static str,
    alignment: MarkdownTableAlignment,
    boundaries: &[usize],
) -> Result<UnicodeCell, String> {
    let canonical = UNICODE_SOURCE[..UNICODE_SOURCE.find(text).expect("固定 Unicode 格存在")]
        .encode_utf16()
        .count();
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    // The first grapheme must actually be visible and hit this cell before its
    // content frame is authorized; column numbers/glyph widths cannot guess the clip.
    let first = scene
        .first_rect(NSRange::new(canonical, boundaries[1]))
        .map_err(|cause| cause.to_string())?;
    let first_point = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let content = scene
        .cell_hit(first_point, false)
        .map_err(|cause| cause.to_string())?
        .ok_or("Unicode 对齐首字没有命中原真实格")?
        .content_frame;
    let clip = NSIntersectionRect(content, key.request.viewport);
    let mut graphemes = Vec::new();
    for pair in boundaries.windows(2) {
        let from = canonical + pair[0];
        let to = canonical + pair[1];
        let value = match scene.first_rect(NSRange::new(from, to - from)) {
            Ok(value) => value,
            Err(ViewError::Pending) => {
                // Offscreen status supplies no glyph rectangle, but both caret
                // queries must preserve original complete-grapheme endpoints.
                // Compare None with actual Native geometry after strict closure;
                // a visible yet Pending grapheme cannot pass.
                for boundary in [from, to] {
                    let caret = scene
                        .first_rect(NSRange::new(boundary, 0))
                        .map_err(|cause| {
                            format!(
                                "Unicode 对齐离屏 GCB 原端点：text={text:?} source={from}..{to} caret={boundary}: {cause}"
                            )
                        })?;
                    if caret.actual_range != NSRange::new(boundary, 0) {
                        return Err("Unicode 离屏 GCB 没有保留原 canonical 光标端点".into());
                    }
                }
                graphemes.push((pair[0]..pair[1], None));
                continue;
            }
            Err(cause) => {
                return Err(format!(
                    "Unicode 对齐字符框：text={text:?} source={from}..{to}: {cause}"
                ));
            }
        };
        if value.actual_range != NSRange::new(from, to - from)
            || value.rect.size.width <= 0.0
            || value.rect.size.height <= 0.0
        {
            return Err(format!(
                "Unicode 对齐格没有原完整 GCB 框：text={text:?} canonical={from}..{to} actual={:?} rect={:?}",
                value.actual_range, value.rect,
            ));
        }
        let start = scene.first_rect(NSRange::new(from, 0)).map_err(|cause| {
            format!("Unicode 对齐起点：text={text:?} source={from}..{to}: {cause}")
        })?;
        let end = scene.first_rect(NSRange::new(to, 0)).map_err(|cause| {
            format!("Unicode 对齐末点：text={text:?} source={from}..{to}: {cause}")
        })?;
        if start.actual_range != NSRange::new(from, 0)
            || end.actual_range != NSRange::new(to, 0)
            || (start.rect.origin.x.min(end.rect.origin.x) - value.rect.origin.x).abs() > 1.0 / 64.0
            || ((start.rect.origin.x - end.rect.origin.x).abs() - value.rect.size.width).abs()
                > 1.0 / 64.0
        {
            return Err(format!(
                "Unicode 对齐的整簇光标和字符框不自洽：text={text:?} source={from}..{to} rect={:?} start_range={:?} start={:?} end_range={:?} end={:?}",
                value.rect, start.actual_range, start.rect, end.actual_range, end.rect,
            ));
        }
        let mut sides = [None; 2];
        for (index, fraction) in [0.125, 0.875].into_iter().enumerate() {
            let point = NSPoint::new(
                value.rect.origin.x + value.rect.size.width * fraction,
                value.rect.origin.y + value.rect.size.height * 0.5,
            );
            if !NSPointInRect(point, clip) {
                // A partially visible grapheme's fixed hit point may lie in a
                // neighboring cell. Keep the point rather than moving it to pass;
                // independent geometry still must match, and only actual clipped
                // points belong to this cell's interaction domain.
                continue;
            }
            let hit = scene.hit(point).map_err(|cause| {
                format!(
                    "Unicode 对齐点击：text={text:?} source={from}..{to} point={point:?}: {cause}"
                )
            })?;
            if hit != from && hit != to {
                return Err(format!(
                    "Unicode 对齐点击切开真实 GCB：text={text:?} range={from}..{to} hit={hit} point={point:?}",
                ));
            }
            let (selection, _) = scene
                .mouse_selection(point, 1, Some(from), true)
                .map_err(|cause| cause.to_string())?;
            if selection.anchor_utf16 != from || selection.head_utf16 != hit {
                return Err("Unicode 对齐有向选择没有保持真实 GCB 锚点与端点".into());
            }
            sides[index] = Some(hit);
        }
        if let [Some(left), Some(right)] = sides
            && left == right
        {
            return Err("Unicode 对齐整簇的两个真实点击侧没有得到不同端点".into());
        }
        graphemes.push((pair[0]..pair[1], Some(value.rect)));
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    selection::exact_scene_rgba(scene, &key, rig.panel.backingScaleFactor(), &current)?;
    Ok(UnicodeCell {
        text,
        alignment,
        content,
        clip,
        graphemes,
    })
}

/// Read/select these short fixtures only in their original mmap graph. Independent
/// TextKit reference construction waits until strict product closure completes.
fn run_unicode(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(UNICODE_SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            UNICODE_SOURCE.encode_utf16().count(),
            UNICODE_SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| -> Result<Vec<UnicodeCell>, String> {
            let mut observed = Vec::new();
            for (text, alignment, boundaries) in UNICODE_CELLS {
                reveal_unicode_grapheme(&rig, text)?;
                observed.push(unicode_cell(&rig, text, alignment, boundaries)?);
            }
            let original = rig.document.upgrade().ok_or("Unicode 对齐原 Doc 已归还")?;
            let document = original.borrow();
            if document.generation() != 0
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
                || document.utf16_len() != UNICODE_SOURCE.encode_utf16().count()
            {
                return Err("Unicode 对齐读取/选择改变了正文或历史".into());
            }
            let mut expected = UNICODE_SOURCE.encode_utf16();
            let mut units = [0_u16; 64];
            for offset in (0..document.utf16_len()).step_by(units.len()) {
                let used = units.len().min(document.utf16_len() - offset);
                document
                    .copy_utf16_code_units(offset, &mut units[..used])
                    .map_err(|cause| cause.to_string())?;
                if units[..used]
                    .iter()
                    .any(|unit| expected.next() != Some(*unit))
                {
                    return Err("Unicode 对齐选择改写了原字符或 CRLF".into());
                }
            }
            drop(document);
            drop(original);
            if expected.next().is_some()
                || std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())?
                    != UNICODE_SOURCE.as_bytes()
            {
                return Err("Unicode 对齐选择改写了磁盘或源长度".into());
            }
            rig.remember_receipt()?;
            Ok(observed)
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    let font = crate::native_surface::text_attributes::native_markdown_document_fixed_font(14.0)
        .map_err(|cause| cause.to_string())?;
    for cell in result? {
        // Independent UNICODE_CELLS declares Prepend/first-space as 1..3 and the
        // final space as 3..4. Retrieve those fixed literal/alignment facts rather
        // than deriving Native query ranges from candidate grapheme observations.
        let boundaries = UNICODE_CELLS
            .iter()
            .find_map(|(text, alignment, boundaries)| {
                (*text == cell.text && *alignment == cell.alignment).then_some(*boundaries)
            })
            .ok_or("Unicode 对齐观察没有对应的原固定 GCB 字面")?;
        let lines = super::super::paragraph_oracle::measure_table_cell_graphemes(
            cell.text,
            cell.content.size.width,
            &font,
            cell.alignment,
            boundaries,
        )?;
        let original: Vec<_> = lines.iter().flat_map(|line| &line.graphemes).collect();
        if original.len() != cell.graphemes.len() {
            return Err(format!(
                "Unicode 对齐原 Native GCB 与固定字面不同：text={:?} actual_count={} original_count={}",
                cell.text,
                cell.graphemes.len(),
                original.len(),
            ));
        }
        for ((source, actual), original) in cell.graphemes.iter().zip(original) {
            // Both engines use the same fixed text, actual column width, and
            // paragraph style. Compare actual RTL visual positions per grapheme;
            // do not pin the logical first character to the baseline or trim the
            // complete Prepend cluster from the reference.
            if *source != original.source {
                return Err("Unicode 对齐观察与独立 Native 的原 GCB 端点不同".into());
            }
            let positioned = original
                .positioned_frame
                .ok_or("独立 Native Unicode GCB 没有完成逐glyph26.6定位证明")?;
            let expected = original_frame(positioned, cell.content);
            let visible = NSIntersectsRect(expected, cell.clip);
            match (visible, actual) {
                (false, None) => {} // Actual reference geometry is offscreen; retain the grapheme instead of deleting trailing spaces.
                (true, Some(actual))
                    if (actual.origin.x - expected.origin.x).abs() <= 1.0 / 64.0
                        && (actual.size.width - expected.size.width).abs() <= 1.0 / 64.0 => {}
                _ => {
                    return Err(format!(
                        "Unicode 真实表格对齐与 Native clip/几何不同：text={:?} alignment={:?} source={source:?} actual={actual:?} original_source={:?} expected={expected:?} visible={visible} clip={:?} content={:?} raw_native={:?}",
                        cell.text,
                        cell.alignment,
                        original.source,
                        cell.clip,
                        cell.content,
                        original.frame,
                    ));
                }
            }
        }
    }
    println!(
        "native_body_table_alignment_unicode actual_rtl_trailing_spaces=true actual_prepend_whitespace_whole_gcb=true native_grapheme_geometry=true canonical_caret_hit_selection=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    // Complete and close both independent actual graphs. An ASCII failure must
    // not conceal RTL/Prepend behavior or resource-release outcomes.
    let ascii = run_ascii(mtm);
    let unicode = run_unicode(mtm);
    match (ascii, unicode) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(ascii), Err(unicode)) => Err(format!("{ascii}; {unicode}")),
    }
}
