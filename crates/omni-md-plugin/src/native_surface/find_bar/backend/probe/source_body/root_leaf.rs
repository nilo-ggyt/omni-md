//! Observe actual RootLeaf font frames from the original Source. Original
//! selection determines marker sizing; this alone does not prove all Markdown decoration.
use std::io::Write;

use super::*;
use objc2_core_graphics::CGContext;
use omni_markdown_core::{MarkdownPresentationMode, MarkdownPresentationStyles};

const TEXT: &str = "# Title\n\n```text\nlet value = 1;\n```\n\n## Next\n";

#[path = "root_leaf/find_highlights.rs"]
mod find_highlights;
#[path = "root_leaf/inline.rs"]
mod inline;
#[path = "root_leaf/links.rs"]
mod links;
#[path = "root_leaf/paragraph_cases.rs"]
mod paragraph_cases;
#[path = "root_leaf/paragraph_long.rs"]
mod paragraph_long;
#[path = "root_leaf/paragraph_oracle.rs"]
mod paragraph_oracle;
#[path = "root_leaf/table.rs"]
mod table;
#[path = "root_leaf/word.rs"]
mod word;

/// Compare actual sealed font coverage and Native geometry with independent
/// fixture coordinates, then navigate selection to reveal editable heading markers.
fn run_case(rig: &mut Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    // Expected positions come from fixed source literals, never inferred from the
    // producer's heading/code ranges under test.
    for (source, points, role) in [
        (2..7, 28.0, "heading"),
        (17..31, 14.0, "code"),
        (40..44, 24.0, "heading"),
    ] {
        let mut cursor = source.start;
        let mut relevant: Vec<_> = parts
            .iter()
            .filter(|part| part.source.start < source.end && source.start < part.source.end)
            .collect();
        relevant.sort_by_key(|part| part.source.start);
        for part in relevant {
            if part.source.start > cursor
                || part.points != points
                || part.native_points != f64::from(points)
                || part.role != role
            {
                return Err(format!(
                    "RootLeaf真实part字体/字号或源覆盖不符：source={:?}, part={:?}, points={}, native={}, role={}",
                    source, part.source, part.points, part.native_points, part.role
                ));
            }
            cursor = cursor.max(part.source.end.min(source.end));
        }
        if cursor != source.end {
            return Err("RootLeaf内容没有完整进入原sealed字形覆盖".into());
        }
    }
    if rig.copy::<8>(0)?
        != [
            '#' as u16,
            ' ' as u16,
            'T' as u16,
            'i' as u16,
            't' as u16,
            'l' as u16,
            'e' as u16,
            '\n' as u16,
        ]
    {
        return Err("语义字体接入改写或移除了原ATX语法".into());
    }
    let marker = parts
        .iter()
        .find(|part| part.source.contains(&0))
        .ok_or("原ATX marker没有真实字形part")?;
    if marker.points < 14.0 || marker.native_points != f64::from(marker.points) {
        return Err("原选择所在标题的marker没有按原语义保持可见字号".into());
    }
    // Initial selection lies in the first heading. Only the second heading's
    // actual semantics permit hiding; inspect sealed parts rather than palette
    // declarations to prove shaping really used the small marker font.
    for offset in 37..40 {
        let hidden = parts
            .iter()
            .find(|part| part.source.contains(&offset))
            .ok_or("未选标题marker没有真实字形part")?;
        if hidden.role != "hidden"
            || hidden.points != 0.1_f32
            || hidden.native_points != 0.1
            || !hidden
                .presentation
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        {
            return Err("未选标题marker丢失真实隐藏字体或字号被normal/heading覆盖".into());
        }
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let heading = scene
        .first_rect(NSRange::new(2, 1))
        .map_err(|cause| cause.to_string())?;
    let code = scene
        .first_rect(NSRange::new(17, 1))
        .map_err(|cause| cause.to_string())?;
    let hidden_start = scene
        .first_rect(NSRange::new(37, 0))
        .map_err(|cause| cause.to_string())?;
    let next_start = scene
        .first_rect(NSRange::new(40, 0))
        .map_err(|cause| cause.to_string())?;
    let marker_width = next_start.rect.origin.x - hidden_start.rect.origin.x;
    if heading.actual_range != NSRange::new(2, 1)
        || code.actual_range != NSRange::new(17, 1)
        || heading.rect.size.height <= code.rect.size.height
        || heading.rect.size.width <= 0.0
        || code.rect.size.width <= 0.0
        || !marker_width.is_finite()
        || !(0.0..1.0).contains(&marker_width)
        || !current()
    {
        return Err("真实标题/代码框或隐藏marker宽度没有保留Source坐标与实际字体几何".into());
    }
    drop(scene); // Return the previous Scene reader before Find preparation so no old prefix snapshot occupies its slot.
    rig.bar.query.setStringValue(ns_string!("Title"));
    let range = NSRange::new(2, 5);
    let frame = observe(&rig.body)?;
    // Title is already in the viewport, so execution may finish synchronously;
    // a remote fixture's required-Pending premise does not apply. If preparation
    // is asynchronous, retain that same request and wait for the original Body's
    // endpoint rather than treating selection commit as visibility.
    let pending = match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => None,
        NativeMarkdownFindExecution::Pending(request) => {
            if !request.pending() || request.completed_visible() || !request.is_current() {
                return Err("RootLeaf Find没有保留原在途请求".into());
            }
            rig.requests.push(request.clone());
            Some(request)
        }
    };
    frame::wait_complete_frame("root_leaf_title_find", &rig.body, frame)?;
    let found = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if let Some(request) = &pending
        && (request.range() != range
            || !request.completed_visible()
            || request.pending()
            || request.cancelled()
            || request.take_failure().is_some()
            || found.request.input != request.input_state())
    {
        return Err("RootLeaf异步Find没有按原请求完成真实可见性".into());
    }
    if found.request.input.primary_range != range
        || found.source != key.source
        || rig.copy::<5>(2)? != [84, 105, 116, 108, 101]
    {
        return Err("RootLeaf Find选区没有沿同原Source和canonical文字".into());
    }
    rig.view
        .verify_find_target_visible(&found, range)
        .map_err(|cause| cause.to_string())?;
    // Find changes canonical selection to the second heading. Its successor must
    // derive semantics from the same source and restore heading-sized markers,
    // rather than preserving a stale hidden slot over editable syntax.
    rig.bar.query.setStringValue(ns_string!("Next"));
    let frame = observe(&rig.body)?;
    match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => {}
        NativeMarkdownFindExecution::Pending(request) => rig.requests.push(request),
    }
    frame::wait_complete_frame("root_leaf_marker_reveal", &rig.body, frame)?;
    let revealed = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if revealed.request.input.primary_range != NSRange::new(40, 4)
        || revealed.source != key.source
        || rig.copy::<3>(37)? != [35, 35, 32]
    {
        return Err("Find显露marker时改变了原正文或选择坐标".into());
    }
    let revealed_parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    for offset in 37..40 {
        let marker = revealed_parts
            .iter()
            .find(|part| part.source.contains(&offset))
            .ok_or("选择后标题marker没有进入真实sealed part")?;
        if marker.role != "heading"
            || marker.points != 24.0
            || marker.native_points != 24.0
            || marker
                .presentation
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        {
            return Err(format!(
                "原选择变化后marker仍复用旧隐藏字体：offset={offset}, source={:?}, points={}, native_points={}, role={}",
                marker.source, marker.points, marker.native_points, marker.role
            )); // Keep actual sealed-part facts; selection/frame completion cannot establish updated styles.
        }
    }
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.semantic_work.source_bytes == 0
        || progress.semantic_work.copied_source_bytes != 0
        || progress.semantic_work.parser_input_bytes != 0
    {
        return Err("RootLeaf没有真实收费或错误回到局部Parser/String路径".into());
    }
    rig.remember_receipt()?; // Include actual added heading/code font captures for strict close-time release verification.
    println!(
        "native_body_root_leaf actual_frame=true heading_points=28,24 code_points=14 exact_source_ranges=true selection_bound_marker_points=0.1,24 canonical_markers_preserved=true original_find_source=true semantic_work={:?} formal=false whole_product=false",
        progress.semantic_work
    );
    Ok(())
}

