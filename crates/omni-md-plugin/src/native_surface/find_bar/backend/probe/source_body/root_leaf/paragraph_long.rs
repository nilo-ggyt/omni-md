//! Edit the same code paragraph across viewports in a four-MiB synthetic document; verify it in fixed
//! stack chunks without a full-document string.

use super::*;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSTextInputClient;
use objc2_foundation::NSString;
use std::io::Read;

const HEADER: &str = "outside\n\n```text\n";
const UNIT: &str = "alpha beta gamma delta ";
const REPEATS: usize = 96;
const LABEL: &str = "代码目标";
const CLOSE: &str = "\n```\n\n";
const TAIL: &str = "tail paragraph\n\n";
const TAIL_REPEATS: usize = (4 * 1024 * 1024) / TAIL.len();
const INSERTED: &str = "跨视口修复 alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
const TARGET: usize = HEADER.len() + UNIT.len() * REPEATS;

/// Write fixed segments to this owned temporary file without aggregating either body or tail into a
/// complete in-memory document.
fn fixture() -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
    file.write_all(HEADER.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..REPEATS {
        file.write_all(UNIT.as_bytes())
            .map_err(|cause| cause.to_string())?;
    }
    file.write_all(LABEL.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..REPEATS {
        file.write_all(UNIT.as_bytes())
            .map_err(|cause| cause.to_string())?;
    }
    file.write_all(CLOSE.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..TAIL_REPEATS {
        file.write_all(TAIL.as_bytes())
            .map_err(|cause| cause.to_string())?;
    }
    file.flush().map_err(|cause| cause.to_string())?;
    Ok(file)
}

/// Independent UTF-16 expectations follow the fixed construction above, without deriving expected
/// characters from edited output.
fn expected(index: usize, replacement: &[u16]) -> u16 {
    if index < HEADER.len() {
        return u16::from(HEADER.as_bytes()[index]);
    }
    if index < TARGET {
        return u16::from(UNIT.as_bytes()[(index - HEADER.len()) % UNIT.len()]);
    }
    let index = index - TARGET;
    if index < replacement.len() {
        return replacement[index];
    }
    let index = index - replacement.len();
    if index < UNIT.len() * REPEATS {
        return u16::from(UNIT.as_bytes()[index % UNIT.len()]);
    }
    let index = index - UNIT.len() * REPEATS;
    if index < CLOSE.len() {
        return u16::from(CLOSE.as_bytes()[index]);
    }
    u16::from(TAIL.as_bytes()[(index - CLOSE.len()) % TAIL.len()])
}

/// Check every canonical UTF-16 unit; independent byte/unit lengths also detect omitted tails or
/// changed UTF-8 encoding.
fn check(rig: &Rig, replacement: &str) -> Result<(), String> {
    let replacement_units: Vec<_> = replacement.encode_utf16().collect(); // Retain this bounded replacement input only.
    let utf16 = TARGET
        + replacement_units.len()
        + UNIT.len() * REPEATS
        + CLOSE.len()
        + TAIL.len() * TAIL_REPEATS;
    let bytes =
        TARGET + replacement.len() + UNIT.len() * REPEATS + CLOSE.len() + TAIL.len() * TAIL_REPEATS;
    let original = rig.document.upgrade().ok_or("跨视口原文稿已释放")?;
    let original = original.borrow();
    if original.byte_len() != bytes || original.utf16_len() != utf16 {
        return Err("跨视口编辑改变了预期范围之外的文稿长度".into());
    }
    let mut units = [0; 2048];
    for start in (0..utf16).step_by(units.len()) {
        let count = units.len().min(utf16 - start);
        original
            .copy_utf16_code_units(start, &mut units[..count])
            .map_err(|cause| cause.to_string())?;
        for (offset, actual) in units[..count].iter().enumerate() {
            if *actual != expected(start + offset, &replacement_units) {
                return Err(format!(
                    "跨视口 canonical 原文在{}发生了未授权变化",
                    start + offset
                ));
            }
        }
    }
    Ok(())
}

/// Independently verify the synthetic disk source remains unchanged through editing, undo, and redo
/// without an explicit save.
fn disk_unchanged(rig: &Rig) -> Result<(), String> {
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut scratch = [0; 64];
    let mut segment = |expected: &str| -> Result<(), String> {
        file.read_exact(&mut scratch[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &scratch[..expected.len()] != expected.as_bytes() {
            return Err("跨视口未保存编辑改变了原合成磁盘文件".into());
        }
        Ok(())
    };
    segment(HEADER)?;
    for _ in 0..REPEATS {
        segment(UNIT)?;
    }
    segment(LABEL)?;
    for _ in 0..REPEATS {
        segment(UNIT)?;
    }
    segment(CLOSE)?;
    for _ in 0..TAIL_REPEATS {
        segment(TAIL)?;
    }
    let mut tail = [0];
    if file.read(&mut tail).map_err(|cause| cause.to_string())? != 0 {
        return Err("跨视口原合成文件末尾出现额外字节".into());
    }
    Ok(())
}

/// Distant Find must deliver an actual CPU frame and target-visibility receipt; canonical selection
/// alone proves no viewport success.
fn find(rig: &mut Rig) -> Result<(), String> {
    rig.bar.query.setStringValue(&NSString::from_str(LABEL));
    let frame = observe(&rig.body)?;
    let pending = match rig
        .action(MarkdownFindAction::Next)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownFindExecution::Complete => None,
        NativeMarkdownFindExecution::Pending(request) => {
            rig.requests.push(request.clone());
            Some(request)
        }
    };
    frame::wait_frame("root_leaf_code_cross_viewport_find", &rig.body, frame)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.primary_range != NSRange::new(TARGET, 4) {
        return Err("原代码段的远端 Find 没有准确落到固定中文目标".into());
    }
    if let Some(request) = pending
        && (request.range() != NSRange::new(TARGET, 4)
            || request.pending()
            || !request.completed_visible()
            || request.cancelled()
            || request.take_failure().is_some()
            || key.request.input != request.input_state())
    {
        return Err("跨视口Find没有交付同一个原请求的实际可见完成收据".into());
    }
    rig.view
        .verify_find_target_visible(&key, NSRange::new(TARGET, 4))
        .map_err(|cause| cause.to_string())?;
    rig.remember_receipt()
}

/// Edit the cross-viewport paragraph through the actual Native input API, then undo/redo through real
/// commands without direct document mutation.
fn exercise(rig: &mut Rig) -> Result<(), String> {
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let initial_lines = rig
        .body
        .probe_root_leaf_lines()
        .map_err(|cause| cause.to_string())?;
    if initial_lines
        .iter()
        .any(|line| line.source.contains(&TARGET))
    {
        return Err("跨视口目标在首帧已可见，固定长段落夹具没有形成真实离屏条件".into());
    }
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.complete
        || progress.semantic_work.source_bytes >= progress.source_total_bytes / 100
        || progress.frontier_source_bytes >= progress.source_total_bytes / 100
    {
        return Err("长文首帧扫描了不需要的远端正文，或没有保留真正分页前缀".into());
    }
    find(rig)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
    // SAFETY: Use the actual main-thread client, bounded replacement, and current target range;
    // original Input still validates and commits.
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(INSERTED), NSRange::new(TARGET, 4))
    };
    frame::wait_frame("root_leaf_code_cross_viewport_edit", &rig.body, frame)?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .generation
        != before.generation + 1
    {
        return Err("跨视口原生输入没有只提交一个 canonical 代次".into());
    }
    check(rig, INSERTED)?;
    rig.remember_receipt()?;
    // Every history transition must produce its own actual frame and independently match the complete
    // canonical stream.
    for (selector, replacement, stage) in [
        (sel!(undo:), LABEL, "root_leaf_code_cross_viewport_undo"),
        (sel!(redo:), INSERTED, "root_leaf_code_cross_viewport_redo"),
        (sel!(undo:), LABEL, "root_leaf_code_cross_viewport_restore"),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(selector)
            .map_err(|cause| cause.to_string())?;
        frame::wait_frame(stage, &rig.body, frame)?;
        check(rig, replacement)?;
        rig.remember_receipt()?;
    }
    disk_unchanged(rig)?;
    let final_key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if final_key.source == initial.source {
        return Err("原文经过实际编辑仍复用了旧 Source 排版权限".into());
    }
    println!(
        "native_body_root_leaf_paragraph_long original_4mib=true same_code_paragraph_cross_viewport=true actual_find_visible=true actual_native_edit=true exact_full_canonical=true undo_redo=true disk_unchanged=true original_three_second_frame_gate=true formal=false whole_product=false"
    );
    Ok(())
}

/// The owned long document lives with this Rig only; original close receipts still verify actual
/// font/file/mapping/pixel release.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let file = fixture()?;
        let utf16 = TARGET + 4 + UNIT.len() * REPEATS + CLOSE.len() + TAIL.len() * TAIL_REPEATS;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            utf16,
            7 + 2 * TAIL_REPEATS,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = exercise(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify()?;
    if publication?.upgrade().is_some() {
        return Err("跨视口长文退出后原有限语义仍被强持有".into());
    }
    result
}
