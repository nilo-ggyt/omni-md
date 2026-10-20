//! 固定短表沿正式 Body/Cell 分页链实绘与查找；不以独立单格原语代替真实 TableRow 接线。

use super::*;
use crate::native_surface::{
    paged_input::NativeMarkdownPagedInputOutcome, paged_text_view::NativeMarkdownPagedHeadAffinity,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use std::io::Read;

#[path = "table/alignment.rs"]
mod alignment;
#[path = "table/insertion.rs"]
mod insertion;

#[path = "table/cell_find.rs"]
mod cell_find;
#[path = "table/cell_word.rs"]
mod cell_word;

const SOURCE: &str = concat!(
    "before-table\r\n\r\n",
    "| 名称 | 内容 |\r\n",
    "| --- | --- |\r\n",
    "| **BoldAlpha** *ItalicBeta* | [链接中文](#omni-table) `a\\|b` |\r\n",
    "| 行二中文 | wrap-start 中文自动换行 keeps bounded text in its cell 中文仍在同格 wrap-end |\r\n",
    "\r\nafter-table\r\n",
);

/// 固定字面独立给出 canonical UTF16，不能从待测显示映射反推期望位置。
fn canonical(label: &str) -> usize {
    SOURCE[..SOURCE.find(label).expect("固定表格夹具标签唯一存在")]
        .encode_utf16()
        .count()
}

/// 源和磁盘都按固定小片核验；表格显示转换、Find和点击均无权改写原 Markdown/CRLF。
fn unchanged(rig: &Rig) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("表格原Doc已提前释放")?;
    let document = document.borrow();
    if document.utf16_len() != SOURCE.encode_utf16().count() || document.generation() != 0 {
        return Err("表格只读操作改变了原Doc长度或generation".into());
    }
    let mut expected = SOURCE.encode_utf16();
    let mut offset = 0;
    let mut units = [0_u16; 128];
    while offset < document.utf16_len() {
        let used = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        for unit in &units[..used] {
            if expected.next() != Some(*unit) {
                return Err("表格原语法、转义管道或CRLF已被显示流程改写".into());
            }
        }
        offset += used;
    }
    drop(document);
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for expected in SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..expected.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..expected.len()] != expected {
            return Err("表格原固定文件字节已被只读流程改写".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("表格原文件末尾出现额外内容".into());
    }
    Ok(())
}

