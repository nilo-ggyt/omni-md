//! Drive paged highlights through the actual Find field. Read-only query changes
//! leave canonical text untouched and synchronously revoke the previous Scene.

use super::*;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSTextInputClient, NSTextView};
use std::io::Read;

const SOURCE: &str = "outside\n\n前 目标 后 目标\n\n末尾 目标\n\n隐藏 **加粗** 尾\n";
const GRID_SOURCE: &str = concat!(
    "outside\n\n",
    "| 字段 | 内容 |\n| --- | --- |\n",
    "| **加粗** | [目标](https://example.invalid/hidden) 与 `目标` 和 escaped\\|pipe |\n",
    "\nend\n",
);

/// Commit one actual short input through the original session field editor's
/// NSTextInputClient, without rewriting the query control or fabricating notifications.
fn query(rig: &Rig, value: &str) -> Result<(), String> {
    let intent = rig.controller.find_input_intent();
    // SAFETY: The original bar/window remain on their main thread. Let the actual
    // field perform focus without replacing its delegate or input method.
    unsafe { rig.bar.query.selectText(None) };
    let editor = rig
        .bar
        .query
        .currentEditor()
        .ok_or("实时查找没有原字段编辑器")?
        .downcast::<NSTextView>()
        .map_err(|_| "实时查找字段不是NSTextView")?;
    if editor.hasMarkedText() {
        return Err("实时查找控制不能替用户确认候选".into());
    }
    let range = NSRange::new(0, editor.string().length());
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    // SAFETY: Replace only the actual field's bounded query; supply no document
    // text and change no document selection.
    unsafe { client.insertText_replacementRange(&NSString::from_str(value), range) };
    if rig.controller.find_input_intent() != intent.checked_add(1).ok_or("查找意图溢出")?
        || !rig
            .bar
            .query
            .stringValue()
            .isEqualToString(&NSString::from_str(value))
    {
        return Err("实时查询未由真实输入产生一次原反馈意图".into());
    }
    Ok(())
}

/// Paint the current Scene's actual bounded protocol. Observe the real Find-color
/// blend on a white background rather than inferring painted pixels from match records.
fn rgba(rig: &Rig) -> Result<Vec<u8>, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    let canvas = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    selection::paint_scene(scene.clone(), &key, &canvas, &current)?;
    CGContext::flush(Some(&canvas.context));
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    let selection::Canvas { context, pixels } = canvas;
    drop(context); // End the context's pixel borrow before independently comparing the actual bounded RGBA buffer.
    Ok(pixels)
}

/// Recognize Find yellow independently for white document backgrounds. Dark table
/// backgrounds instead compare complete bounded RGBA with the original empty query.
fn pixels(rig: &Rig) -> Result<usize, String> {
    let count = rgba(rig)?
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| {
            // White, black text, and ordinary blue selection do not meet this
            // range; actual translucent yellow must reach the original document canvas.
            pixel[0] > 240
                && (175..250).contains(&pixel[1])
                && (100..225).contains(&pixel[2])
                && pixel[3] == 255
        })
        .count();
    Ok(count)
}

