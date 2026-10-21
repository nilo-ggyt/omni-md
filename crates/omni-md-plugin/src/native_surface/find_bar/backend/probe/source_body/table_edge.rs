//! Return an unsealed giant row's original start caret through actual first-cell
//! geometry, without waiting for cell EOF or borrowing an old clip's visibility.

use super::*;
use omni_markdown_core::MarkdownPresentationMode;
use std::io::{Read, Write};

const HEADER: &str = "| H | C |\n| --- | --- |\n";
const ROW_PREFIX: &str = "| **edge** ";
const UNIT: &str = "alpha beta gamma delta ";
const REPEATS: usize = 8192;
const ROW_CLOSE: &str = " | side |\n\noutside paragraph\n";
const ROW_START: usize = HEADER.len();
const FIRST_VISIBLE: usize = ROW_START + 4; // The fixed literal's first e follows two actual hidden bold markers.
const REMOTE: usize = ROW_START + ROW_PREFIX.len() + UNIT.len() * 128;
const GIANT_TEXT_BYTES: usize = ROW_PREFIX.len() - 2 + UNIT.len() * REPEATS;
const UTF16: usize = HEADER.len() + ROW_PREFIX.len() + UNIT.len() * REPEATS + ROW_CLOSE.len();

/// Generate only this case's owned temporary file, in blocks of at most 2,944
/// bytes, without a giant-cell String or second document graph. Actual bold
/// markers exercise the hidden leading gap; a separate tail keeps complete CPU
/// frames distinct from document EOF.
fn fixture() -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
    file.write_all(HEADER.as_bytes())
        .and_then(|()| file.write_all(ROW_PREFIX.as_bytes()))
        .map_err(|cause| cause.to_string())?;
    let mut block = [0_u8; UNIT.len() * 128];
    for unit in block.as_chunks_mut::<{ UNIT.len() }>().0 {
        unit.copy_from_slice(UNIT.as_bytes());
    }
    for _ in 0..REPEATS / 128 {
        file.write_all(&block).map_err(|cause| cause.to_string())?;
    }
    file.write_all(ROW_CLOSE.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|cause| cause.to_string())?;
    Ok(file)
}

/// Fixed ASCII gives one-to-one byte/UTF-16 coordinates. Derive expectations from
/// generation rules, never by reading the display projection under test.
fn expected(index: usize) -> u8 {
    if index < HEADER.len() {
        return HEADER.as_bytes()[index];
    }
    let index = index - HEADER.len();
    if index < ROW_PREFIX.len() {
        return ROW_PREFIX.as_bytes()[index];
    }
    let index = index - ROW_PREFIX.len();
    if index < UNIT.len() * REPEATS {
        return UNIT.as_bytes()[index % UNIT.len()];
    }
    ROW_CLOSE.as_bytes()[index - UNIT.len() * REPEATS]
}

/// Read-only navigation preserves canonical text, history, and disk. Both actual
/// read paths compare fixed small chunks after frame acceptance; these integrity
/// reads do not measure the production layout frontier's source work.
fn unchanged(rig: &Rig) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("巨行边缘原Doc已释放")?;
    let document = document.borrow();
    if document.generation() != 0
        || document.utf16_len() != UTF16
        || document.byte_len() != UTF16
        || document.can_undo()
        || document.can_redo()
    {
        return Err("巨行边缘只读定位改写了原文稿或历史".into());
    }
    let mut units = [0_u16; 1024];
    for start in (0..UTF16).step_by(units.len()) {
        let used = units.len().min(UTF16 - start);
        document
            .copy_utf16_code_units(start, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        for (offset, unit) in units[..used].iter().enumerate() {
            if *unit != u16::from(expected(start + offset)) {
                return Err("巨行边缘导航改变了canonical原语法或独立尾段".into());
            }
        }
    }
    drop(document); // End the actual document borrow before any later Native/run-loop operation.
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 4095];
    for start in (0..UTF16).step_by(bytes.len()) {
        let used = bytes.len().min(UTF16 - start);
        file.read_exact(&mut bytes[..used])
            .map_err(|cause| cause.to_string())?;
        for (offset, byte) in bytes[..used].iter().enumerate() {
            if *byte != expected(start + offset) {
                return Err("巨行边缘导航改变了原临时文件".into());
            }
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("巨行边缘原文件尾部出现额外内容".into());
    }
    Ok(())
}

/// Require actual frontier/shaping work below half the fixed text and an incomplete
/// parent document. Comparing only with raw length could pass after all visible
/// text was shaped merely because hidden marker bytes remain. Projection itself
/// may stream the full raw cell; this proves a layout prefix, not that all raw
/// source reads stopped at the same boundary.
fn prefix(progress: &NativeMarkdownPagedBodyProgress) -> Result<(), String> {
    if progress.complete
        || progress.frontier_source_bytes >= UNIT.len() * REPEATS / 2
        || progress.shaped_input_bytes >= UNIT.len() * REPEATS / 2
    {
        return Err("巨行边缘定位以排到整格或文档EOF为代价".into());
    }
    Ok(())
}