/// 实际格字符框必须映回该独立 canonical 字符，命中不能落进管道、语法标记或邻格。
fn character(scene: &dyn NativeMarkdownPagedTextScene, offset: usize) -> Result<NSRect, String> {
    let first = scene
        .first_rect(NSRange::new(offset, 1))
        .map_err(|cause| format!("table canonical={offset}: {cause}"))?;
    if first.actual_range != NSRange::new(offset, 1)
        || first.rect.size.width <= 0.0
        || first.rect.size.height <= 1.0
    {
        return Err(format!(
            "表格原字符{offset}没有真实非空字符框或canonical映射错误：range={:?}, rect={:?}",
            first.actual_range, first.rect
        ));
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.1,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let hit = scene.hit(local).map_err(|cause| cause.to_string())?;
    if hit != offset {
        return Err(format!("表格实际点击落在{hit}，应为原字符{offset}"));
    }
    Ok(first.rect)
}

/// 当前真实三行格窗口同时检查两列、软换行和CPU墨迹；不从候选row高度反造期望布局。
fn geometry_and_paint(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let columns = [
        (
            character(&*scene, canonical("名称"))?,
            character(&*scene, canonical("内容"))?,
        ),
        (
            character(&*scene, canonical("BoldAlpha"))?,
            character(&*scene, canonical("链接中文"))?,
        ),
        (
            character(&*scene, canonical("行二中文"))?,
            character(&*scene, canonical("wrap-start"))?,
        ),
    ];
    let tolerance = 1.0 / 64.0;
    for (index, (left, right)) in columns.iter().enumerate() {
        if left.origin.x >= right.origin.x
            || (left.origin.x - columns[0].0.origin.x).abs() > tolerance
            || (right.origin.x - columns[0].1.origin.x).abs() > tolerance
            || (left.origin.y + left.size.height - right.origin.y - right.size.height).abs()
                > tolerance
        {
            return Err(format!(
                "真实表格第{index}行列框错位：left={left:?}, right={right:?}"
            ));
        }
        if index != 0 && left.origin.y + left.size.height >= columns[index - 1].0.origin.y {
            return Err(format!("真实表格第{index}行与前行重叠"));
        }
    }
    let wrapped = character(&*scene, canonical("wrap-end"))?;
    if wrapped.origin.y >= columns[2].1.origin.y {
        return Err("表格中英文长格没有在实际最终列宽下软换行".into());
    }
    character(&*scene, canonical("ItalicBeta"))?;
    character(&*scene, canonical("a\\|b"))?;
    character(&*scene, canonical("after-table"))?;
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // 同一实际Grid比较原同步完整帧与每步1项预算的异步帧，边线接缝与代码/选区都必须零RGBA差。
    selection::exact_scene_rgba(
        scene.clone(),
        &key,
        rig.panel.backingScaleFactor(),
        &current,
    )?;
    let canvas = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    let steps = selection::paint_scene(scene.clone(), &key, &canvas, &current)?;
    let mask = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    for (left, right) in columns {
        CGContext::fill_rect(Some(&mask.context), left);
        CGContext::fill_rect(Some(&mask.context), right);
    }
    CGContext::flush(Some(&canvas.context));
    CGContext::flush(Some(&mask.context));
    let ink = canvas
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(pixel, mask)| {
            **mask == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    if ink == 0
        || rig.view.cpu_paint_pending() != Some(false)
        || rig.view.cpu_paints().is_none_or(|paints| paints == 0)
    {
        return Err("真实格只有几何或背景，没有实际完整CPU字符帧".into());
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    println!(
        "native_body_table_row actual_grid_frame=true rows=3 columns=2 actual_soft_wrap=true canonical_rect_and_hit=true actual_text_ink={ink} bounded_paint_steps={steps} whole_product=false formal=false"
    );
    Ok(())
}

/// 查找沿原FindBar→Body→真实Cell可见证明，跨行和表后文字都不能只交付选择而缺实际帧。
fn find(
    rig: &mut Rig,
    query: &str,
    offset: usize,
    complete: bool,
    action: MarkdownFindAction,
    stage: &str,
) -> Result<(), String> {
    let old = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.bar.query.setStringValue(&NSString::from_str(query));
    let frame = observe(&rig.body)?;
    let pending = match rig.action(action).map_err(|cause| cause.to_string())? {
        NativeMarkdownFindExecution::Complete => None,
        NativeMarkdownFindExecution::Pending(request) => {
            rig.requests.push(request.clone());
            Some(request)
        }
    };
    if complete {
        frame::wait_complete_frame(stage, &rig.body, frame)?;
    } else {
        // 长表后保留真实未布局尾部；目标帧不能以完成全篇扫描为代价换取成功。
        frame::wait_frame(stage, &rig.body, frame)?;
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let range = NSRange::new(offset, query.encode_utf16().count());
    if key.source != old.source || key.request.input.primary_range != range {
        return Err(format!(
            "表格Find丢失原Source或canonical范围：query={query}, actual={:?}, expected={range:?}",
            key.request.input.primary_range
        ));
    }
    if let Some(request) = pending
        && (request.pending()
            || !request.completed_visible()
            || request.cancelled()
            || request.take_failure().is_some()
            || request.range() != range)
    {
        return Err(format!("表格Find未交付原请求的真实可见证明：{query}"));
    }
    rig.view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    rig.remember_receipt()?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    selection::exact_scene_rgba(scene, &key, rig.panel.backingScaleFactor(), &current)?;
    Ok(())
}

/// 原唯一Input的公开选择意图表达换行侧；只提交选择，再由真实View通知原Body发布完整帧。
/// 本入口用于行尾上下游业务行为，既不改Doc私态，也不把240文首光标挪进格逃避原Bug。
fn table_caret_intent(
    rig: &mut Rig,
    position: usize,
    affinity: NativeMarkdownPagedHeadAffinity,
    stage: &str,
) -> Result<(), String> {
    let frame = observe(&rig.body)?;
    {
        let input = rig
            .body
            .probe_find_input_weak()
            .upgrade()
            .ok_or("表格原Input已释放")?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let geometry = rig.view.find_geometry_epoch().ok_or("表格原几何已撤销")?;
        let current = |epoch| {
            epoch == EPOCH
                && rig
                    .view
                    .find_input_is_current(&input, epoch, &before, Some(geometry))
                && native_markdown_controller_has_foreground_authority(&rig.controller)
        };
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let outcome = input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(position),
                Some(affinity.is_upstream()),
                None,
                &current,
            )
            .map_err(|cause| cause.to_string())?;
        let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
            return Err("表格caret意图没有发布原唯一选择收据".into());
        };
        if commit.state.generation != before.generation
            || commit.state.primary_range != NSRange::new(position, 0)
        {
            return Err("表格caret空间投影改变了原文或canonical请求位置".into());
        }
        rig.view
            .apply_find_commit(&input, &commit)
            .map_err(|cause| cause.to_string())?;
    } // permit/原Input临时强边不跨实际帧等待和关闭资源门。
    frame::wait_complete_frame(stage, &rig.body, frame)?;
    {
        // 本次显式换行侧属于原选择意图；同源样式重排可以撤旧字形坐标，不能吞掉刚发布的侧。
        // 只借原公开Input票核事实，不窥Scene缓存；这样失败能指出亲和在发布前已经丢失。
        let input = rig
            .body
            .probe_find_input_weak()
            .upgrade()
            .ok_or("表格原Input已释放")?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let current = |epoch| {
            epoch == EPOCH
                && rig
                    .view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(&key))
        };
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let (actual, _) = input
            .navigation_selection_facts(&permit, &current)
            .map_err(|cause| cause.to_string())?;
        if actual != Some(affinity.is_upstream()) {
            return Err(format!(
                "{stage}: 本次显式caret亲和在真实帧发布时丢失：canonical={position}, actual_upstream={actual:?}, expected_upstream={}",
                affinity.is_upstream(),
            ));
        }
    } // 真实Input、permit和key强边在后续资源收据前归还，不改变canonical或补签选择。
    rig.remember_receipt()
}

/// 固定短表中的管道和delimiter没有文字，但零范围的空间位置必须由真实首/末/相邻格caret提供。
/// 每次保留原canonical位置，再比较实际公开字符几何；不从TableRow零字号构造任何框。
fn syntax_carets(rig: &mut Rig) -> Result<(), String> {
    use NativeMarkdownPagedHeadAffinity::{Downstream, Upstream};
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let header_pipe = canonical("| 名称");
    let middle_pipe = canonical("| 内容");
    let delimiter = canonical("| ---");
    let row_end = canonical("| **BoldAlpha");
    // 字面独立指定语法位置和其真实邻格边界，Source跨度中的CRLF不当作显示字符。
    for (stage, position, affinity, expected) in [
        (
            "table_header_pipe_caret",
            header_pipe,
            Downstream,
            canonical("名称"),
        ),
        (
            "table_middle_pipe_upstream",
            middle_pipe,
            Upstream,
            canonical("名称") + 2,
        ),
        (
            "table_middle_pipe_downstream",
            middle_pipe,
            Downstream,
            canonical("内容"),
        ),
        (
            "table_delimiter_caret",
            delimiter,
            Downstream,
            canonical("内容") + 2,
        ),
        (
            "table_row_end_upstream",
            row_end,
            Upstream,
            canonical("内容") + 2,
        ),
        (
            "table_row_end_downstream",
            row_end,
            Downstream,
            canonical("BoldAlpha"),
        ),
    ] {
        table_caret_intent(rig, position, affinity, stage)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let projected = scene
            .first_rect(NSRange::new(position, 0))
            .map_err(|cause| cause.to_string())?;
        let actual = scene
            .first_rect(NSRange::new(expected, 0))
            .map_err(|cause| cause.to_string())?;
        if key.source != initial.source
            || key.request.input.primary_range != NSRange::new(position, 0)
            || projected.actual_range != NSRange::new(position, 0)
            || projected.rect != actual.rect
        {
            return Err(format!(
                "{stage}: 原语法caret没有保canonical位置或没有使用真实格boundary：canonical={position}, affinity={affinity:?}, expected_boundary={expected}, same_source={}, selection={:?}, projected_range={:?}, projected_rect={:?}, boundary_range={:?}, boundary_rect={:?}",
                key.source == initial.source,
                key.request.input.primary_range,
                projected.actual_range,
                projected.rect,
                actual.actual_range,
                actual.rect,
            ));
        }
        let current = || {
            rig.view
                .capture_ready_scene_key()
                .is_ok_and(|value| value.same(&key))
        };
        selection::exact_scene_rgba(
            scene.clone(),
            &key,
            rig.panel.backingScaleFactor(),
            &current,
        )?;
        // 非空管道请求仍没有字符；不能把caret空间投影扩大成可查找/可复制的邻格文字。
        if !matches!(
            scene.first_rect(NSRange::new(position, 1)),
            Err(ViewError::Pending)
        ) {
            return Err("隐藏管道被错误当作邻格的可见字符".into());
        }
    }
    table_caret_intent(rig, 0, Downstream, "table_syntax_caret_restore")?;
    unchanged(rig)
}

/// 链接只在真实 Command mouseUp 消费，固定片段目标保 typed 拒绝；不打开浏览器。
fn link(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    rig.panel.makeFirstResponder(Some(&**rig.view));
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let window_point = links::point(rig, canonical("链接中文"))?;
    let local = rig.view.convertPoint_fromView(window_point, None);
    if scene
        .link_hit(local)
        .map_err(|cause| cause.to_string())?
        .is_none()
        || scene
            .link_target(local)
            .map_err(|cause| cause.to_string())?
            .as_deref()
            != Some("#omni-table")
    {
        return Err("真实格链接没有命中可见标签或丢失原Parser目标".into());
    }
    drop(scene);
    links::command_mouse(rig, window_point, NSEventType::LeftMouseDown)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != key.request.input {
        return Err("表格链接mouseDown提前改写了选择或吞掉原拖动入口".into());
    }
    links::command_mouse(rig, window_point, NSEventType::LeftMouseUp)?;
    if !matches!(links::settle(rig)?, Some(ViewError::LinkAnchorUnsupported)) {
        return Err("表格链接真实mouseUp没有按现片段导航边界交付typed结果".into());
    }
    unchanged(rig)
}

/// 失败同样先沿原严格Native/Font/Source/像素门清场，不能靠后续用例关闭当前泄漏图。
pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            SOURCE.encode_utf16().count(),
            SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| {
            unchanged(&rig)?;
            geometry_and_paint(&rig)?;
            syntax_carets(&mut rig)?;
            link(&mut rig)?;
            for query in ["链接中文", "行二中文", "after-table", "BoldAlpha"] {
                find(
                    &mut rig,
                    query,
                    canonical(query),
                    true,
                    MarkdownFindAction::Next,
                    "table_row_find",
                )?;
                unchanged(&rig)?;
            }
            rig.remember_receipt()
        })();
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    if result.is_ok() {
        println!(
            "native_body_table_row source_and_disk_unchanged=true actual_find_and_link=true strict_original_release=true offscreen_row_restore_tested=false cross_cell_edit_tested=false physical_ime_tested=false whole_product=false formal=false"
        );
    }
    // 下列场景各自拥有独立文稿与严格关闭边，不共享失败后的状态；保留任一真实失败，
    // 同时继续核其它用户操作，避免跨格缺陷遮住列对齐、插表、长表或横向滚动的新问题。
    let mut failures = Vec::new();
    for (name, result) in [
        ("row", result),
        ("active", active_table_ui(mtm)),
        ("word", cell_word::run(mtm)),
        ("alignment", alignment::run(mtm)),
        ("insertion", insertion::run(mtm)),
        ("long", long_table(mtm)),
        ("wide", wide_table(mtm)),
    ] {
        if let Err(cause) = result {
            eprintln!("native_root_table_case role={name} outcome=failed reason={cause}");
            failures.push(format!("{name}: {cause}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

const LONG_HEADER: &str = "| 普通 | 粗体 | 斜体 | 行内代码 |\r\n| --- | --- | --- | --- |\r\n";
const LONG_ROWS: usize = 240;
const LONG_TAIL: &str = "\r\n表后尾部保持按视口读取\r\n";
const LONG_TAIL_REPEATS: usize = 128;

/// 只保首末两行的独立输入位置；四个源端点来自生成字面而非待测 Parser/显示映射。
struct LongRowTarget {
    query: String,
    canonical: [usize; 4],
}

/// 每次仅创建一条固定短行；生成和逐块核对都不聚合整表或整篇字符串。
fn long_row(row: usize) -> String {
    format!(
        "| 中文普通{row:04} | **中文粗体{row:04}** | *中文斜体{row:04}* | `中文代码{row:04}` |\r\n"
    )
}

/// 首末目标以当前固定行模板独立计算 UTF16；实际产品只能从 mmap 和原结构票取得内容。
fn long_target(row: usize, offset: usize) -> LongRowTarget {
    let text = long_row(row);
    let labels = [
        format!("中文普通{row:04}"),
        format!("中文粗体{row:04}"),
        format!("中文斜体{row:04}"),
        format!("中文代码{row:04}"),
    ];
    let canonical = labels.each_ref().map(|label| {
        let byte = text.find(label.as_str()).expect("固定输入每列标签唯一存在");
        offset + text[..byte].encode_utf16().count()
    });
    LongRowTarget {
        query: labels[0].clone(),
        canonical,
    }
}

/// 检查整份实际 Doc 与磁盘，仅保一行模板和128单元栈片；原保存位置没有任何写入动作。
fn long_unchanged(rig: &Rig) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("长表原Doc已提前释放")?;
    let document = owner.borrow();
    if document.generation() != 0 {
        return Err("长表只读交互改变了原文稿代次".into());
    }
    let mut disk = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut offset = 0;
    let mut units = [0_u16; 128];
    let mut bytes = [0_u8; 128];
    let pieces = std::iter::once(LONG_HEADER.to_owned())
        .chain((1..=LONG_ROWS).map(long_row))
        .chain((0..LONG_TAIL_REPEATS).map(|_| LONG_TAIL.to_owned()));
    for piece in pieces {
        let mut expected = piece.encode_utf16();
        loop {
            // 单行也按固定栈片核原 canonical 内容，不把多字节中文当作 UTF16 单元偏移。
            let mut used = 0;
            for _ in 0..units.len() {
                let Some(unit) = expected.next() else {
                    break;
                };
                units[used] = unit;
                used += 1;
            }
            if used == 0 {
                break;
            }
            let mut actual = [0_u16; 128];
            document
                .copy_utf16_code_units(offset, &mut actual[..used])
                .map_err(|cause| cause.to_string())?;
            if actual[..used] != units[..used] {
                return Err(format!("长表原 canonical 在{offset}被显示或Find改写"));
            }
            offset += used;
        }
        for expected in piece.as_bytes().chunks(bytes.len()) {
            disk.read_exact(&mut bytes[..expected.len()])
                .map_err(|cause| cause.to_string())?;
            if &bytes[..expected.len()] != expected {
                return Err("长表原磁盘 Markdown、样式标记或 CRLF 被只读流程改写".into());
            }
        }
    }
    if offset != document.utf16_len()
        || disk
            .read(&mut bytes[..1])
            .map_err(|cause| cause.to_string())?
            != 0
    {
        return Err("长表原文稿或磁盘出现遗漏/额外尾部".into());
    }
    Ok(())
}

/// 逐列实际字符框/命中、有限 paint 和真实 CPU 帧必须都完成，不能只签末行选择。
fn long_frame(rig: &Rig, target: &LongRowTarget) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let mut rectangles = [NSRect::new(NSPoint::ZERO, NSSize::ZERO); 4];
    for (column, offset) in target.canonical.iter().enumerate() {
        rectangles[column] = character(&*scene, *offset)?;
        if column > 0 && rectangles[column].origin.x <= rectangles[column - 1].origin.x {
            return Err("长表四列真实字符框没有保持原列顺序".into());
        }
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // 原同步帧与每步一项的真实预算帧仍须零 RGBA 差，不为长表另造绘制入口。
    selection::exact_scene_rgba(
        scene.clone(),
        &key,
        rig.panel.backingScaleFactor(),
        &current,
    )?;
    let canvas = selection::Canvas::scaled(key.request.viewport, 1.0)?;
    selection::paint_scene(scene.clone(), &key, &canvas, &current)?;
    CGContext::flush(Some(&canvas.context));
    for (column, rectangle) in rectangles.iter().enumerate() {
        // 每列独立遮罩必须都有真实字符墨迹；其它列画对不能掩盖粗体、斜体或代码格空白。
        let mask = selection::Canvas::scaled(key.request.viewport, 1.0)?;
        CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
        CGContext::fill_rect(Some(&mask.context), *rectangle);
        CGContext::flush(Some(&mask.context));
        let ink = canvas
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(mask.pixels.as_chunks::<4>().0)
            .any(|(pixel, mask)| {
                *mask == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
            });
        if !ink {
            return Err(format!("长表第{column}列没有实际字符墨迹"));
        }
    }
    if rig.view.cpu_paint_pending() != Some(false)
        || rig.view.cpu_paints().is_none_or(|paints| paints == 0)
    {
        return Err("长表目标没有真实完成的 CPU 文字帧".into());
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    Ok(())
}

/// 原240行夹具从管道0开始，首帧不准移动初始selection；其空间投影只能来自真实首格caret。
/// 同时核非空管道仍无显示字符，不能靠给TableRow结构part补假字号使绘图通过。
fn long_initial_caret(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.primary_range != NSRange::new(0, 0) {
        return Err("240行首帧把原文首光标挪进格以绕过隐藏管道".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let original = scene
        .first_rect(NSRange::new(0, 0))
        .map_err(|cause| cause.to_string())?;
    let first_cell = scene
        .first_rect(NSRange::new(2, 0))
        .map_err(|cause| cause.to_string())?;
    if original.actual_range != NSRange::new(0, 0)
        || first_cell.actual_range != NSRange::new(2, 0)
        || original.rect != first_cell.rect
        || !matches!(
            scene.first_rect(NSRange::new(0, 1)),
            Err(ViewError::Pending)
        )
    {
        return Err("240行文首语法没有按真实首格边界保留原canonical语义".into());
    }
    Ok(())
}

/// 末行 Find 必须实际跨过240行再回首行；每次仍使用原三秒帧门，失败也先关闭本轮真实图。
fn long_table(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(LONG_HEADER.as_bytes())
            .map_err(|cause| cause.to_string())?;
        let mut utf16 = LONG_HEADER.encode_utf16().count();
        let first = long_target(1, utf16);
        let mut last = None;
        for row in 1..=LONG_ROWS {
            if row == LONG_ROWS {
                last = Some(long_target(row, utf16));
            }
            let text = long_row(row);
            file.write_all(text.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += text.encode_utf16().count();
        }
        // 真正尚未显示的尾部防止把整篇 complete 冒充末行 Find 的视口就绪。
        for _ in 0..LONG_TAIL_REPEATS {
            file.write_all(LONG_TAIL.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += LONG_TAIL.encode_utf16().count();
        }
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            utf16,
            2 + LONG_ROWS + LONG_TAIL_REPEATS * 2 + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let last = last.expect("固定240行包含真实末行");
        let result = (|| {
            long_unchanged(&rig)?;
            long_initial_caret(&rig)?;
            long_frame(&rig, &first)?;
            for (stage, target) in [
                ("table_240_last_find", &last),
                ("table_240_first_restore", &first),
            ] {
                find(
                    &mut rig,
                    &target.query,
                    target.canonical[0],
                    false,
                    MarkdownFindAction::Next,
                    stage,
                )?;
                long_frame(&rig, target)?;
                long_unchanged(&rig)?;
            }
            rig.remember_receipt()
        })();
        // 原 release 遍历每份真实字体 payload/mmap/FD 和 worker；没有为复用放宽资源断言。
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    result?;
    println!(
        "native_body_table_240 rows=240 columns=4 cjk_plain_strong_emphasis_code=true actual_canonical_rect_hit=true actual_cpu_frame=true actual_last_find_first_restore=true source_and_disk_unchanged=true strict_original_font_release=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const WIDE_COLUMNS: usize = 20;
const WIDE_ROWS: usize = 3;
const WIDE_LEAD: &str = "宽表前的独立正文保持原布局\r\n\r\n";
const WIDE_LEAD_REPEATS: usize = 32;
const WIDE_LEFT: &str = "左端首列目标";
const WIDE_RIGHT: &str = "右端末列目标";
const WIDE_TAIL: &str = "宽表后仍未显示的固定尾段\r\n\r\n";
const WIDE_TAIL_REPEATS: usize = (1024 * 1024) / WIDE_TAIL.len();

/// 固定20列的表头包含不同长度，真实自然宽必须由产品原解算器取得，不能用均列宽通过往返。
/// 这里只保一条有限输入行；不读取产品列宽，不生成整表或尾部长文字符串。
fn wide_header() -> String {
    let mut header = String::from("|");
    // 长度变化只属于固定输入配方；正式列宽仍由真实字体和原表格解算器决定。
    // 这样左右两端的坐标不会因为测试和生产共用一份均列宽假设而同时算错。
    for column in 1..=WIDE_COLUMNS {
        let varying = "宽".repeat(column % 4 + 1);
        header.push_str(&format!(" 表头{column:02}{varying}中文 |"));
    }
    header.push_str("\r\n|");
    for _ in 0..WIDE_COLUMNS {
        header.push_str(" --- |"); // 每个真实列都有独立GFM分隔格，不能由可见列数反推表格输入。
    }
    header.push_str("\r\n");
    header
}

/// 首行两个独立文字目标落在真实第一/第二十列；其它格同样写入原Markdown，而非空占位。
fn wide_row(row: usize) -> String {
    let mut text = String::from("|");
    for column in 1..=WIDE_COLUMNS {
        text.push(' ');
        if row == 1 && column == 1 {
            text.push_str(WIDE_LEFT);
        } else if row == 1 && column == WIDE_COLUMNS {
            text.push_str(WIDE_RIGHT);
        } else {
            text.push_str(&format!("真实第{row:02}行第{column:02}列"));
        }
        text.push_str(" |");
    }
    text.push_str("\r\n");
    text
}

/// 只在完整帧门之后逐个小片核canonical与原磁盘。检查本身可以顺序读尾部，产品仍不得
/// 为显示目标准备全部尾部；两者由独立生成配方和原实际progress事实分别证明。
fn wide_unchanged(rig: &Rig) -> Result<(), String> {
    let document = rig.document.upgrade().ok_or("宽表原Doc已提前释放")?;
    if document.borrow().generation() != 0 {
        return Err("宽表只读Find改变了原文稿代次".into());
    }
    let mut disk = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    // 源顺序由输入合同独立重建；每次最多一行和两个128单元栈片，不从显示目录反序列化期望。
    // 尾部也逐段核验，避免只比较两个标签而漏掉查找意外写回或截短磁盘文件。
    let pieces = (0..WIDE_LEAD_REPEATS)
        .map(|_| WIDE_LEAD.to_owned())
        .chain(std::iter::once(wide_header()))
        .chain((1..=WIDE_ROWS).map(wide_row))
        .chain((0..WIDE_TAIL_REPEATS).map(|_| WIDE_TAIL.to_owned()));
    let mut utf16 = 0;
    let mut byte_length = 0;
    let mut units = [0_u16; 128];
    let mut actual = [0_u16; 128];
    let mut bytes = [0_u8; 128];
    for piece in pieces {
        let mut expected = piece.encode_utf16();
        loop {
            let mut used = 0;
            for unit in &mut units {
                let Some(value) = expected.next() else { break };
                *unit = value;
                used += 1;
            }
            if used == 0 {
                break;
            }
            // 同一canonical只借这次固定栈片；文件I/O和后续Native操作不跨活RefCell租约。
            document
                .borrow()
                .copy_utf16_code_units(utf16, &mut actual[..used])
                .map_err(|cause| cause.to_string())?;
            if actual[..used] != units[..used] {
                return Err(format!("宽表canonical在{utf16}被只读流程改写"));
            }
            utf16 += used;
        }
        for expected in piece.as_bytes().chunks(bytes.len()) {
            disk.read_exact(&mut bytes[..expected.len()])
                .map_err(|cause| cause.to_string())?;
            if &bytes[..expected.len()] != expected {
                return Err("宽表原磁盘的列、文字或CRLF发生变化".into());
            }
            byte_length += expected.len();
        }
    }
    if document.borrow().utf16_len() != utf16
        || document.borrow().byte_len() != byte_length
        || disk
            .read(&mut bytes[..1])
            .map_err(|cause| cause.to_string())?
            != 0
    {
        return Err("宽表原文稿或磁盘出现遗漏/额外尾部".into());
    }
    Ok(())
}

/// 原find已完成同票同步/分步零RGBA；此处核真正发布图的目标墨迹和canonical字符框/命中。
/// 返回实际字符框供调用方与原clip滚动坐标对照，不读Grid私有字段或推算列宽。
fn wide_frame(rig: &Rig, offset: usize) -> Result<NSRect, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let rectangle = character(&*scene, offset)?;
    let current = || {
        rig.view.cpu_paint_pending() == Some(false)
            && rig
                .view
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
    };
    // find已用原三秒通知和独立RGBA对照核同一帧；此处不重复对照，改读真正挂载的CPU图。
    // 两张画布始终只覆盖当前实际viewport，二十列的整张宽表不会成为离屏图像。
    let scale = rig.panel.backingScaleFactor();
    let canvas = selection::Canvas::scaled(key.request.viewport, scale)?;
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("宽表正文没有真实backing")?;
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    // SAFETY: 原正文和主线程backing在本同步栈存活；只读取产品实际挂载关系，不构造替身layer。
    let children = unsafe { backing.sublayers() }.ok_or("宽表CPU帧没有实际挂载点")?;
    if children.len() != 1 {
        return Err("宽表有限正文没有保持唯一实际CPU挂载点".into());
    }
    let mount = children.objectAtIndex(0);
    if mount.frame() != bounds || mount.bounds() != NSRect::new(NSPoint::ZERO, bounds.size) {
        return Err("宽表CPU挂载几何偏离当前有限正文".into());
    }
    // 原mount坐标从零开始；恢复当前view bounds后才映到同一scale，不把document全宽画入内存。
    CGContext::translate_ctm(Some(&canvas.context), bounds.origin.x, bounds.origin.y);
    mount.renderInContext(&canvas.context);
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&mask.context), rectangle);
    CGContext::flush(Some(&canvas.context));
    CGContext::flush(Some(&mask.context));
    // mask只选首字符的真实框；白色表底、选区或其它可见列不能替目标字符提供黑色墨迹。
    // Canvas采用原明确RGBA8顺序，比较的是实际发布图像素，不是字体或layer存在与否。
    let ink = canvas
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels.as_chunks::<4>().0)
        .filter(|(pixel, marker)| {
            **marker == [255, 0, 0, 255] && pixel[..3].iter().all(|channel| *channel < 24)
        })
        .count();
    if ink == 0 || !current() {
        return Err("宽表目标只有选择/几何，当前已发布CPU图没有同一字符墨迹".into());
    }
    // 完整帧只代表这次视口Ready；原Composer仍须未Complete，实际排版与语义读取也未遍历整源。
    // 约1MiB未布局尾部不是高度占位或假行；后续滚动仍须按原mmap/目录继续取得真实内容。
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if progress.complete
        || progress.frontier_source_bytes >= progress.source_total_bytes
        || progress.semantic_work.source_bytes >= progress.source_total_bytes
    {
        return Err("宽表显示目标以准备全部尾部为代价，或原实际读取已遍历整篇".into());
    }
    println!(
        "native_body_table_wide target={offset} actual_canonical_rect_hit=true actual_published_cpu_ink={ink} exact_sync_async_rgba=true actual_prefix_only=true frontier_bytes={} semantic_source_bytes={} source_total_bytes={} formal=false whole_product=false",
        progress.frontier_source_bytes,
        progress.semantic_work.source_bytes,
        progress.source_total_bytes,
    );
    Ok(rectangle)
}

/// 首帧是原正文，随后原Find依次进入真实最右列与首列；原scroll的X和flag必须真的变化，
/// 关闭时在归还临时Native claim后再走原strict资源收据，不能为flag检查保活整个滚动图。
fn wide_table(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        let mut utf16 = 0;
        // 固定前文让首帧尚未进入表格，因而能在同一真实scroll上记录横向范围改变前的状态。
        // 不直接设置scroll flag、clip原点或Body高度来制造这个前提。
        for _ in 0..WIDE_LEAD_REPEATS {
            file.write_all(WIDE_LEAD.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += WIDE_LEAD.encode_utf16().count();
        }
        let header = wide_header();
        file.write_all(header.as_bytes())
            .map_err(|cause| cause.to_string())?;
        utf16 += header.encode_utf16().count();
        let first = wide_row(1);
        // 中文源位置按UTF16前缀计算，UTF8字节长度不能替代Find/输入协议的canonical单元。
        // 行模板至多二十个短格，源写入和独立偏移计算没有聚合后面的长尾。
        // 两个源位置只按固定输入模板的UTF16前缀计算，完全独立于Parser、显示映射与实际列几何。
        let left = utf16
            + first[..first.find(WIDE_LEFT).expect("首列固定唯一目标")]
                .encode_utf16()
                .count();
        let right = utf16
            + first[..first.find(WIDE_RIGHT).expect("末列固定唯一目标")]
                .encode_utf16()
                .count();
        file.write_all(first.as_bytes())
            .map_err(|cause| cause.to_string())?;
        utf16 += first.encode_utf16().count();
        for row in 2..=WIDE_ROWS {
            let text = wide_row(row);
            file.write_all(text.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += text.encode_utf16().count();
        }
        // 约1MiB尾部只按固定短片写入；产品寻找这两个表内目标不需要遍历或排版尾部。
        for _ in 0..WIDE_TAIL_REPEATS {
            file.write_all(WIDE_TAIL.as_bytes())
                .map_err(|cause| cause.to_string())?;
            utf16 += WIDE_TAIL.encode_utf16().count();
        }
        file.flush().map_err(|cause| cause.to_string())?;
        drop((header, first)); // 表头和首行输入小片也不跨真实Rig生命周期保留。
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            utf16,
            WIDE_LEAD_REPEATS * 2 + 2 + WIDE_ROWS + WIDE_TAIL_REPEATS * 2 + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let original_horizontal = rig.scroll.hasHorizontalScroller();
        let result = (|| {
            // 初始可见正文没有宽内容；在真实进入表格之前记录原scroll公开状态用于关闭归还。
            if original_horizontal {
                return Err("宽表前的原正文已经出现无内容需要的横向滚动条".into());
            }
            let initial = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let initial_clip_x = rig.scroll.contentView().bounds().origin.x;
            wide_unchanged(&rig)?;
            find(
                &mut rig,
                WIDE_RIGHT,
                right,
                false,
                MarkdownFindAction::Next,
                "table_wide_right_find",
            )?;
            let right_rectangle = wide_frame(&rig, right)?;
            let right_key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let right_clip_x = rig.scroll.contentView().bounds().origin.x;
            if !rig.scroll.hasHorizontalScroller()
                || right_clip_x <= initial_clip_x
                || right_key.request.virtual_viewport.document_origin.x
                    <= initial.request.virtual_viewport.document_origin.x
            {
                return Err("宽表最右列Find未通过原NSScrollView真实横向滚动交付".into());
            }
            // 比较实际document原点与局部字符框的和，不把“第二十列”换算成任何假定列宽。
            // 同时要求clip确已移动，单纯改Scene坐标或选择范围不能通过这次往返。
            let right_document_x =
                right_key.request.virtual_viewport.document_origin.x + right_rectangle.origin.x;
            wide_unchanged(&rig)?;
            // Previous直接回同一行首列；不绕过文件尾部搜索，也不人为设置clip或选择伪造往返。
            find(
                &mut rig,
                WIDE_LEFT,
                left,
                false,
                MarkdownFindAction::Previous,
                "table_wide_left_return",
            )?;
            let left_rectangle = wide_frame(&rig, left)?;
            let left_key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let left_clip_x = rig.scroll.contentView().bounds().origin.x;
            let left_document_x =
                left_key.request.virtual_viewport.document_origin.x + left_rectangle.origin.x;
            if !rig.scroll.hasHorizontalScroller()
                || left_clip_x >= right_clip_x
                || left_key.request.virtual_viewport.document_origin.x
                    >= right_key.request.virtual_viewport.document_origin.x
                || right_document_x - left_document_x
                    <= left_key.request.virtual_viewport.layout_width
            {
                return Err("宽表首列返回没有真实反向滚动，或固定20列未形成超视口真实宽度".into());
            }
            wide_unchanged(&rig)?;
            rig.remember_receipt()?;
            println!(
                "native_body_table_wide columns=20 rows=3 actual_right_find_left_previous=true initial_clip_x={initial_clip_x} right_clip_x={right_clip_x} returned_clip_x={left_clip_x} actual_horizontal_scroller=true source_and_disk_unchanged=true original_three_second_frame_gate=true formal=false whole_product=false"
            );
            Ok(())
        })();
        // 这份短期claim只为了读同一scroll的关闭后公开flag；退出pool前立即drop，仍由原
        // Retirement核真实Native weak nil、Source/mmap/FD/worker/字体/像素，不放宽任一关闭门。
        let scroll = Retained::clone(&rig.scroll);
        let retirement = release::close(rig, false)?;
        // 读取的是同一原scroll的公开属性。close中的正式Viewport清理必须已经归还它，
        // 测试绝不替产品设置回原值，也不会用后续窗口销毁掩盖仍未归还的flag。
        let restored = if scroll.hasHorizontalScroller() == original_horizontal {
            Ok(())
        } else {
            Err("宽表关闭没有归还原scroll的horizontal flag".to_owned())
        };
        drop(scroll);
        // 无论动作成功与否都先完成关闭；保留首次业务失败，关闭资源的真实失败由原verify处理。
        Ok((retirement, result.and(restored)))
    })?;
    retirement.verify(false)?;
    result?;
    println!(
        "native_body_table_wide horizontal_flag_restored=true strict_original_source_fd_worker_font_native_release=true physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const UI_SOURCE: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| seed | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_INSERTED: &str = "中文|a\r\nb";
const UI_FAST_COMMITTED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| sEed | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_COMMITTED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| 中文\\|a b | other |\r\n",
    "\r\nafter-ui\r\n",
);
const UI_APPENDED: &str = concat!(
    "before-ui\r\n\r\n",
    "| H | C |\r\n",
    "| --- | --- |\r\n",
    "| 中文\\|a b | other |\r\n",
    "| | |\r\n",
    "\r\nafter-ui\r\n",
);

/// 这份短表只服务真实格UI事务；固定期望独立给出CRLF、管道转义和新增行，绝不拼成长文oracle。
/// 原文件始终未保存，所有内存事务都必须保原磁盘字节；本函数没有写入或修复文档的权限。
fn ui_document(rig: &Rig, expected: &str) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("格UI原Doc已释放")?;
    let document = owner.borrow();
    if document.utf16_len() != expected.encode_utf16().count() {
        return Err(format!(
            "格UI canonical长度不符：actual={}, expected={}",
            document.utf16_len(),
            expected.encode_utf16().count()
        ));
    }
    let mut utf16 = expected.encode_utf16();
    let mut offset = 0;
    let mut units = [0_u16; 128];
    while offset < document.utf16_len() {
        let count = units.len().min(document.utf16_len() - offset);
        document
            .copy_utf16_code_units(offset, &mut units[..count])
            .map_err(|cause| cause.to_string())?;
        for unit in &units[..count] {
            if utf16.next() != Some(*unit) {
                return Err("格UI事务改变了独立期望之外的正文或CRLF".into());
            }
        }
        offset += count;
    }
    drop(document);
    drop(owner);
    // 与生产mmap独立打开真实文件；固定小块读取，不用一份内存全文掩盖未保存的数据变化。
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for original in UI_SOURCE.as_bytes().chunks(bytes.len()) {
        file.read_exact(&mut bytes[..original.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..original.len()] != original {
            return Err("格UI局部事务提前改写了原磁盘文件".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("格UI原磁盘尾部出现新增字节".into());
    }
    Ok(())
}

/// 只从原系统firstResponder核真正child，再用原CPU已发布key证明像素完成；owner getter同时是菜单实际consumer。
/// 既不从私有active槽构造场景，也不注册第二个绘图观察者覆盖Body正在完成的真实激活动作。
fn ui_active(
    rig: &mut Rig,
    previous: Option<&Rc<NativeMarkdownPagedBody>>,
    started: Instant,
    stage: &str,
) -> Result<Rc<NativeMarkdownPagedBody>, String> {
    loop {
        if let Some(cause) = rig.body.take_failure() {
            return Err(format!("{stage}: 父Body失败：{cause}"));
        }
        if let Some(cause) = rig.view.take_failure()
            && !matches!(cause, ViewError::Pending)
        {
            return Err(format!("{stage}: 父View失败：{cause}"));
        }
        if let Some(body) = rig.body.active_cell_body() {
            if let Some(cause) = body.take_failure() {
                return Err(format!("{stage}: 子Body失败：{cause}"));
            }
            let view = body.view().map_err(|cause| cause.to_string())?;
            if let Some(cause) = view.take_failure()
                && !matches!(cause, ViewError::Pending)
            {
                return Err(format!("{stage}: 子View失败：{cause}"));
            }
            let focused = rig.panel.firstResponder().is_some_and(|responder| {
                let actual: &objc2::runtime::AnyObject = responder.as_ref();
                actual
                    .downcast_ref::<NativeMarkdownPagedTextView>()
                    .is_some_and(|actual| std::ptr::eq(actual, &**view))
            });
            let changed = previous.is_none_or(|previous| !Rc::ptr_eq(previous, &body));
            if focused
                && changed
                && !body.is_preparing()
                && let Ok(key) = view.capture_ready_scene_key()
                && let Some((published, paints)) = view.probe_cpu_published_frame()
                && published.same(&key)
                && view.cpu_paints() == Some(paints)
                && view.cpu_paint_pending() == Some(false)
            {
                if key.request.target.is_body(&rig.document)
                    || view
                        .cell_history_facts()
                        .map_err(|cause| cause.to_string())?
                        .is_none()
                {
                    return Err(format!("{stage}: 系统焦点没有进入真实Cell输入域"));
                }
                // 当前真实Native子图登记进原Rig释放账本；历史格不留生产缓存，测试只保原弱观察者。
                // 临时Weak升级在这里结束，之后换格与最终close都不能被验收代码强引用遮住。
                for (role, weak) in body.native_weaks().map_err(|cause| cause.to_string())? {
                    if let Some(object) = weak.load() {
                        rig.ledger.include(role, &object)?;
                    }
                }
                if let Some(scroll) = view.enclosingScrollView() {
                    rig.ledger.include("table-ui.child-scroll", &scroll)?;
                }
                rig.receipts.push(
                    body.resource_receipts()
                        .map_err(|cause| cause.to_string())?,
                );
                println!(
                    "native_body_table_cell_ui stage={stage} actual_focused_cell=true actual_cpu_paints={paints} actual_elapsed_ms={:.3} deadline_seconds=3 whole_product=false formal=false",
                    started.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(body);
            }
        }
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or_else(|| format!("{stage}: 三秒内没有当前系统焦点中的真实Cell完整帧"))?;
        // SAFETY: 只消费同次动作剩余的原主runloop期限，真实排版/绘图消息唤醒后重核公开结果。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
}

/// 读取实际NSTextInputClient当前小草稿，范围来自独立固定期望；不借输入私态或为测试捕第二份Source。
fn ui_local(view: &NativeMarkdownPagedTextView, expected: &str) -> Result<(), String> {
    let range = NSRange::new(0, expected.encode_utf16().count());
    let mut actual = NSRange::new(objc2_foundation::NSNotFound as usize, 0);
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: 本次实际主线程client和有效out参数；只请求独立固定短夹具明确给出的有限范围。
    let value =
        unsafe { client.attributedSubstringForProposedRange_actualRange(range, &mut actual) }
            .ok_or("真实Cell client没有交付当前有限草稿")?;
    if actual != range
        || value.string().to_string() != expected
        || view
            .input_state()
            .map_err(|cause| cause.to_string())?
            .utf16_len
            != range.length
    {
        return Err(format!(
            "真实Cell client内容/长度不符：actual_range={actual:?}, expected_range={range:?}"
        ));
    }
    Ok(())
}

/// 实际鼠标进入显示格，再由系统焦点中的同一NSTextInputClient编辑；组件Rig不宣称正式workspace或整产品通过。
fn ui_flow(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow();
    if !rig.panel.isKeyWindow() {
        return Err("格UI合成窗口没有取得真实keyWindow".into());
    }
    let seed = UI_SOURCE[..UI_SOURCE.find("seed").expect("固定格UI seed存在")]
        .encode_utf16()
        .count();
    let point = links::point(rig, seed)?;
    let started = Instant::now();
    links::mouse(rig, point, NSEventType::LeftMouseDown)?;
    links::mouse(rig, point, NSEventType::LeftMouseUp)?; // Ready之前真实Up也必须正常结束Down，不复活drag。
    let first = ui_active(rig, None, started, "table_ui_mouse_cell")?;
    let view = first.view().map_err(|cause| cause.to_string())?;
    let before = view.input_state().map_err(|cause| cause.to_string())?;
    ui_local(&view, "seed")?;
    ui_document(rig, UI_SOURCE)?;
    let original_generation = rig
        .document
        .upgrade()
        .ok_or("格UI原Doc已释放")?
        .borrow()
        .generation();
    let frame = observe(&first)?;
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
    // SAFETY: 与实际键盘确认相同的Native输入协议；仅固定新增载荷，不冒充物理中文输入法候选验收。
    unsafe {
        client.insertText_replacementRange(&NSString::from_str(UI_INSERTED), NSRange::new(0, 4));
    }
    frame::wait_complete_frame("table_ui_native_edit", &first, frame)?;
    ui_local(&view, UI_INSERTED)?;
    ui_document(rig, UI_SOURCE)?;
    for (selector, expected, stage) in [
        (sel!(undo:), "seed", "table_ui_local_undo"),
        (sel!(redo:), UI_INSERTED, "table_ui_local_redo"),
    ] {
        // 原标准命令只动当前格局部历史；canonical代次、内容和原磁盘在实际完整帧后仍须不变。
        let frame = observe(&first)?;
        view.command(selector).map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &first, frame)?;
        ui_local(&view, expected)?;
        ui_document(rig, UI_SOURCE)?;
        if rig
            .document
            .upgrade()
            .ok_or("格UI原Doc已释放")?
            .borrow()
            .generation()
            != original_generation
        {
            return Err("格UI局部历史误提交了父文档".into());
        }
        if selector == sel!(undo:)
            && view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .selection
                != before.selection
        {
            return Err("格UI局部Undo没有恢复实际点按前的有向选择".into());
        }
    }
    rig.receipts.push(
        first
            .resource_receipts()
            .map_err(|cause| cause.to_string())?,
    );
    let started = Instant::now();
    view.command(sel!(insertTab:))
        .map_err(|cause| cause.to_string())?;
    let second = ui_active(rig, Some(&first), started, "table_ui_tab_next_cell")?;
    let second_view = second.view().map_err(|cause| cause.to_string())?;
    if second_view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .primary_range
        != NSRange::new(0, 5)
    {
        return Err("真实Tab没有进入下一格并全选原other".into());
    }
    ui_local(&second_view, "other")?;
    ui_document(rig, UI_COMMITTED)?;
    // 已到真实末行末格；同一Tab必须获得Core真End后追加一次，不把viewport末行冒充表格结尾。
    let started = Instant::now();
    second_view
        .command(sel!(insertTab:))
        .map_err(|cause| cause.to_string())?;
    let empty = ui_active(rig, Some(&second), started, "table_ui_terminal_append")?;
    let empty_view = empty.view().map_err(|cause| cause.to_string())?;
    if empty_view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .utf16_len
        != 0
    {
        return Err("末格Tab没有进入真实新增行首个空格".into());
    }
    ui_document(rig, UI_APPENDED)?;
    // 新空格没有改字，Esc只取消它；随后文档Undo必须一次移除追加行，再一次恢复首格原字。
    // 全部动作继续走原标准selector，不直接写Doc或手工清除活动槽来帮助资源验证。
    empty_view
        .command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    let started = Instant::now();
    while rig.body.active_cell_body().is_some() {
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("取消空格超过三秒")?;
        // SAFETY: 原主线程只等待同次取消剩余期限；没有另开线程、睡眠或替换系统焦点。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
    for (expected, stage) in [
        (UI_COMMITTED, "table_ui_append_one_undo"),
        (UI_SOURCE, "table_ui_cell_one_undo"),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        ui_document(rig, expected)?;
        rig.remember_receipt()?;
    }
    // 测试临时强claim先归还，原严格close还将核这些已换走子格的Native/FD/mmap/worker/字体与像素。
    drop((empty_view, empty, second_view, second, view, first));
    Ok(())
}

/// 只把真实鼠标事件交给实际 firstResponder 子 View；跨出后 Cocoa 仍向原接收者交 Drag/Up。
/// 这里不借父 mouse 入口改写接收者，也不以测试方法直接转移选择或手势所有权。
fn ui_child_mouse(
    rig: &Rig,
    view: &NativeMarkdownPagedTextView,
    point: NSPoint,
    kind: NSEventType,
) -> Result<(), String> {
    // 原 child 已 detach 后仍可能是 Cocoa 该手势的接收者；事件继续属于同一个原窗口。
    // 从实际 Rig 主 View 取活窗口号码，不能要求已关闭的 child 重新挂载来提供 window。
    let window = rig.view.window().ok_or("格拖选真实窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), 1.0, window.windowNumber(), None, 1, 1, 1.0,
    ).ok_or("无法构造真实格拖选鼠标事件")?;
    // SAFETY: 同一个主线程实际 Native responder 与真实窗口坐标；每个同步活事件仅交付一次。
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![view, mouseUp: &*event];
        } else {
            unreachable!("格拖选只交原 Down/Drag/Up");
        }
    }
    Ok(())
}

/// 真正编辑后的子格跨入父正文；原 Down、连续 Drag 和 Up 不跑 runloop，终点不能被异步交接丢掉。
/// 父旧 Scene 在提交前可合法 Ready，这里只验实际选择、单笔历史与源文件，不把内部阶段当用户结果。
fn ui_fast_pointer(rig: &mut Rig) -> Result<(), String> {
    let seed = UI_SOURCE[..UI_SOURCE.find("seed").expect("固定格 seed")]
        .encode_utf16()
        .count();
    let after = UI_SOURCE[..UI_SOURCE.find("after-ui").expect("固定正文尾")]
        .encode_utf16()
        .count();
    for (replace_up, stage) in [
        (false, "table_ui_fast_drag_queued_up"),
        (true, "table_ui_fast_drag_new_down_revokes_up"),
    ] {
        // 每例都从上一笔Undo后的真实原文件布局取点；等长局部修改不会伪造目标的canonical偏移。
        let first_point = links::point(rig, after + 1)?;
        let second_point = links::point(rig, after + 3)?;
        let up_point = links::point(rig, after + 6)?;
        let new_point = links::point(rig, 2)?;
        let seed_point = links::point(rig, seed)?;
        let started = Instant::now();
        links::mouse(rig, seed_point, NSEventType::LeftMouseDown)?;
        links::mouse(rig, seed_point, NSEventType::LeftMouseUp)?;
        let cell = ui_active(rig, None, started, stage)?;
        let child = cell.view().map_err(|cause| cause.to_string())?;
        let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
        {
            let document = rig.document.upgrade().ok_or("快拖原Doc已归还")?;
            if document.borrow().can_undo() {
                return Err("快拖开始前上一流程仍有未撤销的正文历史".into());
            }
        }
        // 原NSTextInputClient只改局部草稿；父文档此时不得改代、改字或提前产生正文Undo。
        let edit = observe(&cell)?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**child);
        // SAFETY: 当前系统焦点中的真实child，固定seed第2个ASCII字符与有效原替换范围。
        unsafe {
            client.insertText_replacementRange(&NSString::from_str("E"), NSRange::new(1, 1));
        }
        frame::wait_complete_frame("table_ui_fast_drag_local_edit", &cell, edit)?;
        ui_local(&child, "sEed")?;
        ui_document(rig, UI_SOURCE)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != original {
            return Err("快拖前局部输入已经改动父文档的原输入事实".into());
        }
        let key = child
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*cell, &key.request)
            .map_err(|cause| cause.to_string())?;
        let first = scene
            .first_rect(NSRange::new(1, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(1, 1) || first.rect.size.width <= 0.0 {
            return Err("编辑后的真实子格首击没有独立local字符框".into());
        }
        let down_point = child.convertPoint_toView(
            NSPoint::new(
                first.rect.origin.x + first.rect.size.width * 0.1,
                first.rect.origin.y + first.rect.size.height * 0.5,
            ),
            None,
        );
        drop((scene, key)); // 旧Scene不能由验收保活，遮住提交前的退读与后续严格关闭。
        ui_child_mouse(rig, &child, down_point, NSEventType::LeftMouseDown)?;
        let frame = observe(&rig.body)?;
        ui_child_mouse(rig, &child, first_point, NSEventType::LeftMouseDragged)?;
        ui_child_mouse(rig, &child, second_point, NSEventType::LeftMouseDragged)?;
        ui_child_mouse(rig, &child, up_point, NSEventType::LeftMouseUp)?;
        if replace_up {
            // 不等待任何提交/新帧；原父View的新Down必须撤旧Up，脏草稿仍只沿真实点击提交一次。
            links::mouse(rig, new_point, NSEventType::LeftMouseDown)?;
            links::mouse(rig, new_point, NSEventType::LeftMouseUp)?;
            ui_child_mouse(rig, &child, second_point, NSEventType::LeftMouseDragged)?;
        }
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let expected = if replace_up {
            MarkdownTextSelection::caret(2)
        } else {
            MarkdownTextSelection {
                anchor_utf16: seed + 1,
                head_utf16: after + 6,
            }
        };
        let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if actual.selection != expected || rig.body.active_cell_body().is_some() {
            return Err(format!(
                "{stage}: 真实排队交接没有保最后一次手势端点：actual={:?}, expected={expected:?}",
                actual.selection
            ));
        }
        ui_document(rig, UI_FAST_COMMITTED)?;
        {
            let document = rig.document.upgrade().ok_or("快拖提交后原Doc已归还")?;
            let document = document.borrow();
            if original.generation.checked_add(1) != Some(document.generation())
                || !document.can_undo()
                || document.can_redo()
            {
                return Err("快拖没有只提交一笔真实修改，或仍保留已被新编辑替代的Redo".into());
            }
        }
        // 原Up之后，Cocoa仍可能把迟到Drag交给旧child；它不能改变已经完成的父输入事实。
        ui_child_mouse(rig, &child, first_point, NSEventType::LeftMouseDragged)?;
        if rig.view.input_state().map_err(|cause| cause.to_string())? != actual {
            return Err("真实Up后旧child的迟到Drag复活了原手势".into());
        }
        rig.remember_receipt()?;
        drop((child, cell));
        // 选择交接不应增加正文历史；一次原Undo同时恢复完整原字节、CRLF及编辑前canonical选择。
        let undo = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame("table_ui_fast_drag_one_undo", &rig.body, undo)?;
        ui_document(rig, UI_SOURCE)?;
        let restored = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let document = rig.document.upgrade().ok_or("快拖Undo后原Doc已归还")?;
        let document = document.borrow();
        if restored.selection != original.selection || document.can_undo() || !document.can_redo() {
            return Err(format!(
                "{stage}: 一次Undo没有恢复原选择或只有一笔正文历史：actual={:?}, expected={:?}",
                restored.selection, original.selection
            ));
        }
        drop(document);
        rig.remember_receipt()?;
    }
    println!(
        "native_body_table_cell_ui fast_caret_drag_queued_handoff=true actual_dirty_cell_commit=true one_document_undo=true original_disk_unchanged=true actual_old_child_drag_and_up=true late_drag_revoked=true new_down_revokes_accepted_up=true word_cross_grid_tested=false physical_ime_tested=false formal=false whole_product=false"
    );
    Ok(())
}

fn active_table_ui(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(UI_SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            UI_SOURCE.encode_utf16().count(),
            UI_SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = ui_flow(&mut rig)
            .and_then(|()| ui_fast_pointer(&mut rig))
            .and_then(|()| cell_find::run(&mut rig));
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    result?;
    println!(
        "native_body_table_cell_ui actual_mouse_and_native_client=true local_history_parent_unchanged=true required_row_navigation=true terminal_append_one_undo=true strict_original_release=true fast_caret_drag_tested=true cross_grid_word_tested=false physical_ime_tested=false whole_product=false formal=false"
    );
    Ok(())
}