/// Exercise actual fields in both presentation modes of the same short source.
/// Each mode is a real construction value, never a test-time private-state mutation.
fn exercise(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("实时查找控制未取得原keyWindow".into());
    }
    let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let old = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let old_scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &old.request)
        .map_err(|cause| cause.to_string())?;
    let event = observe(&rig.body)?;
    query(rig, "目标")?;
    if !matches!(old_scene.validate_ready(&|| true), Err(ViewError::Revoked)) {
        return Err("实际查询输入未同步撤销旧Scene与CPU候选权限".into());
    }
    drop(old_scene); // Return the reader after proving revocation, before the same producer publishes a new window.
    frame::wait_complete_frame("root_leaf_find_live_matches", &rig.body, event)?;
    let found = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if found.same(&old)
        || found.source != old.source
        || found.request.input != original
        || pixels(rig)? == 0
    {
        return Err("实时查找未复用同Source发布新nonce和实际高亮，或改写了正文选择".into());
    }
    if !rig
        .bar
        .status
        .stringValue()
        .isEqualToString(ns_string!("当前可见区域找到 3 处匹配"))
    {
        return Err("实时查找反馈没有来自同次完整可见范围的三处匹配".into());
    }
    rig.remember_receipt()?;
    let event = observe(&rig.body)?;
    query(rig, "不存在")?;
    // Successor input arrives without waiting for the old frame. Only the latest
    // committed query may publish; an earlier match cannot revive after setter reentry.
    query(rig, "目标")?;
    query(rig, "仍不存在")?;
    frame::wait_complete_frame("root_leaf_find_live_successor", &rig.body, event)?;
    if pixels(rig)? != 0 || rig.view.input_state().map_err(|cause| cause.to_string())? != original {
        return Err("后继实时输入仍显示旧高亮或改变原正文".into());
    }
    let event = observe(&rig.body)?;
    query(rig, "目标")?;
    frame::wait_complete_frame("root_leaf_find_live_again", &rig.body, event)?;
    if pixels(rig)? == 0 {
        return Err("同一原栏再次输入没有恢复真实高亮".into());
    }
    // Preview uses only actual visible part domains. Hidden syntax contributes
    // neither counts nor placeholder highlight bars; Source still searches literal syntax.
    let event = observe(&rig.body)?;
    query(rig, "**")?;
    frame::wait_complete_frame("root_leaf_find_live_markers", &rig.body, event)?;
    let marker_pixels = pixels(rig)?;
    let marker_status = if old.presentation_mode == MarkdownPresentationMode::Source {
        if marker_pixels == 0 {
            return Err("Source字面Markdown语法没有真实Find像素".into());
        }
        "当前可见区域找到 2 处匹配"
    } else {
        if marker_pixels != 0 {
            return Err("Preview为隐藏Markdown语法画了占位高亮".into());
        }
        "当前可见区域找到 0 处匹配"
    };
    if !rig
        .bar
        .status
        .stringValue()
        .isEqualToString(&NSString::from_str(marker_status))
    {
        return Err("Find反馈计数与当前模式的真实可见源域不一致".into());
    }
    let event = observe(&rig.body)?;
    query(rig, "加粗")?;
    frame::wait_complete_frame("root_leaf_find_live_visible_bold", &rig.body, event)?;
    if pixels(rig)? == 0 {
        return Err("可见粗体正文被隐藏语法过滤错误地一同移除了".into());
    }
    let event = observe(&rig.body)?;
    // Hide belongs to the original layout action. The matching backend explicitly
    // rejects Hide; that rejection cannot be mistaken for a product hide failure.
    let intent = rig.controller.advance_find_input_intent();
    rig.bar.viewport_feedback_intent.set(None);
    rig.body
        .finish_cell_before_find(&rig.controller, intent)
        .map_err(|cause| cause.to_string())?;
    if !rig
        .bar
        .hide(&rig.scroll, rig.view.as_super(), &rig.controller)?
        || rig.bar.is_visible()
    {
        return Err("实时查找原Hide动作未完成".into());
    }
    frame::wait_complete_frame("root_leaf_find_live_hide", &rig.body, event)?;
    if pixels(rig)? != 0 {
        return Err("原查找栏隐藏后仍保留黄色像素".into());
    }
    unchanged(rig, SOURCE)?;
    rig.remember_receipt()?;
    println!(
        "native_find_live_highlights actual_field_input=true original_source=true same_canonical_selection=true actual_yellow_pixels=true successor_cancels_old_scene=true hide_clears_pixels=true exact_canonical_and_disk=true original_three_second_frame_gate=true formal=false whole_product=false"
    );
    Ok(())
}

