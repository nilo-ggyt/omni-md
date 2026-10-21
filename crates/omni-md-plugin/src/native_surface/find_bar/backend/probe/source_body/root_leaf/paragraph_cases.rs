//! Lay out the same fixed short fixture with actual TextKit and the paged candidate sequentially,
//! without simultaneous graphs or shared new geometry policy.

use super::*;
use objc2::{runtime::ProtocolObject, sel};
use objc2_app_kit::{NSEventType, NSTextInputClient};
use objc2_foundation::{NSNotFound, NSString};
use std::io::Read;

const CASES: [(&str, &str); 11] = [
    (
        "body",
        "outside\n\nalpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta\n\nend\n",
    ),
    (
        "atx",
        "# Heading alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\n\nend\n",
    ),
    (
        "setext",
        "outside\n\nHeading alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\n---\n\nend\n",
    ),
    (
        "fence",
        "outside\n\n```text\nalpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta\n```\n\nend\n",
    ),
    ("rule", "outside\n\n---\n\nend\n"),
    (
        "mixed_scripts",
        "outside\n\nwater H~2~O and x^2^ with **bold~sub~** and _italic^up^_.\n\nend\n",
    ),
    ("only_subscript", "~subscript~\n"),
    ("literal_carets", "^superscript^\n"),
    (
        "scoped_quote_plain",
        "outside\n\n> 中文引用段落保留真实字体与缩进，混合 emoji 😀 和 English words，在原可用列宽内自然换行。中文继续验证实际软行来源、基线、上下边距和段落边框的占位。\n\nend\n",
    ),
    (
        "scoped_quote_inline",
        "outside\r\n\r\n> **引用中文粗体\r\n> 续行粗体** 与 *引用斜体*、`引用代码\r\n> 跨行代码😀` 和普通中文保持同一实际容器来源。\r\n\r\nend\r\n",
    ),
    (
        "scoped_list_quote_code",
        "outside\n\n> - **嵌套列表中文\n>   续行粗体** 与 ` 代码\n>   \t跨行代码😀 ` 后面普通中文。\n\nend\n",
    ),
];

/// After candidate retirement retain finite width/displayed-line facts only; the reference needs no
/// simultaneous second body graph.
struct Candidate {
    lines: Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>,
    width: f64,
    height: f64,
    origin_y: f64,
    parts: Vec<super::paragraph_oracle::PartAdvance>,
}

/// Derive characters from fixed literals and this insertion only, verifying complete canonical chunks
/// without using new selection as expectation.
fn punctuation_source(rig: &Rig, source: &str, inserted: Option<usize>) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("标点原文稿已释放")?;
    let document = owner.borrow();
    let length = source.encode_utf16().count() + usize::from(inserted.is_some());
    if document.utf16_len() != length
        || document.byte_len() != source.len() + if inserted.is_some() { "中".len() } else { 0 }
    {
        return Err("标点实际输入改变了独立预期以外的文稿长度".into());
    }
    let mut expected = source.encode_utf16();
    let mut units = [0_u16; 128];
    for start in (0..length).step_by(units.len()) {
        let used = units.len().min(length - start);
        document
            .copy_utf16_code_units(start, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        for (offset, actual) in units[..used].iter().enumerate() {
            let expected = if inserted == Some(start + offset) {
                0x4e2d // This actual NSTextInputClient input inserts one character without replacing punctuation or later source text.
            } else {
                expected.next().ok_or("标点 canonical 出现额外字符")?
            };
            if *actual != expected {
                return Err(format!(
                    "标点输入或撤销改变了原 canonical 字符{}",
                    start + offset
                ));
            }
        }
    }
    if expected.next().is_some() {
        return Err("标点输入或撤销丢失了原 canonical 尾部".into());
    }
    Ok(()) // End document borrowing before the next Native call without retaining old Source or selection authority.
}

/// A separate descriptor reads synthetic Source only; display, mouse, unsaved input, and undo must not
/// silently write to disk.
fn unchanged_disk(rig: &Rig, name: &str, source: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for expected in source.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..expected.len()] != expected {
            return Err(format!("{name}: 段落交互改写了原磁盘字节"));
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err(format!("{name}: 段落原磁盘出现额外尾部"));
    }
    Ok(())
}

/// Original SourceTarget performs actual scroll without selection change; consume its visibility
/// receipt rather than mere range existence.
fn punctuation_reveal(
    rig: &mut Rig,
    index: usize,
    stage: &str,
    expected_complete: bool,
) -> Result<(), String> {
    let event = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    if expected_complete {
        frame::wait_complete_frame(stage, &rig.body, event)?;
    } else {
        frame::wait_frame(stage, &rig.body, event)?;
    }
    rig.body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("标点原目标没有真实可见收据")?;
    rig.remember_receipt()
}

