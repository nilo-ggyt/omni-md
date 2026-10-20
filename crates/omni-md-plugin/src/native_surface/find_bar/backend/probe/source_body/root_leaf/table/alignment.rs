//! 同原短表实际列方向、软行、尾空白和活动格几何；标准 TextKit 图在产品图关闭后才创建。

use super::*;
use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;
use objc2_foundation::{NSIntersectionRect, NSIntersectsRect, NSPointInRect};
use omni_markdown_core::MarkdownTableAlignment;

const SOURCE: &str = concat!(
    "before\r\n\r\n",
    "| Left | Center | Right |\r\n",
    "| :--- | :---: | ---: |\r\n",
    "| left alpha beta gamma delta epsilon zeta eta theta iota kappa | center alpha beta gamma delta epsilon zeta eta theta iota kappa | right alpha beta gamma delta epsilon zeta eta theta iota kappa |\r\n",
    "| `L  ` | `C  ` | `R  ` |\r\n",
    "\r\nafter\r\n",
);
const CELLS: [(&str, MarkdownTableAlignment); 6] = [
    (
        "left alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Left,
    ),
    (
        "center alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Center,
    ),
    (
        "right alpha beta gamma delta epsilon zeta eta theta iota kappa",
        MarkdownTableAlignment::Right,
    ),
    ("L  ", MarkdownTableAlignment::Left),
    ("C  ", MarkdownTableAlignment::Center),
    ("R  ", MarkdownTableAlignment::Right),
];

// 此固定例特意使用 Latin LTR：每个 byte 就是一个独立 UTF16 单元，不把测试索引算法扩为
// 产品 Unicode 解码器。三种方向都含真实软换行，另一行的代码保留两个可见 trailing space。
// 内容总量不足一页；这里有限采样的许可绝不传到产品长文或整格 materialization 路径。

/// 关闭后仅留下已显示的有限字符框；方向取固定 Markdown 字面，绝不从候选 styles 推期望。
struct ObservedCell {
    text: &'static str,
    alignment: MarkdownTableAlignment,
    content: NSRect,
    clip: NSRect,
    // None 只记录真实 Pending；是否合理必须等产品严格关闭后由独立 Native 字形框判断。
    // 每个固定字符仍保原顺序，没有提前按空白、行末或候选失败过滤观察。
    characters: Vec<Option<NSRect>>,
    active: bool,
}

fn canonical(text: &str) -> usize {
    SOURCE[..SOURCE.find(text).expect("固定对齐格唯一存在")]
        .encode_utf16()
        .count()
}

/// 三列自然宽可超过真实 clip；右对齐末软行必须先经原目标定位进入横向视口。
/// 只请求固定 ASCII 字面的真实首/末 GCB，不改选区、尺寸、点击点或产品滚动策略。
fn reveal_sample(rig: &Rig, text: &str) -> Result<(), String> {
    let start = canonical(text);
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    // 固定 R␠␠ 样本的两个尾空白保留在对齐宽度之外，不能要求 SourceTarget 给离屏字形签可见。
    // 这里仍揭示原首 R；所有空白的框、Pending 与两端 caret 都在 capture/独立 Native 中逐项核。
    // 其它短例，特别是长软行末字母，继续保原首末两次真实目标定位。
    let tail = (text != "R  ").then_some(start + text.len() - 1);
    for position in std::iter::once(start).chain(tail) {
        let range = NSRange::new(position, 1);
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        match rig.view.verify_find_target_visible(&key, range) {
            Ok(_) => continue, // 已真实可见就沿同一帧读取，不为了测试制造一次多余滚动或发布。
            Err(
                crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending,
            ) => {}
            Err(cause) => return Err(cause.to_string()),
        }
        // 原 visible proof 明确未覆盖才驱动真实 SourceTarget。新 key 由产品自身滚动、布局和
        // 完整 CPU 发布取得；原三秒帧门照常生效，不能把离屏 Pending 夹成首屏假字形。
        let frame = observe(&rig.body)?;
        let target = rig
            .body
            .request_source_target(range)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame("table_aligned_sample_reveal", &rig.body, frame)?;
        let proof = rig
            .body
            .take_source_target_visible(&target)
            .map_err(|cause| cause.to_string())?
            .ok_or("对齐 sample 没有原目标的真实可见证明")?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let (published, paints) = rig
            .view
            .probe_cpu_published_frame()
            .ok_or("对齐目标没有真实完整 CPU 发布")?;
        if proof.range() != range
            || !proof.key().same(&key)
            || !published.same(&key)
            || paints == 0
            || rig.view.cpu_paints() != Some(paints)
            || rig.view.cpu_paint_pending() != Some(false)
            || rig.view.input_state().map_err(|cause| cause.to_string())? != before
        {
            return Err("对齐目标的原 proof、CPU 帧或完整输入已经改变".into());
        }
        rig.view
            .verify_find_target_visible(&key, range)
            .map_err(|cause| cause.to_string())?;
    }
    Ok(())
}

