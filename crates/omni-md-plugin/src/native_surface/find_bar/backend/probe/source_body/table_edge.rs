//! 未封存巨行的原行首零caret必须沿真实首格字形返回，不能等待整格EOF或借旧clip签可见。

use super::*;
use omni_markdown_core::MarkdownPresentationMode;
use std::io::{Read, Write};

const HEADER: &str = "| H | C |\n| --- | --- |\n";
const ROW_PREFIX: &str = "| **edge** ";
const UNIT: &str = "alpha beta gamma delta ";
const REPEATS: usize = 8192;
const ROW_CLOSE: &str = " | side |\n\noutside paragraph\n";
const ROW_START: usize = HEADER.len();
const FIRST_VISIBLE: usize = ROW_START + 4; // 固定字面的首个e；前面的两颗星号确实是隐藏语法。
const REMOTE: usize = ROW_START + ROW_PREFIX.len() + UNIT.len() * 128;
const GIANT_TEXT_BYTES: usize = ROW_PREFIX.len() - 2 + UNIT.len() * REPEATS;
const UTF16: usize = HEADER.len() + ROW_PREFIX.len() + UNIT.len() * REPEATS + ROW_CLOSE.len();

/// 只生成本用例拥有的临时文件；最大写块2944B，既不构造巨格String，也不增添第二个正文图。
/// 首列的真实粗体标记检查raw起点的隐藏gap；独立尾段保证完整CPU帧仍不是文档EOF。
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

/// 全部夹具是固定ASCII，字节与UTF16一一对应；预期来自生成规则，不能读取显示映射反推。
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

/// 只读导航不能改canonical、历史或磁盘；两条真实读取链都以固定小片完成精确核对。
/// 这些完整性读取发生在帧门之后，不把测试校验的读量混成生产排版frontier工作量。
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
    drop(document); // 真实Doc借用先结束，后面不跨Native或runloop保留租约。
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

/// 实际frontier和shape累计仍小于固定正文的一半，且父文档未结束，才证明回退仍属真实前缀。
/// 不能只和含隐藏标记的raw总长相比，否则排完显示正文后少掉的标记字节也可能误过门。
/// 显示转换自身可能逐片读完整raw；本门只声明真实排版前缀，不能外推成没有读取整格原源。
fn prefix(progress: &NativeMarkdownPagedBodyProgress) -> Result<(), String> {
    if progress.complete
        || progress.frontier_source_bytes >= UNIT.len() * REPEATS / 2
        || progress.shaped_input_bytes >= UNIT.len() * REPEATS / 2
    {
        return Err("巨行边缘定位以排到整格或文档EOF为代价".into());
    }
    Ok(())
}

/// 唯一原SourceTarget只滚动，不提交选择。原3秒observer必须交付当前完整CPU帧，
/// 然后才消费同请求proof；范围、完整Input、Source和实际布局宽始终由原票核对。
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
    drop(scene); // 后继目标不能借旧Reader补票；只留下无正文的原SceneKey与资源观察。
    rig.remember_receipt()?;
    println!(
        "native_body_table_edge stage={stage} target_location={} target_length={} actual_cpu_frame=true original_source_input_width=true actual_prefix_only=true frontier_bytes={} shaped_input_bytes={} giant_text_bytes={GIANT_TEXT_BYTES} formal=false whole_product=false",
        range.location, range.length, progress.frontier_source_bytes, progress.shaped_input_bytes,
    );
    Ok(key)
}

/// 先把同一未封存巨格的远端真正滚入clip，再回到其原物理行首；旧视口不能替行首签可见。
/// 首格以隐藏粗体开头，回来的零caret须与真实首个e的caret同位，非空pipe仍没有字符几何。
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
    drop(scene); // 旧远端窗口先归还，原Body才能继续同一个格Source的首行目标。
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

/// 成功与普通Err都先结束原Rig及其唯一Source/Native资源，才报告功能结果或进入下一个夹具。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    result
}