/// Reacquire current Scene before actual mouse down; the character box and both carets must belong to
/// the same publication.
/// These punctuation marks are independent BMP graphemes; neither edge may be inferred from another
/// layout engine advances.
fn punctuation_point(rig: &Rig, index: usize, after: bool) -> Result<NSPoint, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let character = scene
        .first_rect(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    let left = scene
        .first_rect(NSRange::new(index, 0))
        .map_err(|cause| cause.to_string())?;
    let right = scene
        .first_rect(NSRange::new(index + 1, 0))
        .map_err(|cause| cause.to_string())?;
    // The fixed 1/64 tolerance is one original layout unit, without relaxing Native glyph/line geometry
    // requirements.
    if character.actual_range != NSRange::new(index, 1)
        || left.actual_range != NSRange::new(index, 0)
        || right.actual_range != NSRange::new(index + 1, 0)
        || character.rect.size.width <= 1.0
        || character.rect.size.height <= 1.0
        || (left.rect.origin.x - character.rect.origin.x).abs() > 1.0 / 64.0
        || (right.rect.origin.x - character.rect.max().x).abs() > 1.0 / 64.0
        || (left.rect.origin.y - right.rect.origin.y).abs() > 1.0 / 64.0
    {
        return Err(format!(
            "标点真实字符框与两侧 caret 不连续：index={index} character={:?}/{:?} left={:?}/{:?} right={:?}/{:?}",
            character.actual_range,
            character.rect,
            left.actual_range,
            left.rect,
            right.actual_range,
            right.rect,
        ));
    }
    let local = NSPoint::new(
        character.rect.origin.x + character.rect.size.width * if after { 0.85 } else { 0.15 },
        character.rect.origin.y + character.rect.size.height * 0.5,
    );
    let hit = scene.hit(local).map_err(|cause| cause.to_string())?;
    let expected = index + usize::from(after);
    if hit != expected {
        return Err(format!(
            "标点真实字符两侧命中错误：index={index} after={after} actual={hit} expected={expected}"
        ));
    }
    drop(scene); // Return the original Reader before convertPoint and actual NSEvent delivery; old layout authority cannot span a click.
    Ok(rig.view.convertPoint_toView(local, None))
}

/// Read canonical caret after actual Down/Up; subsequent input uses that selection without a known
/// replacement range bypassing hit testing.
fn punctuation_click(
    rig: &mut Rig,
    index: usize,
    insert: bool,
    source: &str,
    expected_complete: bool,
) -> Result<(), String> {
    for after in [false, true] {
        let point = punctuation_point(rig, index, after)?;
        let event = observe(&rig.body)?;
        super::links::mouse(rig, point, NSEventType::LeftMouseDown)?;
        super::links::mouse(rig, point, NSEventType::LeftMouseUp)?;
        if expected_complete {
            frame::wait_complete_frame("root_leaf_cjk_punctuation_click", &rig.body, event)?;
        } else {
            // The 40-point viewport requires its actual complete visible region only; click
            // reprojection may stop at covered prefix without forced tail reads.
            frame::wait_frame("root_leaf_cjk_punctuation_click", &rig.body, event)?;
        }
        let state = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if state.primary_range != NSRange::new(index + usize::from(after), 0) {
            return Err(format!(
                "标点真实鼠标没有落到独立 canonical caret：index={index} after={after} actual={:?}",
                state.primary_range
            ));
        }
        rig.remember_receipt()?;
    }
    if !insert {
        return Ok(()); // Resize/scroll round trips verify actual selection without appending history transactions for each geometry observation.
    }
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let event = observe(&rig.body)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
    // SAFETY: Use the original main-thread client; NSNotFound selects normal input at the actual
    // clicked selection.
    unsafe {
        client.insertText_replacementRange(
            &NSString::from_str("中"),
            NSRange::new(NSNotFound as usize, 0),
        );
    }
    frame::wait_complete_frame("root_leaf_cjk_punctuation_insert", &rig.body, event)?;
    let after = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if after.generation != before.generation + 1
        || after.primary_range != NSRange::new(index + 2, 0)
    {
        return Err("标点真实输入没有只提交一次当前 caret 的 canonical 事务".into());
    }
    punctuation_source(rig, source, Some(index + 1))?;
    rig.remember_receipt()?;
    let event = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("root_leaf_cjk_punctuation_undo", &rig.body, event)?;
    punctuation_source(rig, source, None)?;
    rig.remember_receipt()
}