/// Verify canonical tags, sealed fonts, and actual text geometry in the same
/// complete frame without manufacturing parts from parser expectations.
fn verify_literal_code(rig: &mut Rig, source: &str) -> Result<(), String> {
    let literal = "<u>中文😀</u>";
    let start = source[..source.find(literal).expect("固定代码字面样本")]
        .encode_utf16()
        .count();
    if rig.copy::<11>(start)? != [60, 117, 62, 0x4e2d, 0x6587, 0xd83d, 0xde00, 60, 47, 117, 62] {
        return Err("真实代码字面丢失原标签、中文或emoji UTF16".into());
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.primary_range != NSRange::new(0, 0)
        || rig.view.cpu_paint_pending() != Some(false)
    {
        return Err("代码字面验收未保持代码外初始caret和真实完整帧".into());
    }
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    for position in start..start + 11 {
        let part = parts
            .iter()
            .find(|part| part.source.contains(&position))
            .ok_or("真实代码字面没有完整进入sealed glyph输出")?;
        if !part
            .presentation
            .contains(MarkdownPresentationStyles::FENCED_CODE)
            || part
                .presentation
                .contains(MarkdownPresentationStyles::UNDERLINE)
            || part
                .presentation
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || part.points != 14.0
            || part.native_points != 14.0
        {
            return Err(format!(
                "代码字面被错误解释或隐藏：source={:?}, actual_points={}, native_points={}, styles={:?}",
                part.source, part.points, part.native_points, part.presentation
            ));
        }
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let actual = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    let mask = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    let steps = selection::paint_scene(scene.clone(), &key, &actual, &current)?;
    let mut position = start;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    for scalar in literal.chars() {
        let range = NSRange::new(position, scalar.len_utf16());
        let first = scene.first_rect(range).map_err(|cause| cause.to_string())?;
        if first.actual_range != range
            || first.rect.size.width <= 0.0
            || first.rect.size.height <= 1.0
        {
            return Err("代码标签或真实Unicode GCB仍使用隐藏字号/零宽几何".into());
        }
        if position < start + 3 {
            CGContext::fill_rect(Some(&mask.context), first.rect);
        } // Mask only the actual three <u> characters; Chinese/emoji ink cannot certify hidden literal tags.
        position += scalar.len_utf16();
    }
    CGContext::flush(Some(&actual.context));
    CGContext::flush(Some(&mask.context));
    let ink = actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(pixel, marker)| {
            // Count only actual black glyph ink. The code background's lowest
            // channel is about 33, so a dark fill cannot certify literal-tag glyphs.
            **marker == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    if ink == 0 {
        return Err("代码中的原<u>标签没有真实像素墨迹".into());
    }
    drop(scene); // Return reader/font/Source claims before close; bitmap/mask remain local to this bounded control.
    rig.remember_receipt()?;
    println!(
        "native_body_root_leaf_code_literal actual_marker_ink={ink} bounded_paint_steps={steps} original_utf16=true actual_code_points=14 actual_tag_rects=true no_underline_or_hidden=true original_complete_frame=true formal=false whole_product=false"
    );
    Ok(())
}

/// Assemble and close each actual code-block variant separately. Return the prior
/// fonts, readers, and pixels before constructing its successor.
fn literal_code_case(mtm: MainThreadMarker, source: &str) -> Result<(), String> {
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
        let result = verify_literal_code(&mut rig, source);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result)) // Failed assertions still dismantle the actual synthetic window/font owners first.
    })?;
    retirement.verify()?;
    if publication?.upgrade().is_some() {
        return Err("代码字面关闭后原RootLeaf Ready仍被业务持有".into());
    }
    result
}