/// 每个实际字符框都须保原 canonical 范围；非空白字的墨迹左侧还必须反向命中原位置。
fn capture(
    body: &NativeMarkdownPagedBody,
    view: &NativeMarkdownPagedTextView,
    text: &'static str,
    alignment: MarkdownTableAlignment,
    active: bool,
) -> Result<ObservedCell, String> {
    let key = view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let start = if active { 0 } else { canonical(text) };
    // 活动 Source 的位置从局部零点开始，只读预览则保原 Markdown 的 canonical 端点。
    // 这两种域都只能消费当前 key；不能用父原文下标解释 child 的 raw 草稿。
    let first = scene.first_rect(NSRange::new(start, 1)).map_err(|cause| {
        format!("对齐 sample 首字：text={text:?} canonical={start} active={active}: {cause}")
    })?;
    let point = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let content = if active {
        // 实际 child 的 layout lane 与局部坐标来自原 Native 转换，不借父 clip 宽猜活动格大小。
        NSRect::new(
            NSPoint::new(
                key.request.virtual_viewport.layout_origin_x
                    - key.request.virtual_viewport.document_origin.x,
                key.request.viewport.origin.y,
            ),
            NSSize::new(
                key.request.virtual_viewport.layout_width,
                key.request.viewport.size.height,
            ),
        )
    } else {
        // 内容框只提供用户实际得到的排版 lane，方向和文字期望仍由上方固定字面决定。
        // 若已对齐的字形无法命中这张真实格票，直接失败，不去邻列寻找可通过的框。
        let hit = scene
            .cell_hit(point, false)
            .map_err(|cause| cause.to_string())?
            .ok_or("对齐字符没有落在真实格内容中")?;
        if hit.head_utf16 != start {
            return Err(format!(
                "对齐原格点击来源不符：actual={} expected={start}",
                hit.head_utf16
            ));
        }
        hit.content_frame
    };
    let clip = NSIntersectionRect(content, key.request.viewport);
    let mut characters = Vec::with_capacity(text.len());
    // 一次只记录这六个固定短串的纯值；不保 Scene/Reader/字体，也不形成生产整段目录。
    // 每个字符的 canonical 证明同时防止隐藏 backtick 把代码尾空白映到闭标记之外。
    for (local, byte) in text.bytes().enumerate() {
        let at = start + local;
        let value = match scene.first_rect(NSRange::new(at, 1)) {
            Ok(value) => value,
            Err(ViewError::Pending) => {
                // 只封存这次公开结果；关闭后若原 Native 框与 clip 相交，仍然是产品失败。
                // 零范围查询继续核两个原端点，不能因字符离屏把范围改到邻字或拆开 GCB。
                for boundary in [at, at + 1] {
                    let caret = scene
                        .first_rect(NSRange::new(boundary, 0))
                        .map_err(|cause| {
                            format!(
                                "对齐离屏字符原端点：text={text:?} canonical={boundary} active={active}: {cause}"
                            )
                        })?;
                    if caret.actual_range != NSRange::new(boundary, 0) {
                        return Err("离屏对齐字符没有保留原 canonical 光标端点".into());
                    }
                }
                characters.push(None);
                continue;
            }
            Err(cause) => {
                return Err(format!(
                    "对齐 sample 字符：text={text:?} canonical={at} active={active}: {cause}"
                ));
            }
        };
        if value.actual_range != NSRange::new(at, 1)
            || value.rect.size.width <= 0.0
            || value.rect.size.height <= 0.0
        {
            return Err(format!(
                "对齐格原字符框错误：active={active} canonical={at} actual={:?} rect={:?}",
                value.actual_range, value.rect
            ));
        }
        if byte != b' ' {
            // 空白端点可能同时属于相邻软行；此处保非空字形的独立点击侧，不能靠改点迁就命中。
            let point = NSPoint::new(
                value.rect.origin.x + value.rect.size.width * 0.125,
                value.rect.origin.y + value.rect.size.height * 0.5,
            );
            let actual = NSPointInRect(point, clip)
                .then(|| scene.hit(point))
                .transpose()
                .map_err(|cause| cause.to_string())?;
            if actual.is_some_and(|actual| actual != at) {
                return Err(format!(
                    "对齐后绘图和命中不一致：active={active} actual={actual:?} expected={at} rect={:?}",
                    value.rect
                ));
            }
        }
        characters.push(Some(value.rect));
    }
    let current = || {
        view.capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    // 同一个实际 key 的同步帧和逐项预算帧必须零 RGBA 差；baseline 只动 paint 的修补过不了
    // 上面的字符框/命中往返，也不能把一次像素完成当成后继 Scene 的绘图权限。
    selection::exact_scene_rgba(
        scene,
        &key,
        view.window().ok_or("对齐窗口已归还")?.backingScaleFactor(),
        &current,
    )?;
    Ok(ObservedCell {
        text,
        alignment,
        content,
        clip,
        characters,
        active,
    })
}

/// 只读预览、激活与取消均不得写入 canonical、历史或磁盘；完整核验也仅限固定小夹具。
fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.document.upgrade().ok_or("对齐原 Doc 已归还")?;
    let document = original.borrow();
    if document.generation() != 0
        || document.is_dirty()
        || document.can_undo()
        || document.can_redo()
    {
        return Err("对齐显示/活动格取消产生了正文或历史".into());
    }
    let mut actual = [0_u16; 128];
    // 固定小片读取只用于核这个合成例。产品用户笔记从不经过本函数，不拼接完整字符串。
    let mut expected = SOURCE.encode_utf16();
    for start in (0..document.utf16_len()).step_by(actual.len()) {
        let used = actual.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut actual[..used])
            .map_err(|cause| cause.to_string())?;
        if actual[..used]
            .iter()
            .any(|unit| expected.next() != Some(*unit))
        {
            return Err("对齐流程改写了原 Markdown 或 CRLF".into());
        }
    }
    if expected.next().is_some() || document.utf16_len() != SOURCE.encode_utf16().count() {
        return Err("对齐流程改变了原长度".into());
    }
    drop(document);
    drop(original);
    // 所有文档租约先归还，再通过独立文件句柄核原磁盘；未保存的活动格不能偷偷落盘。
    if std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())? != SOURCE.as_bytes() {
        return Err("对齐流程写入了原磁盘".into());
    }
    Ok(())
}

