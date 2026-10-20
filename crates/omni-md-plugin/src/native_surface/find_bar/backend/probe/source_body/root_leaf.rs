//! 同原Source的实际RootLeaf字体帧；原选择决定marker字号，不宣称完整Markdown装饰已完成。
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
        return Err("原选择所在标题的marker没有按原语义保持可见字号".into());
    }
    // 初始caret在第一标题；第二标题的实际语义才授予隐藏权。检查真正sealed part，
    // 不能只查palette或期望styles来冒充实际shaping已经使用该小字体。
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
    // Find进入第二标题会改变原canonical选择；后继必须重新取得同一来源的语义，
    // 使此前隐藏的相同源marker恢复标题字号，不能沿旧槽永久隐藏可编辑语法。
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
            )); // 保留实际封存part事实，失败不能只靠选择或已完成帧反推样式已更新。
        }
    }
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.semantic_work.source_bytes == 0
        || progress.semantic_work.copied_source_bytes != 0
        || progress.semantic_work.parser_input_bytes != 0
    {
        return Err("RootLeaf没有真实收费或错误回到局部Parser/String路径".into());
    }
    rig.remember_receipt()?; // 包含实际新增heading/code capture，close沿原strict资源门逐一核释放。
    println!(
        "native_body_root_leaf actual_frame=true heading_points=28,24 code_points=14 exact_source_ranges=true selection_bound_marker_points=0.1,24 canonical_markers_preserved=true original_find_source=true semantic_work={:?} formal=false whole_product=false",
        progress.semantic_work
    );
    Ok(())
}

/// 原canonical标签、真实sealed字体与实际文字几何共用同一个完整帧，不用Parser期望反造parts。
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
        } // 遮罩只定位真实<u>三个字面字符，不借中文或emoji墨迹替隐藏标签过门。
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
            // 只计Rig黑色字形墨迹；新代码底色最小通道约33，不能拿整块深底替字面标签过门。
            **marker == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    if ink == 0 {
        return Err("代码中的原<u>标签没有真实像素墨迹".into());
    }
    drop(scene); // 原Reader/Font/Source强边在收据与close前归还，位图和mask也不逃出本次固定控制。
    rig.remember_receipt()?;
    println!(
        "native_body_root_leaf_code_literal actual_marker_ink={ink} bounded_paint_steps={steps} original_utf16=true actual_code_points=14 actual_tag_rects=true no_underline_or_hidden=true original_complete_frame=true formal=false whole_product=false"
    );
    Ok(())
}

/// 每种真实代码块单独装配和关闭；上一份字体、Reader与像素全部归还后才构造下一种。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result)) // 即使某个断言失败也先沿原严格拆图，不留下合成窗口或Font owner。
    })?;
    retirement.verify(false)?;
    if publication?.upgrade().is_some() {
        return Err("代码字面关闭后原RootLeaf Ready仍被业务持有".into());
    }
    result
}

/// 用户刚键入后立即点击标题按钮，像素尚在重建也必须接纳同一真实选择的格式事务。
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
            // 真实输入回调先发布中文；这里不运行消息循环，不等旧Scene重建后才补按格式按钮。
            // SAFETY: 完整的短NSString与未指定替换范围沿原NSTextInputClient入口消费当前caret。
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
            // 独立短文稿才允许完整oracle；生产的格式和保存仍直接消费原piece root。
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
            drop(window); // 真实事件只带窗口号，不能让验收局部 claim 延长原窗口的关闭寿命。
            // 格式与字符键入是两个独立用户动作；原Undo逐笔恢复，不能合并或吞掉其中一步。
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
                menu.performActionForItemAtIndex(0); // 由真实NSMenu将原action交给本客户，不直接调用测试替代命令。
                drop((undo, menu)); // 本次菜单的临时强引用必须先归还，owner仍只保当前唯一菜单。
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
            // 保存和资源释放仍由实际owner负责，测试不私改selection、Scene或撤销栈。
            println!(
                "native_root_format immediate_after_input=true actual_text_input_client=true native_context_menu_undo=true no_added_shortcuts=true two_real_undo=true disk_unchanged=true whole_product=false formal=false"
            );
            rig.remember_receipt()
        })();
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?; // 失败也在报告前归还真实Native、字体、Reader与原映射。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify(false)?;
    if publication.upgrade().is_some() {
        return Err("RootLeaf关闭后原有限Ready仍被业务强持有".into());
    }
    result?;
    literal_code_case(mtm, "outside\n\n```text\n<u>中文😀</u>\n```\n\n")?;
    literal_code_case(mtm, "outside\n\n    <u>中文😀</u>\n\n")?;
    // 每个独立场景都在返回前核自己的完整关闭；这里顺序执行全部原流程，
    // 把各自失败一并保留，便于一次真实运行发现编辑、字形和链接各自的生产缺陷。
    // 不在失败后重放同一动作，不缩小样本，也不改变原期限、像素或资源断言。
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
