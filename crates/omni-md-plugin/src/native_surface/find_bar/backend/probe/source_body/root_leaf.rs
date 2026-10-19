//! 同原Source的实际RootLeaf字体帧；marker字面保留，不宣称装饰/完整Markdown预览。
use std::io::Write;

use super::*;
use omni_markdown_core::MarkdownPresentationMode;

const TEXT: &str = "# Title\n\n```text\nlet value = 1;\n```\n\n## Next\n";

fn run_case(rig: &mut Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    // 期望位置来自固定源字面；不是由新Producer的结果反算heading/code范围。
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
        return Err("RootLeaf首片偷偷使用隐藏marker的小字体".into());
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
    if heading.actual_range != NSRange::new(2, 1)
        || code.actual_range != NSRange::new(17, 1)
        || heading.rect.size.height <= code.rect.size.height
        || heading.rect.size.width <= 0.0
        || code.rect.size.width <= 0.0
        || !current()
    {
        return Err("真实标题/代码框没有保留Source坐标与不同字体几何".into());
    }
    drop(scene); // 真实Find准备前归还旧Scene Reader，不占旧prefix快照槽。
    rig.bar.query.setStringValue(ns_string!("Title"));
    let range = NSRange::new(2, 5);
    let frame = observe(&rig.body)?;
    // Title已在当前viewport；原execute可同步提交选择，不应套远端样本必须Pending的前提。
    // 若实际仍需异步准备，则保留同一请求并由原Body续体完成，不能只凭选择提交判可见。
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
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.semantic_work.source_bytes == 0
        || progress.semantic_work.copied_source_bytes != 0
        || progress.semantic_work.parser_input_bytes != 0
    {
        return Err("RootLeaf没有真实收费或错误回到局部Parser/String路径".into());
    }
    rig.remember_receipt()?; // 包含实际新增heading/code capture，close沿原strict资源门逐一核释放。
    println!(
        "native_body_root_leaf actual_frame=true heading_points=28,24 code_points=14 exact_source_ranges=true literal_markers=true original_find_source=true semantic_work={:?} formal=false whole_product=false",
        progress.semantic_work
    );
    Ok(())
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
        )?;
        let result = run_case(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string())?;
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify(false)?;
    if publication.upgrade().is_some() {
        return Err("RootLeaf关闭后原有限Ready仍被业务强持有".into());
    }
    result
}