/// Independently compare canonical/disk contents of either fixed short fixture in
/// bounded chunks. Find acquired no authority to write document text.
fn unchanged(rig: &Rig, source: &str) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("实时查找原Doc已释放")?;
    let document = document.borrow();
    if document.generation() != 0 || document.utf16_len() != source.encode_utf16().count() {
        return Err("实时查找改变canonical世代或长度".into());
    }
    // Read fixed windows against the independent literal iterator, then release
    // the canonical borrow before accessing its original on-disk file.
    let mut units = [0_u16; 32];
    let mut expected = source.encode_utf16();
    for start in (0..document.utf16_len()).step_by(units.len()) {
        let count = units.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut units[..count])
            .map_err(|cause| cause.to_string())?;
        if units[..count]
            .iter()
            .any(|unit| expected.next() != Some(*unit))
        {
            return Err("实时查找改写原canonical字符".into());
        }
    }
    drop(document);
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 32];
    for expected in source.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..expected.len()] != expected {
            return Err("实时查找改变了原磁盘".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("实时查找原磁盘出现额外尾部".into());
    }
    Ok(())
}

/// The canonical envelope cannot restore syntax removed by the actual table
/// display projection. Both counts and actual RGBA must exclude hidden syntax.
fn exercise_grid(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("表格实时查找没有实际keyWindow".into());
    }
    let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let baseline = rgba(rig)?;
    for (search, expected) in [
        ("目标", 2),
        ("**", 0),
        ("hidden", 0),
        ("---", 0),
        ("escaped\\|pipe", 0),
        ("pipe", 1),
        ("加粗", 1),
    ] {
        let event = observe(&rig.body)?;
        query(rig, search)?;
        frame::wait_complete_frame("root_leaf_find_grid_visible_source", &rig.body, event)?;
        let actual = rgba(rig)?;
        if actual.len() != baseline.len()
            || (actual != baseline) != (expected != 0)
            || rig.view.input_state().map_err(|cause| cause.to_string())? != original
            || !rig
                .bar
                .status
                .stringValue()
                .isEqualToString(&NSString::from_str(&format!(
                    "当前可见区域找到 {expected} 处匹配"
                )))
        {
            return Err(format!(
                "表格可见raw片段的实际计数/像素不一致：expected_count={expected}"
            ));
        }
        rig.remember_receipt()?;
    }
    unchanged(rig, GRID_SOURCE)?;
    println!(
        "native_find_grid actual_field_input=true original_sealed_projection=true visible_bold_code_links=true hidden_markers_urls_separators_escape_not_counted_or_painted=true exact_rgba_against_original_empty_query=true source_and_disk_unchanged=true formal=false whole_product=false"
    );
    Ok(())
}

/// Construct modes sequentially and close each strictly. The original release
/// sequence owns fields, Body, fonts, readers, pixels, and the synthetic file.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    for (mode, source, grid) in [
        (MarkdownPresentationMode::Source, SOURCE, false),
        (MarkdownPresentationMode::InlinePreview, SOURCE, false),
        (MarkdownPresentationMode::InlinePreview, GRID_SOURCE, true),
    ] {
        let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
            let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
            file.write_all(source.as_bytes())
                .map_err(|cause| cause.to_string())?;
            file.flush().map_err(|cause| cause.to_string())?;
            let mut rig = Rig::attach_mode(
                mtm,
                file,
                source.encode_utf16().count(),
                source.lines().count() + 1,
                false,
                mode,
                true,
            )?;
            let result = (|| {
                let bound = observe(&rig.body)?;
                rig.bar.bind_body(Rc::downgrade(&rig.body), &rig.controller);
                frame::wait_complete_frame("root_leaf_find_live_bind", &rig.body, bound)?;
                if grid {
                    exercise_grid(&mut rig)
                } else {
                    exercise(&mut rig)
                }
            })();
            let retirement = release::close(rig)?;
            Ok((retirement, result))
        })?;
        retirement.verify()?;
        result?;
    }
    Ok(())
}