/// The original SourceTarget scrolls without committing selection. Its original
/// three-second observer must receive a current complete CPU frame before proof
/// consumption; range, full Input, Source, and actual width retain original authority.
fn reveal(
    rig: &mut Rig,
    stage: &str,
    range: NSRange,
    original: &NativeMarkdownPagedSceneKey,
) -> Result<NativeMarkdownPagedSceneKey, String> {
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(range)
        .map_err(|cause| cause.to_string())?;
    let progress = wait_frame(stage, &rig.body, frame)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let proof = rig
        .body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("巨行边缘原SourceTarget缺少真实可见证明")?;
    if target.is_pending()
        || proof.range() != range
        || !proof.key().same(&key)
        || key.source != original.source
        || key.request.input != original.request.input
        || key.request.virtual_viewport.layout_width
            != original.request.virtual_viewport.layout_width
        || key.request.virtual_viewport.layout_origin_x
            != original.request.virtual_viewport.layout_origin_x
    {
        return Err("巨行边缘目标改写了原范围、完整Input或布局来源".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let first = scene.first_rect(range).map_err(|cause| cause.to_string())?;
    if first.actual_range != range || first.rect.size.height <= 0.0 {
        return Err("巨行边缘目标没有保持原canonical实际范围".into());
    }
    prefix(&progress)?;
    drop(scene); // A successor cannot borrow this reader's authority; retain only source-free key/resource observations.
    rig.remember_receipt()?;
    println!(
        "native_body_table_edge stage={stage} target_location={} target_length={} actual_cpu_frame=true original_source_input_width=true actual_prefix_only=true frontier_bytes={} shaped_input_bytes={} giant_text_bytes={GIANT_TEXT_BYTES} formal=false whole_product=false",
        range.location, range.length, progress.frontier_source_bytes, progress.shaped_input_bytes,
    );
    Ok(key)
}

/// Scroll a remote position of the same unsealed giant cell into the clip, then
/// return to its original physical row start. The old viewport cannot prove the
/// start visible. Its zero caret must coincide with the actual first e after
/// hidden bold markers; a nonempty pipe still has no character geometry.
fn exercise(rig: &mut Rig) -> Result<(), String> {
    let original = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    prefix(&rig.body.progress().map_err(|cause| cause.to_string())?)?;
    let remote = reveal(
        rig,
        "table_giant_edge_remote",
        NSRange::new(REMOTE, 1),
        &original,
    )?;
    if remote.request.virtual_viewport.document_origin.y
        <= original.request.virtual_viewport.document_origin.y
    {
        return Err("巨行边缘远端目标没有真正离开首部视口".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &remote.request)
        .map_err(|cause| cause.to_string())?;
    match scene.first_rect(NSRange::new(ROW_START, 0)) {
        Err(ViewError::Pending) => {}
        Err(cause) => return Err(cause.to_string()),
        Ok(_) => return Err("巨行原行首仍可见，夹具没有形成真实返回条件".into()),
    }
    drop(scene); // Return the remote reader before the same Body/cell Source prepares its row-start target.
    let returned = reveal(
        rig,
        "table_giant_edge_row_start",
        NSRange::new(ROW_START, 0),
        &original,
    )?;
    if returned.request.virtual_viewport.document_origin.y
        >= remote.request.virtual_viewport.document_origin.y
        || returned.same(&remote)
    {
        return Err("巨行原行首目标没有撤销旧clip并发布真实新视口".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &returned.request)
        .map_err(|cause| cause.to_string())?;
    let edge = scene
        .first_rect(NSRange::new(ROW_START, 0))
        .map_err(|cause| cause.to_string())?;
    let visible = scene
        .first_rect(NSRange::new(FIRST_VISIBLE, 0))
        .map_err(|cause| cause.to_string())?;
    if edge.actual_range != NSRange::new(ROW_START, 0)
        || visible.actual_range != NSRange::new(FIRST_VISIBLE, 0)
        || edge.rect != visible.rect
    {
        return Err("巨行前导语法零caret没有沿真实首格隐藏标记边缘投影".into());
    }
    match scene.first_rect(NSRange::new(ROW_START, 1)) {
        Err(ViewError::Pending) => {}
        Err(cause) => return Err(cause.to_string()),
        Ok(_) => return Err("巨行非空管道错误取得邻格字符几何".into()),
    }
    drop(scene);
    unchanged(rig)
}

/// Both success and ordinary errors end the original Rig and its unique Source/
/// Native resources before reporting results or creating another fixture.
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut rig = Rig::attach_mode(
            mtm,
            fixture()?,
            UTF16,
            6,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = exercise(&mut rig);
        let retirement = release::close(rig)?;
        Ok((retirement, result))
    })?;
    retirement.verify()?;
    result
}
