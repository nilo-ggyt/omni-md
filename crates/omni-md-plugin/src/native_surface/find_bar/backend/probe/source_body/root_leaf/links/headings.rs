//! Heading links within the actual long document must publish new visible CPU
//! pixels. Finding a heading or returning from its command does not prove navigation.

use super::*;

const HEAD: &str = "[first](#deep-title) [duplicate](#deep-title-2) [top](#)\n\n";
const PAD: &str =
    "original bounded source paragraph keeps unchanged bytes for heading navigation.\n\n";
const ATX: &str = "# Deep *Title*\n\n";
const COLLISION: &str = "# Deep Title-1\n\n";
const SETEXT: &str = "Deep **Title**\n===\n\n";
const PADDING: usize = 512;

/// Stream fragments much smaller than 4KiB into the synthetic file. No complete
/// expected String duplicates or extends the lifetime of the original mmap graph.
fn segments() -> impl Iterator<Item = &'static str> {
    std::iter::once(HEAD)
        .chain(std::iter::repeat_n(PAD, PADDING))
        .chain([ATX, COLLISION, SETEXT])
        .chain(std::iter::repeat_n(PAD, PADDING))
}

/// Expected bytes come from fixed independent literals. Selection may navigate;
/// document generation, history, and original disk contents must remain untouched.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("标题原Doc已释放")?;
    let document = owner.borrow();
    if document.generation() != 0
        || document.can_undo()
        || document.can_redo()
        || document.is_dirty()
        || !document.original_file_identity_is_unchanged()
    {
        return Err("标题导航改变了原正文、历史或磁盘身份".into());
    }
    let mut expected = segments().flat_map(str::bytes);
    let mut scratch = [0_u16; 256];
    for start in (0..document.utf16_len()).step_by(scratch.len()) {
        let count = scratch.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut scratch[..count])
            .map_err(|cause| cause.to_string())?;
        if scratch[..count]
            .iter()
            .any(|unit| expected.next().map(u16::from) != Some(*unit))
        {
            return Err("标题导航改写了canonical源字节".into());
        }
    }
    if expected.next().is_some() {
        return Err("标题导航截断了原canonical尾部".into());
    }
    drop(document);
    drop(owner); // Independent disk checks hold no document borrow, Native call, or run-loop scope.
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0; 256];
    for segment in segments() {
        file.read_exact(&mut bytes[..segment.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..segment.len()] != segment.as_bytes() {
            return Err("标题只读导航改变了原磁盘内容".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("标题磁盘夹具出现额外尾部".into());
    }
    Ok(())
}

/// Activate each label through actual Command mouse down/up and check its caret
/// and new CPU frame against independent fixture positions. The second target
/// combines Setext and an occupied -1 slug; duplicate counting alone is insufficient.
fn verify(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    if !rig.panel.makeFirstResponder(Some(&**rig.view)) || !rig.panel.isKeyWindow() {
        return Err("标题链接原窗口没有真实正文焦点".into());
    }
    let first = HEAD.len() + PAD.len() * PADDING;
    let second = first + ATX.len() + COLLISION.len();
    for (label, expected, stage) in [
        ("first", first, "root_heading_atx_visible"),
        ("duplicate", second, "root_heading_setext_unique_visible"),
        ("top", 0, "root_heading_empty_fragment_top"),
    ] {
        let index = HEAD.find(label).expect("固定链接标签存在");
        reveal(rig, "root_heading_link_label", index)?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let old = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let press = point(rig, index)?;
        let frame = observe(&rig.body)?;
        command_mouse(rig, press, NSEventType::LeftMouseDown)?;
        command_mouse(rig, press, NSEventType::LeftMouseUp)?;
        wait_frame(stage, &rig.body, frame)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let input = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if key.source != old.source
            || key.same(&old)
            || input.generation != before.generation
            || input.utf16_len != before.utf16_len
            || input.primary_range != NSRange::new(expected, 0)
            || input.marked.is_some()
            || input.selection_serial <= before.selection_serial
            || key.request.input != input
            || NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &old)
        {
            return Err("标题链接没有按同源新选择交付准确后继帧，或旧Scene仍有权限".into());
        }
        // Verify actual viewport/caret geometry, not merely successful heading
        // lookup or a returned selection-commit call.
        rig.view
            .verify_find_target_visible(&key, NSRange::new(expected, 0))
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let caret = scene
            .first_rect(NSRange::new(expected, 0))
            .map_err(|cause| cause.to_string())?;
        // Public empty-range geometry is an insertion line: caret geometry keeps
        // zero width and actual line height, adding paint thickness only during
        // drawing. Verify zero width here separately from range, visibility, and
        // same-ticket CPU-frame requirements.
        if caret.actual_range != NSRange::new(expected, 0)
            || !caret.rect.origin.x.is_finite()
            || !caret.rect.origin.y.is_finite()
            || !caret.rect.size.height.is_finite()
            || caret.rect.size.height <= 0.0
            || caret.rect.size.width != 0.0
        {
            return Err(format!(
                "标题终点没有原Source零caret的真实行几何：stage={stage} expected={expected} actual_range={:?} rect={:?}",
                caret.actual_range, caret.rect,
            ));
        }
        drop(scene);
        unchanged(rig)?;
        rig.remember_receipt()?;
    }
    println!(
        "native_root_heading_links command_click=true atx=true setext=true duplicate_collision=true empty_fragment_top=true original_source=true exact_caret_cpu_visible=true original_history_disk=true whole_product=false formal=false"
    );
    Ok(())
}

/// Reuse the original Rig and strict Retirement sequence. Even a navigation
/// failure must first release Native/Source/font/worker/mmap/pixel resources.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        let mut units = 0;
        let mut lines = 1;
        for segment in segments() {
            file.write_all(segment.as_bytes())
                .map_err(|cause| cause.to_string())?;
            units += segment.len(); // This all-ASCII fixture has equal independent literal byte and UTF-16 lengths.
            lines += segment.bytes().filter(|byte| *byte == b'\n').count();
        }
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            units,
            lines,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = verify(&mut rig);
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    result
}