/// Keep the original two quote fixtures and their styled punctuation, without changing literals to
/// avoid observed Native glyph differences.
fn punctuation_interaction(rig: &mut Rig, name: &str, source: &str) -> Result<(), String> {
    let mut positions = Vec::new();
    for scalar in ['、', '，', '。'] {
        if let Some(byte) = source.find(scalar) {
            positions.push(source[..byte].encode_utf16().count());
        }
    }
    if positions.is_empty() {
        return Err("引用固定字面缺少实际 CJK 标点交互对象".into());
    }
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() || !rig.panel.makeFirstResponder(Some(&**rig.view)) {
        return Err("标点真实 NSResponder 没有取得原窗口焦点".into());
    }
    for &index in &positions {
        punctuation_click(rig, index, true, source, true)?;
    }
    // Resize the same actual window while preserving font size, Source, and Input owner; a new Rig
    // cannot bypass reflow.
    for width in [320.0, 720.0] {
        let before = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let event = observe(&rig.body)?;
        rig.panel.setContentSize(NSSize::new(width, 348.0));
        frame::wait_complete_frame("root_leaf_cjk_punctuation_width", &rig.body, event)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if key.request.bounds_width == before.request.bounds_width
            || key.request.input != before.request.input
        {
            return Err("标点真实宽度重排没有换几何或错误改变了原输入".into());
        }
        for &index in &positions {
            punctuation_click(rig, index, false, source, true)?;
        }
    }
    // Shrink the already prepared same-width short Source to 40 points and actually scroll to tail
    // punctuation/back without extra unread-Source budget.
    let event = observe(&rig.body)?;
    rig.panel.setContentSize(NSSize::new(720.0, 148.0));
    frame::wait_complete_frame("root_leaf_cjk_punctuation_short_viewport", &rig.body, event)?;
    punctuation_reveal(rig, 0, "root_leaf_cjk_punctuation_top", true)?;
    let top = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let index = *positions.last().expect("原引用短例至少一个标点");
    punctuation_reveal(rig, index, "root_leaf_cjk_punctuation_far", true)?;
    let far = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if far.request.virtual_viewport.document_origin.y
        <= top.request.virtual_viewport.document_origin.y
    {
        return Err("标点滚动夹具没有实际移出原首屏".into());
    }
    // The fixed literal independently supplies the tail position; actual current character geometry
    // must prove it lies outside the 40-point viewport.
    // Do not derive expectation from progress.complete; later click-driven style reset need not prepare
    // a tail outside this viewport.
    let tail = source[..source.rfind("end").ok_or("标点固定短例缺少独立尾段")?]
        .encode_utf16()
        .count();
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &far.request)
        .map_err(|cause| cause.to_string())?;
    match scene.first_rect(NSRange::new(tail, 1)) {
        Err(ViewError::Pending) => {}
        Ok(_) => return Err("40pt标点窗口没有把固定原字面尾段移到屏外".into()),
        Err(cause) => return Err(cause.to_string()),
    }
    drop(scene); // Return Reader after offscreen proof, before actual mouse input can retain stale layout or block bounded-prefix reflow.
    punctuation_click(rig, index, false, source, false)?;
    punctuation_reveal(rig, 0, "root_leaf_cjk_punctuation_back", false)?;
    let back = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if back.request.virtual_viewport.document_origin.y
        != top.request.virtual_viewport.document_origin.y
    {
        return Err("标点滚回没有恢复原首屏真实位置".into());
    }
    let event = observe(&rig.body)?;
    rig.panel.setContentSize(NSSize::new(720.0, 348.0));
    frame::wait_complete_frame("root_leaf_cjk_punctuation_restore", &rig.body, event)?;
    punctuation_source(rig, source, None)?;
    unchanged_disk(rig, name, source)?;
    rig.remember_receipt()?;
    println!(
        "native_root_cjk_punctuation case={name} independent_literal_positions=true actual_mouse_down_up=true both_caret_edges=true actual_native_insert_and_undo=true actual_width_change=true original_source_reveal_roundtrip=true exact_canonical_and_disk=true original_three_second_frame_gate=true formal=false whole_product=false"
    );
    Ok(())
}