/// Formatting immediately after actual typing must accept the same current
/// selection transaction even while its pixels are still rebuilding.
fn format_after_input(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(b"T\n").map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            2,
            2,
            false,
            MarkdownPresentationMode::Source,
            true,
        )?;
        let result = (|| {
            let frame = observe(&rig.body)?;
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
            // Commit Chinese text through the actual input callback, then format
            // without running the loop or waiting for a rebuilt Scene.
            // SAFETY: The complete short NSString and unspecified replacement range
            // use the original NSTextInputClient entry to consume its current caret.
            unsafe {
                client.insertText_replacementRange(
                    &NSString::from_str("中"),
                    NSRange::new(NSNotFound as usize, 0),
                );
            }
            rig.view
                .apply_format(omni_markdown_core::MarkdownFormatAction::Heading1)
                .map_err(|cause| cause.to_string())?;
            frame::wait_complete_frame("format_immediately_after_input", &rig.body, frame)?;
            // Only this independent short fixture permits a complete oracle;
            // production formatting/save still consume the original piece root.
            let assert_text = |expected: &[u8]| -> Result<(), String> {
                let document = rig.document.upgrade().ok_or("格式验证的原文稿已经释放")?;
                let mut actual = Vec::new();
                document
                    .borrow()
                    .write_to(&mut actual)
                    .map_err(|cause| cause.to_string())?;
                if actual != expected {
                    return Err("连续键入与格式没有保留预期正文或逐笔历史".into());
                }
                Ok(())
            };
            assert_text("# 中T\n".as_bytes())?;
            let window = rig.view.window().ok_or("右键编辑菜单的原窗口已归还")?;
            let menu_event = objc2_app_kit::NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                objc2_app_kit::NSEventType::RightMouseDown,
                rig.view.convertPoint_toView(NSPoint::new(12.0, 12.0), None),
                objc2_app_kit::NSEventModifierFlags::empty(),
                1.0,
                window.windowNumber(),
                None,
                1,
                1,
                1.0,
            ).ok_or("无法构造原窗口的右键菜单事件")?;
            drop(window); // The event retains only the window number; a local claim must not extend window lifetime.
            // Formatting and character insertion are separate user actions. Actual
            // Undo restores each individually without merging or swallowing a step.
            for (index, expected) in ["中T\n", "T\n"].into_iter().enumerate() {
                let frame = observe(&rig.body)?;
                let menu = rig
                    .view
                    .menuForEvent(&menu_event)
                    .ok_or("分页正文没有原生右键编辑菜单")?;
                let undo = menu.itemAtIndex(0).ok_or("右键菜单没有撤销动作")?;
                if undo.action() != Some(sel!(undo:))
                    || !undo.isEnabled()
                    || !undo.keyEquivalent().is_empty()
                {
                    return Err("右键撤销未绑定原历史，或擅自增加了快捷键".into());
                }
                menu.performActionForItemAtIndex(0); // Actual NSMenu delivers the original action to this client.
                drop((undo, menu)); // Return temporary menu claims; the owner retains only its one current menu.
                frame::wait_complete_frame(
                    if index == 0 {
                        "format_immediate_undo_format"
                    } else {
                        "format_immediate_undo_input"
                    },
                    &rig.body,
                    frame,
                )?;
                assert_text(expected.as_bytes())?;
            }
            if std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())? != b"T\n" {
                return Err("格式与撤销流程意外覆盖了原磁盘文件".into());
            }
            // Actual owners retain save/release responsibility; this test does
            // not mutate private selection, Scene, or undo-stack state.
            println!(
                "native_root_format immediate_after_input=true actual_text_input_client=true native_context_menu_undo=true no_added_shortcuts=true two_real_undo=true disk_unchanged=true whole_product=false formal=false"
            );
            rig.remember_receipt()
        })();
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?; // Even failure returns actual Native/font/reader/mapping resources before reporting.
    result
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(TEXT.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            TEXT.encode_utf16().count(),
            TEXT.bytes().filter(|&byte| byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = run_case(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string())?;
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify()?;
    if publication.upgrade().is_some() {
        return Err("RootLeaf关闭后原有限Ready仍被业务强持有".into());
    }
    result?;
    literal_code_case(mtm, "outside\n\n```text\n<u>中文😀</u>\n```\n\n")?;
    literal_code_case(mtm, "outside\n\n    <u>中文😀</u>\n\n")?;
    // Each independent scenario verifies complete closure before returning. Run
    // the original cases sequentially and retain their separate failures so one
    // actual run can expose edit, glyph, and link defects. Never replay a failed
    // action, shrink fixtures, or relax deadlines, pixel checks, or resource assertions.
    let mut failures = Vec::new();
    for (name, result) in [
        ("table", table::run(mtm)),
        ("word", word::run(mtm)),
        ("find_highlights", find_highlights::run(mtm)),
        ("paragraph", paragraph_cases::run(mtm)),
        ("long_paragraph", paragraph_long::run(mtm)),
        ("inline", inline::run(mtm)),
        ("format", format_after_input(mtm)),
        ("links", links::run(mtm)),
    ] {
        if let Err(cause) = result {
            eprintln!("native_root_leaf_case role={name} outcome=failed reason={cause}");
            failures.push(format!("{name}: {cause}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
