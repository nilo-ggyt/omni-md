//! 同一固定短例分别实际排标准 TextKit 与分页候选；资源图不同时存活，也不共享新几何策略。

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

/// 候选退出后只保有限输入列宽和已经显示的行；标准 oracle 不需要第二张正文图同活。
struct Candidate {
    lines: Vec<crate::native_surface::preview_paragraph::PreviewLineObservation>,
    width: f64,
    height: f64,
    origin_y: f64,
    parts: Vec<super::paragraph_oracle::PartAdvance>,
}

/// 字符预期只来自固定字面与这一次插入；逐片核完整 canonical，不能用新选区反造期望。
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
                0x4e2d // 正常 NSTextInputClient 本次唯一输入，不替换标点或后方原文。
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
    Ok(()) // 文稿借用在下一次 Native 调用之前结束，不保住旧 Source 或选择权限。
}

/// 独立文件句柄始终只读合成源；显示、鼠标、未保存输入与 Undo 都不能暗中写回磁盘。
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

/// 原 SourceTarget 真滚动但不改选择；取走同一次可见收据，不以范围已存在代替实际帧。
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

/// 每次实际按下前重取当前 Scene；字符框和两端 caret 必须来自同一张当前出版物。
/// 中文标点均为本短例中的独立 BMP 字素；两侧几何不能由另一排版引擎的 advance 推算。
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
    // 这里的 1/64 仍是原布局坐标的一个单位；不放宽旧 Native 对照的 glyph 或行几何要求。
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
    drop(scene); // 原 Reader 在 convertPoint 和真实 NSEvent 前归还，不跨点击保旧排版权。
    Ok(rig.view.convertPoint_toView(local, None))
}

/// 真 Down/Up 后再读 canonical caret；随后一次输入沿当前选择提交，不指定已知替换坐标绕开点击。
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
            // 40pt正文只要求真实当前视口完整，点击重排可合法停在已覆盖前缀，不能强读固定尾段。
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
        return Ok(()); // 尺寸和滚动往返只核真实选择，不为每轮几何观察追加历史事务。
    }
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let event = observe(&rig.body)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
    // SAFETY: 同主线程的原 client；NSNotFound 表示正常输入采用刚点击出的当前选择。
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

/// 原两个引用短例已有这些标点和真实样式，不改字面来避开已观察到的 Native 字形差异。
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
    // 同一个真实窗口改变列宽，原字号、源文本与输入 owner 保持；不能用新 Rig 避开重排。
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
    // 先前已完整准备的同宽短源缩为40pt正文，真正滚到尾标点再回首屏；不增加未读正文预算。
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
    // 固定字面给出独立尾段位置，再由当前真实字符几何证明它确实在40pt窗口外。
    // 不读取progress.complete反算期望：之后点击激活样式重置Composer，尾段不属于当前视口需求。
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
    drop(scene); // 屏外证明结束即归还Reader，不能跨真实鼠标保旧排版或阻止有限前缀重排。
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

/// 只使用真实 sealed 窗口的纯值；容差固定为同布局格式的一个 26.6 单位，不因失败增大。
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
        ); // 只移除同夹具的外层 padding 并转换 y-up；不从旧结果反算候选行框。
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
        ); // 固定短例只输出纯值；失败仍保原容差，不通过后继算式重写旧 Native 结果。
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

/// 每个短例先完整关闭分页图；失败也沿同一个真实资源门结束，不把临时窗口留给下一例。
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
                // 有上界短例只观察真实注册Font；换行或行槽失败时可核对实际face与系统度量，
                // 不从语义角色猜字号，也不把字体强引用带到独立TextKit对照阶段。
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
                        // Reader和Body内部借用已在probe返回前退出；此处只打印同一原Font的
                        // 已捕获Core元数据与封存glyph纯值，不调用Native另取字体或重形。
                        // 总推进相等不能排除同宽错字形，因此独立OT对照必须保留真实cluster/id。
                        eprintln!(
                            "native_paragraph_glyph_observation case={name} source={:?} script={:?} direction={:?} font_metadata={:?} glyphs={:?}",
                            part.source,
                            part.script,
                            part.direction,
                            font.native().core_font().metadata(),
                            part.glyphs,
                        );
                        // 只观察本固定引用短例的真实完整 part；旧参考在图关闭后独立创建。
                        // probe 本身已限制总 part 数，跨关闭只保存有界纯值，不延长任何字体或出版物寿命。
                        parts.push(super::paragraph_oracle::PartAdvance {
                            source: part.source,
                            advance_26_6: part.advance_26_6,
                        });
                    }
                }
            }
            // 字体和几何消费只能观察源；按固定小片确认当前文档及原磁盘，不能把渲染成功换成内容改写。
            let original_document = rig.document.upgrade().ok_or("段落原 Doc 已提前归还")?;
            let document = original_document.borrow();
            // 此流程没有编辑命令，连内容世代也不得变化；实际显示前缀、软行和 CRLF 仍须保原源。
            if document.generation() != 0 || document.utf16_len() != source.encode_utf16().count() {
                return Err(format!("{name}: 段落排版改变了原文档"));
            }
            let mut expected = source.encode_utf16();
            let mut units = [0_u16; 128];
            // 固定短例也保持生产读取边界，不因 oracle 输入小就回到全文正文复制接口。
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
            drop(document); // 真实文档借用在文件 I/O 和后继 Native 收尾之前归还。
            drop(original_document);
            // 源缓冲区未改不代表没有误保存；独立文件句柄核每片字节及真实 EOF。
            unchanged_disk(&rig, name, source)?;
            if name.starts_with("scoped_quote") {
                // 原对照的纯值已经冻结；随后真实鼠标/编辑/重排不能改写旧帧的期望或覆盖差异。
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
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify(false)?;
    if publication.upgrade().is_some() {
        return Err(format!(
            "{name}: 短几何对照结束后原语义 publication 仍未释放"
        ));
    }
    let candidate = result?;
    let mut differences = Vec::new();
    // 候选已严格关闭；完整旧 TextKit 图先确定真实软行，再归还其全部 owner。
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    let original =
        super::paragraph_oracle::measure(source, candidate.width, &font, &candidate.parts)?;
    if !candidate.parts.is_empty() {
        // 原 TextKit 真实行内已采字形边缘；这里只合计纯值，不再用 CTLine 的另一套 CJK 行末宽。
        // 这个值用于观察平台排版政策，不是 OpenType 引擎的正确性期望：系统 UIFont 会把
        // 中文标点替为半宽字形并按上下文分配间距，空 feature 的真实 OpenType 整形没有这项默认行为。
        // 本轮差异所涉的 14pt 片段已用同字体表、face、轴、尺度和原上下文的独立 HarfBuzz
        // 逐项核过实际 glyph、cluster、advance 与 offset；未将此结论外推到其他字体和轴。
        // 不能用跨引擎总宽相等代替该证明，也不能为追平它补固定像素。
        let expected = super::paragraph_oracle::part_advances(source, &original, &candidate.parts)?;
        for (part, expected) in candidate.parts.iter().zip(expected) {
            eprintln!(
                "native_paragraph_advance_observation case={name} source={:?} actual_26_6={} textkit_26_6={expected} identical={} actual_policy=opentype_default reference_policy=coretext_contextual",
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
        return Ok(Some(differences.join("\n"))); // 图均已真实释放，完整 part 与几何的独立差异一起保留。
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
                // 后继真实装配、交互或资源失败仍立即停止；先前已严格清场的独立差异不能被它吞掉。
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