/// Compare pure actual sealed-window facts under the fixed one-unit 26.6 tolerance, without increasing
/// it after failure.
fn compare(
    case: &str,
    candidate: &Candidate,
    original: &[super::paragraph_oracle::OriginalLine],
) -> Result<(), String> {
    let actual = &candidate.lines;
    if actual.len() != original.len() {
        return Err(format!(
            "{case}: 原标准软行数{}与分页实际行数{}不符",
            original.len(),
            actual.len()
        ));
    }
    let tolerance = 1.0 / 64.0;
    let mut differences = Vec::new();
    for (ordinal, (actual, original)) in actual.iter().zip(original).enumerate() {
        let baseline = NSPoint::new(
            actual.baseline.x - 8.0,
            candidate.origin_y + candidate.height - actual.baseline.y - 8.0,
        ); // Remove fixture padding and convert Y-up coordinates only, without deriving candidate frames from the reference.
        eprintln!(
            "native_paragraph_geometry_observation case={case} ordinal={ordinal} source={:?}/{:?} paragraph={:?}/{:?} candidate_frame={:?} original_frame={:?} original_paragraph_frame={:?} baseline={baseline:?}/{:?} text_height={}/{} before={}/{} after={}/{}",
            actual.source,
            original.source,
            actual.paragraph,
            original.paragraph,
            actual.frame,
            original.frame,
            original.paragraph_frame,
            original.baseline,
            actual.text_height,
            original.frame.size.height,
            actual.before,
            original.before,
            actual.after,
            original.after
        ); // Log fixed-fixture pure values only; preserve tolerance on failure without later arithmetic rewriting original Native results.
        if actual.source != original.source
            || actual.paragraph != original.paragraph
            || (actual.text_height - original.frame.size.height).abs() > tolerance
            || (baseline.x - original.baseline.x).abs() > tolerance
            || (baseline.y - original.baseline.y).abs() > tolerance
            || (actual.before - original.before).abs() > tolerance
            || (actual.after - original.after).abs() > tolerance
        {
            differences.push(format!(
                "{case}: 原标准 Native 与分页实际行不同 ordinal={ordinal} source={:?}/{:?} paragraph={:?}/{:?} candidate_frame={:?} original_frame={:?} original_paragraph_frame={:?} baseline={baseline:?}/{:?} text_height={}/{} before={}/{} after={}/{} tolerance={tolerance}",
                actual.source,
                original.source,
                actual.paragraph,
                original.paragraph,
                actual.frame,
                original.frame,
                original.paragraph_frame,
                original.baseline,
                actual.text_height,
                original.frame.size.height,
                actual.before,
                original.before,
                actual.after,
                original.after
            ));
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences.join("\n"))
    }
}