/// 标准短表采用向下的容器坐标；原内容框提供共同物理原点，不从候选字宽或行高拟合参考。
fn original_frame(frame: NSRect, content: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            content.origin.x + frame.origin.x,
            content.origin.y + content.size.height - frame.origin.y - frame.size.height,
        ),
        frame.size,
    )
}

/// 产品图关闭后才排独立 Native 短串，实际 NSParagraphStyle 决定对齐和尾空白语义。
fn original(observed: &[ObservedCell]) -> Result<(), String> {
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    for cell in observed {
        // 这六个固定字面只有 ASCII 字母/空格；每个 byte 是一个 scalar 和完整 GCB。
        // 端点仅从原 fixture 字面生成，不读取产品字符框、选区或 Core GCB 输出。
        if !cell.text.is_ascii() {
            return Err("固定 ASCII 对齐参考出现了非 ASCII 字面".into());
        }
        let boundaries: Vec<_> = (0..=cell.text.len()).collect();
        let lines = super::super::paragraph_oracle::measure_table_cell_graphemes(
            cell.text,
            cell.content.size.width,
            &font,
            cell.alignment,
            &boundaries,
        )?;
        if cell.text.len() > 10 && lines.len() < 2 {
            return Err("独立原对齐夹具没有真实软换行".into());
        }
        // 首行和后继每条软行都消费原 Native 的真实源范围；尾空白不从参考属性串删除。
        // Right 对齐会让保留的尾空白超出 content.right，只有独立框确在 clip 外才允许 Pending。
        let original_characters = lines.iter().map(|line| line.graphemes.len()).sum::<usize>();
        if original_characters != cell.characters.len() {
            return Err("ASCII 对齐原 Native GCB 没有覆盖全部固定字符".into());
        }
        for line in &lines {
            if line.source.is_empty() {
                continue;
            }
            let actual_line = cell
                .characters
                .get(line.source.clone())
                .ok_or("原 Native 软行范围越界")?;
            if let Some(first) = actual_line.iter().flatten().next()
                && actual_line
                    .iter()
                    .flatten()
                    .any(|character| character.origin.y != first.origin.y)
            {
                // 来源落在同一原 Native 软行的字符必须同一候选 y；只对齐第一行而后续仍左贴不能通过。
                return Err(format!(
                    "对齐格与原 Native 真实软行端点不同：alignment={:?} active={} source={:?}",
                    cell.alignment, cell.active, line.source
                ));
            }
            for grapheme in &line.graphemes {
                let [actual] = cell
                    .characters
                    .get(grapheme.source.clone())
                    .ok_or("原 Native 字符越出固定 ASCII 字面")?
                else {
                    return Err("固定 ASCII 字面不是原 Native 的单单元 GCB".into());
                };
                let positioned = grapheme
                    .positioned_frame
                    .ok_or("独立 Native GCB 没有完成逐glyph26.6定位证明")?;
                let expected = original_frame(positioned, cell.content);
                let visible = NSIntersectsRect(expected, cell.clip);
                match (visible, actual) {
                    (false, None) => {} // 这是原 Native 框证明的真实离屏，不按空白类别豁免。
                    (true, Some(actual))
                        if (actual.origin.x - expected.origin.x).abs() <= 1.0 / 64.0
                            && (actual.size.width - expected.size.width).abs() <= 1.0 / 64.0 => {}
                    _ => {
                        return Err(format!(
                            "原 Native 对齐字符与实际 clip/几何不同：alignment={:?} active={} source={:?} actual={actual:?} expected={expected:?} visible={visible} clip={:?} content={:?} original_line={:?} original_baseline={:?} raw_native={:?}",
                            cell.alignment,
                            cell.active,
                            grapheme.source,
                            cell.clip,
                            cell.content,
                            line.source,
                            line.baseline,
                            grapheme.frame,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn run_ascii(mtm: MainThreadMarker) -> Result<(), String> {
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
        let result = (|| -> Result<Vec<ObservedCell>, String> {
            let mut observed = Vec::with_capacity(8);
            for (text, alignment) in CELLS {
                reveal_sample(&rig, text)?;
                observed.push(capture(&rig.body, &rig.view, text, alignment, false)?);
            }
            rig.panel.makeKeyAndOrderFront(None);
            // 继续用真实父 mouseDown/up 挂载唯一 child，不直接创建 CellSession 或改活动槽。
            // Ready 前已发 Up；原三秒焦点/完整像素门仍由现 ui_active 负责，不能把 owner 存在当完成。
            for (text, alignment) in &CELLS[1..3] {
                reveal_sample(&rig, text)?;
                let point = links::point(&rig, canonical(text))?;
                let started = Instant::now();
                links::mouse(&rig, point, NSEventType::LeftMouseDown)?;
                links::mouse(&rig, point, NSEventType::LeftMouseUp)?;
                let child = ui_active(&mut rig, None, started, "table_aligned_active_cell")?;
                let view = child.view().map_err(|cause| cause.to_string())?;
                ui_local(&view, text)?;
                observed.push(capture(&child, &view, text, *alignment, true)?);
                // Esc 是原格的取消动作，仅关闭这次草稿；不使用应用退出或另加快捷键帮助收尾。
                let frame = observe(&rig.body)?;
                view.command(sel!(cancelOperation:))
                    .map_err(|cause| cause.to_string())?;
                frame::wait_complete_frame("table_aligned_cell_cancel", &rig.body, frame)?;
                if rig.body.active_cell_body().is_some() {
                    return Err("对齐格取消后仍保活动 owner".into());
                }
                drop(view);
                drop(child);
                // 短期强引用在下一格开始前归还；最终严格关闭仍核原 Native/FD/mmap/字体/worker。
                unchanged(&rig)?;
            }
            rig.remember_receipt()?;
            unchanged(&rig)?;
            Ok(observed)
        })();
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    // 即使业务动作失败也先走上方真实关闭；只有整个产品图退场后才分配标准参考图。
    // 两图永不同存，记录仅是可见字符框与方向纯值，不能掩盖产品资源延迟释放。
    original(&result?)?;
    println!(
        "native_body_table_alignment actual_left_center_right=true actual_soft_lines_and_trailing_spaces=true active_cell_geometry=true canonical_hit_and_zero_rgba=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}

const UNICODE_SOURCE: &str = concat!(
    "before\r\n\r\n",
    "| Center | Right |\r\n",
    "| :---: | ---: |\r\n",
    "| `אבג  ` | `דהו  ` |\r\n",
    "| `x\u{0600}  ` | `y\u{0600}  ` |\r\n",
    "\r\nafter\r\n",
);

/// 固定 Hebrew 与 Prepend 字面直接给出真实 GCB 端点；不是复制产品 Unicode 解码或分词算法。
/// U+0600 与其后第一个空格为整簇，后一个独立空格仍可退出对齐宽度；任何命中都不能切簇。
const UNICODE_CELLS: [(&str, MarkdownTableAlignment, &[usize]); 4] = [
    ("אבג  ", MarkdownTableAlignment::Center, &[0, 1, 2, 3, 4, 5]),
    ("דהו  ", MarkdownTableAlignment::Right, &[0, 1, 2, 3, 4, 5]),
    ("x\u{0600}  ", MarkdownTableAlignment::Center, &[0, 1, 3, 4]),
    ("y\u{0600}  ", MarkdownTableAlignment::Right, &[0, 1, 3, 4]),
];

struct UnicodeCell {
    text: &'static str,
    alignment: MarkdownTableAlignment,
    content: NSRect,
    clip: NSRect,
    graphemes: Vec<(std::ops::Range<usize>, Option<NSRect>)>,
}

/// 查找首标量仍沿正式 SourceTarget；内部 UBA seam 不能让原完整 GCB 的可见证明失败。
fn reveal_unicode_grapheme(rig: &Rig, text: &str) -> Result<(), String> {
    let Some(within) = text.find('\u{0600}') else {
        return Ok(()); // 普通 Hebrew 样本继续只验证其原几何，没有额外目标动作。
    };
    let canonical = UNICODE_SOURCE[..UNICODE_SOURCE.find(text).expect("固定 Unicode 格存在")]
        .encode_utf16()
        .count()
        + text[..within].encode_utf16().count();
    let range = NSRange::new(canonical, 1);
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(range)
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("table_prepend_original_find_target", &rig.body, frame)?;
    let proof = rig
        .body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("Prepend 原首标量没有真实可见证明")?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let (published, paints) = rig
        .view
        .probe_cpu_published_frame()
        .ok_or("Prepend 原目标没有完整 CPU 帧")?;
    if proof.range() != range
        || !proof.key().same(&key)
        || !published.same(&key)
        || paints == 0
        || rig.view.cpu_paints() != Some(paints)
        || rig.view.cpu_paint_pending() != Some(false)
        || rig.view.input_state().map_err(|cause| cause.to_string())? != before
    {
        return Err("Prepend 原目标与完整输入、实际 CPU 发布不一致".into());
    }
    rig.view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    Ok(())
}

/// 只通过原 Scene 的公开字符框、光标、命中和有向选择消费同一真实格；不读内部布局字段。
fn unicode_cell(
    rig: &Rig,
    text: &'static str,
    alignment: MarkdownTableAlignment,
    boundaries: &[usize],
) -> Result<UnicodeCell, String> {
    let canonical = UNICODE_SOURCE[..UNICODE_SOURCE.find(text).expect("固定 Unicode 格存在")]
        .encode_utf16()
        .count();
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    // 首 GCB 仍须实际可见并命中当前格，才取得真实内容框；不从列号或候选字宽猜 clip。
    let first = scene
        .first_rect(NSRange::new(canonical, boundaries[1]))
        .map_err(|cause| cause.to_string())?;
    let first_point = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.125,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    let content = scene
        .cell_hit(first_point, false)
        .map_err(|cause| cause.to_string())?
        .ok_or("Unicode 对齐首字没有命中原真实格")?
        .content_frame;
    let clip = NSIntersectionRect(content, key.request.viewport);
    let mut graphemes = Vec::new();
    for pair in boundaries.windows(2) {
        let from = canonical + pair[0];
        let to = canonical + pair[1];
        let value = match scene.first_rect(NSRange::new(from, to - from)) {
            Ok(value) => value,
            Err(ViewError::Pending) => {
                // 离屏不授字形框，但两个公开插入点仍须保原完整 GCB 的 canonical 端点。
                // None 会在严格关闭后与真实 Native 框逐项核对，可见却 Pending 不能通过。
                for boundary in [from, to] {
                    let caret = scene
                        .first_rect(NSRange::new(boundary, 0))
                        .map_err(|cause| {
                            format!(
                                "Unicode 对齐离屏 GCB 原端点：text={text:?} source={from}..{to} caret={boundary}: {cause}"
                            )
                        })?;
                    if caret.actual_range != NSRange::new(boundary, 0) {
                        return Err("Unicode 离屏 GCB 没有保留原 canonical 光标端点".into());
                    }
                }
                graphemes.push((pair[0]..pair[1], None));
                continue;
            }
            Err(cause) => {
                return Err(format!(
                    "Unicode 对齐字符框：text={text:?} source={from}..{to}: {cause}"
                ));
            }
        };
        if value.actual_range != NSRange::new(from, to - from)
            || value.rect.size.width <= 0.0
            || value.rect.size.height <= 0.0
        {
            return Err(format!(
                "Unicode 对齐格没有原完整 GCB 框：text={text:?} canonical={from}..{to} actual={:?} rect={:?}",
                value.actual_range, value.rect,
            ));
        }
        let start = scene.first_rect(NSRange::new(from, 0)).map_err(|cause| {
            format!("Unicode 对齐起点：text={text:?} source={from}..{to}: {cause}")
        })?;
        let end = scene.first_rect(NSRange::new(to, 0)).map_err(|cause| {
            format!("Unicode 对齐末点：text={text:?} source={from}..{to}: {cause}")
        })?;
        if start.actual_range != NSRange::new(from, 0)
            || end.actual_range != NSRange::new(to, 0)
            || (start.rect.origin.x.min(end.rect.origin.x) - value.rect.origin.x).abs() > 1.0 / 64.0
            || ((start.rect.origin.x - end.rect.origin.x).abs() - value.rect.size.width).abs()
                > 1.0 / 64.0
        {
            return Err(format!(
                "Unicode 对齐的整簇光标和字符框不自洽：text={text:?} source={from}..{to} rect={:?} start_range={:?} start={:?} end_range={:?} end={:?}",
                value.rect, start.actual_range, start.rect, end.actual_range, end.rect,
            ));
        }
        let mut sides = [None; 2];
        for (index, fraction) in [0.125, 0.875].into_iter().enumerate() {
            let point = NSPoint::new(
                value.rect.origin.x + value.rect.size.width * fraction,
                value.rect.origin.y + value.rect.size.height * 0.5,
            );
            if !NSPointInRect(point, clip) {
                // 部分可见 GCB 的原固定点击点可能在邻格；保留原点，不移动到可通过的位置。
                // 关闭后的独立框仍须与实际框精确对应，只有真实 clip 内的点才属于本格交互。
                continue;
            }
            let hit = scene.hit(point).map_err(|cause| {
                format!(
                    "Unicode 对齐点击：text={text:?} source={from}..{to} point={point:?}: {cause}"
                )
            })?;
            if hit != from && hit != to {
                return Err(format!(
                    "Unicode 对齐点击切开真实 GCB：text={text:?} range={from}..{to} hit={hit} point={point:?}",
                ));
            }
            let (selection, _) = scene
                .mouse_selection(point, 1, Some(from), true)
                .map_err(|cause| cause.to_string())?;
            if selection.anchor_utf16 != from || selection.head_utf16 != hit {
                return Err("Unicode 对齐有向选择没有保持真实 GCB 锚点与端点".into());
            }
            sides[index] = Some(hit);
        }
        if let [Some(left), Some(right)] = sides
            && left == right
        {
            return Err("Unicode 对齐整簇的两个真实点击侧没有得到不同端点".into());
        }
        graphemes.push((pair[0]..pair[1], Some(value.rect)));
    }
    let current = || {
        rig.view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
    };
    selection::exact_scene_rgba(scene, &key, rig.panel.backingScaleFactor(), &current)?;
    Ok(UnicodeCell {
        text,
        alignment,
        content,
        clip,
        graphemes,
    })
}

/// 短例只在原 mmap 文档图中读取/选择；参考 TextKit 图必须等严格关闭完成后才创建。
fn run_unicode(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        file.write_all(UNICODE_SOURCE.as_bytes())
            .map_err(|cause| cause.to_string())?;
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            UNICODE_SOURCE.encode_utf16().count(),
            UNICODE_SOURCE.bytes().filter(|byte| *byte == b'\n').count() + 1,
            false,
            MarkdownPresentationMode::InlinePreview,
            true,
        )?;
        let result = (|| -> Result<Vec<UnicodeCell>, String> {
            let mut observed = Vec::new();
            for (text, alignment, boundaries) in UNICODE_CELLS {
                reveal_unicode_grapheme(&rig, text)?;
                observed.push(unicode_cell(&rig, text, alignment, boundaries)?);
            }
            let original = rig.document.upgrade().ok_or("Unicode 对齐原 Doc 已归还")?;
            let document = original.borrow();
            if document.generation() != 0
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
                || document.utf16_len() != UNICODE_SOURCE.encode_utf16().count()
            {
                return Err("Unicode 对齐读取/选择改变了正文或历史".into());
            }
            let mut expected = UNICODE_SOURCE.encode_utf16();
            let mut units = [0_u16; 64];
            for offset in (0..document.utf16_len()).step_by(units.len()) {
                let used = units.len().min(document.utf16_len() - offset);
                document
                    .copy_utf16_code_units(offset, &mut units[..used])
                    .map_err(|cause| cause.to_string())?;
                if units[..used]
                    .iter()
                    .any(|unit| expected.next() != Some(*unit))
                {
                    return Err("Unicode 对齐选择改写了原字符或 CRLF".into());
                }
            }
            drop(document);
            drop(original);
            if expected.next().is_some()
                || std::fs::read(rig._file.path()).map_err(|cause| cause.to_string())?
                    != UNICODE_SOURCE.as_bytes()
            {
                return Err("Unicode 对齐选择改写了磁盘或源长度".into());
            }
            rig.remember_receipt()?;
            Ok(observed)
        })();
        let retirement = release::close(rig, false)?;
        Ok((retirement, result))
    })?;
    retirement.verify(false)?;
    let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
    for cell in result? {
        // 原 UNICODE_CELLS 已独立声明 UAX GCB：Prepend+首空格为1..3，尾空格仍为3..4。
        // 这里只按固定字面/方向找回该声明，不能从待测 cell.graphemes 反推 Native 查询范围。
        let boundaries = UNICODE_CELLS
            .iter()
            .find_map(|(text, alignment, boundaries)| {
                (*text == cell.text && *alignment == cell.alignment).then_some(*boundaries)
            })
            .ok_or("Unicode 对齐观察没有对应的原固定 GCB 字面")?;
        let lines = super::super::paragraph_oracle::measure_table_cell_graphemes(
            cell.text,
            cell.content.size.width,
            &font,
            cell.alignment,
            boundaries,
        )?;
        let original: Vec<_> = lines.iter().flat_map(|line| &line.graphemes).collect();
        if original.len() != cell.graphemes.len() {
            return Err(format!(
                "Unicode 对齐原 Native GCB 与固定字面不同：text={:?} actual_count={} original_count={}",
                cell.text,
                cell.graphemes.len(),
                original.len(),
            ));
        }
        for ((source, actual), original) in cell.graphemes.iter().zip(original) {
            // 两种引擎都来自同一短字面、真实同列宽和原 NSParagraphStyle；RTL 逐 GCB 比真实
            // 视觉左右，不能把逻辑首字直接贴 baseline，也不在参考侧 trim 掉 Prepend 整簇。
            if *source != original.source {
                return Err("Unicode 对齐观察与独立 Native 的原 GCB 端点不同".into());
            }
            let positioned = original
                .positioned_frame
                .ok_or("独立 Native Unicode GCB 没有完成逐glyph26.6定位证明")?;
            let expected = original_frame(positioned, cell.content);
            let visible = NSIntersectsRect(expected, cell.clip);
            match (visible, actual) {
                (false, None) => {} // 原字形确在 clip 外；保留该 GCB，不把末空白从结果删除。
                (true, Some(actual))
                    if (actual.origin.x - expected.origin.x).abs() <= 1.0 / 64.0
                        && (actual.size.width - expected.size.width).abs() <= 1.0 / 64.0 => {}
                _ => {
                    return Err(format!(
                        "Unicode 真实表格对齐与 Native clip/几何不同：text={:?} alignment={:?} source={source:?} actual={actual:?} original_source={:?} expected={expected:?} visible={visible} clip={:?} content={:?} raw_native={:?}",
                        cell.text,
                        cell.alignment,
                        original.source,
                        cell.clip,
                        cell.content,
                        original.frame,
                    ));
                }
            }
        }
    }
    println!(
        "native_body_table_alignment_unicode actual_rtl_trailing_spaces=true actual_prepend_whitespace_whole_gcb=true native_grapheme_geometry=true canonical_caret_hit_selection=true strict_original_release=true whole_product=false formal=false"
    );
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    // 两个独立真实图都完整执行并关闭；ASCII 首次失败不能遮住 RTL/Prepend 的资源或功能结果。
    let ascii = run_ascii(mtm);
    let unicode = run_unicode(mtm);
    match (ascii, unicode) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(ascii), Err(unicode)) => Err(format!("{ascii}; {unicode}")),
    }
}