/// Fully close each paged fixture through the same actual resource checks on failure, leaving no
/// temporary window for the next case.
fn run_case(mtm: MainThreadMarker, name: &str, source: &str) -> Result<Option<String>, String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(source.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            source.encode_utf16().count(),
            source.bytes().filter(|&byte| byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| {
            let key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let width = ((key.request.virtual_viewport.layout_width - 16.0) * 64.0).round() / 64.0;
            if key.request.virtual_viewport.document_origin.y != 0.0 {
                return Err(format!(
                    "{name}: 原短例真实列宽/正文原点不符 width={width} origin={:?}",
                    key.request.virtual_viewport.document_origin
                ));
            }
            let lines = rig
                .body
                .probe_root_leaf_lines()
                .map_err(|cause| cause.to_string())?;
            let mut parts = Vec::new();
            if name.starts_with("scoped_") {
                // Observe actual registered fonts from bounded short cases for face/system-metric
                // checks on line-layout failure,
                // without guessing size by semantic role or retaining fonts into independent TextKit
                // measurement.
                for part in rig
                    .body
                    .probe_root_leaf_parts()
                    .map_err(|cause| cause.to_string())?
                {
                    let font = part.font_owner.upgrade().ok_or("段落实际Font已归还")?;
                    eprintln!(
                        "native_paragraph_font_observation case={name} source={:?} presentation={:?} face={} points={} preview={:?} metrics={:?} sealed_advance_26_6={}",
                        part.source,
                        part.presentation,
                        font.native().source_face_postscript(),
                        font.native().point_size(),
                        font.preview_metrics(),
                        part.metrics,
                        part.advance_26_6,
                    );
                    if name.starts_with("scoped_quote") {
                        // Reader/Body borrows ended before probe return; print the same original font
                        // captured Core metadata
                        // and sealed glyph values only, without another Native font capture or reshape.
                        // Equal total advance cannot exclude equal-width wrong glyphs, so independent
                        // OpenType observations preserve actual clusters/IDs.
                        eprintln!(
                            "native_paragraph_glyph_observation case={name} source={:?} script={:?} direction={:?} font_metadata={:?} glyphs={:?}",
                            part.source,
                            part.script,
                            part.direction,
                            font.native().core_font().metadata(),
                            part.glyphs,
                        );
                        // Observe complete actual parts for this fixed quote fixture only; construct
                        // the independent reference after graph retirement.
                        // The probe bounds total parts; retain bounded pure values across close without
                        // extending font/publication lifetime.
                        parts.push(super::paragraph_oracle::PartAdvance {
                            source: part.source,
                            advance_26_6: part.advance_26_6,
                        });
                    }
                }
            }
            // Font/geometry consumers only observe Source; fixed-chunk canonical/disk checks prevent
            // correct rendering from hiding text mutation.
            let original_document = rig.document.upgrade().ok_or("段落原 Doc 已提前归还")?;
            let document = original_document.borrow();
            // Before interaction, this flow issues no edits, so generation, displayed prefixes, soft
            // lines, and CRLF must preserve Source.
            if document.generation() != 0 || document.utf16_len() != source.encode_utf16().count() {
                return Err(format!("{name}: 段落排版改变了原文档"));
            }
            let mut expected = source.encode_utf16();
            let mut units = [0_u16; 128];
            // Even short reference cases retain bounded production reads instead of a whole-document
            // copy API.
            for offset in (0..document.utf16_len()).step_by(units.len()) {
                let used = units.len().min(document.utf16_len() - offset);
                document
                    .copy_utf16_code_units(offset, &mut units[..used])
                    .map_err(|cause| cause.to_string())?;
                if units[..used]
                    .iter()
                    .any(|unit| expected.next() != Some(*unit))
                {
                    return Err(format!("{name}: 段落显示改写了 canonical 字符"));
                }
            }
            drop(document); // End actual document borrowing before file I/O and subsequent Native teardown.
            drop(original_document);
            // An unchanged buffer cannot exclude an accidental save; a separate descriptor checks each
            // byte chunk and actual EOF.
            unchanged_disk(&rig, name, source)?;
            if name.starts_with("scoped_quote") {
                // Freeze original comparison facts before actual mouse/edit/reflow work can change
                // expectations or overwrite differences.
                punctuation_interaction(&mut rig, name, source)?;
            }
            rig.remember_receipt()?;
            Ok(Candidate {
                lines,
                width,
                height: key.request.viewport.size.height,
                origin_y: key.request.virtual_viewport.document_origin.y,
                parts,
            })
        })();
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string())?;
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify()?;
    if publication.upgrade().is_some() {
        return Err(format!(
            "{name}: 短几何对照结束后原语义 publication 仍未释放"
        ));
    }
    let candidate = result?;
    let mut differences = Vec::new();
    // After strict candidate close, the independent TextKit graph determines actual soft lines and
    // returns every owner.
    let font = crate::native_surface::text_attributes::native_markdown_document_fixed_font(14.0)
        .map_err(|cause| cause.to_string())?;
    let original =
        super::paragraph_oracle::measure(source, candidate.width, &font, &candidate.parts)?;
    if !candidate.parts.is_empty() {
        // Actual TextKit lines already supplied glyph edges; sum their pure facts without CTLine
        // alternative CJK line-end widths.
        // Both paths use the public document font role, excluding UI-only font half-width defaults from
        // this product comparison.
        // Preserve actual per-part advances because equal line width cannot replace Source, glyph, and
        // local-geometry evidence,
        // or authorize changed glyphs/fixed pixel adjustments to fit platform results.
        let expected = super::paragraph_oracle::part_advances(source, &original, &candidate.parts)?;
        for (part, expected) in candidate.parts.iter().zip(expected) {
            eprintln!(
                "native_paragraph_advance_observation case={name} source={:?} actual_26_6={} textkit_26_6={expected} identical={} actual_policy=opentype_default reference_policy=coretext_document_default",
                part.source,
                part.advance_26_6,
                part.advance_26_6 == expected,
            );
        }
    }
    drop(font);
    if let Err(cause) = compare(name, &candidate, &original) {
        differences.push(cause);
    }
    if !differences.is_empty() {
        return Ok(Some(differences.join("\n"))); // After actual graph release, retain independent full-part and geometry differences together.
    }
    println!(
        "native_body_root_leaf_paragraph case={name} actual_old_native_geometry=true exact_soft_line_source=true same_font=true fixed_tolerance_26_6=1 actual_paged_frame=true formal=false whole_product=false"
    );
    Ok(None)
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    super::paragraph_oracle::font_defaults()?;
    let mut differences = Vec::new();
    for (name, source) in CASES {
        match run_case(mtm, name, source) {
            Ok(Some(cause)) => differences.push(cause),
            Ok(None) => {}
            Err(cause) => {
                // Stop on actual later assembly/interaction/resource failure while preserving prior
                // independent differences from strictly closed cases.
                differences.push(format!("{name}: {cause}"));
                return Err(differences.join("\n"));
            }
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences.join("\n"))
    }
}
