//! 同一长文旧sealed窗口的真实单击回查与取消；片段目标只核解析/消费，不冒充内部anchor导航。

use super::*;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use std::io::Read;

const PREFIX: &str = "outside\n\n[cached](#cached)\n\n";
const PARAGRAPH: &str =
    "bounded viewport paragraph text keeps the original source and stable native geometry. ";
const LABEL: usize = 10;

/// 每段不足4KiB且逐片写文件；夹具不先拼出全文String，生产仍只打开原mmap文档。
fn fixture() -> Result<(tempfile::NamedTempFile, usize, usize), String> {
    let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
    file.write_all(PREFIX.as_bytes())
        .map_err(|cause| cause.to_string())?;
    for _ in 0..48 {
        for _ in 0..6 {
            file.write_all(PARAGRAPH.as_bytes())
                .map_err(|cause| cause.to_string())?;
        }
        file.write_all(b"\n\n").map_err(|cause| cause.to_string())?;
    }
    file.flush().map_err(|cause| cause.to_string())?;
    Ok((file, PREFIX.len() + 48 * (PARAGRAPH.len() * 6 + 2), 101))
}

/// 原Source目标只滚动，不改选择；真实三秒完整帧与唯一Reader证明都来自原Body。
fn reveal(rig: &mut Rig, stage: &str, index: usize) -> Result<(), String> {
    let frame = observe(&rig.body)?;
    let target = rig
        .body
        .request_source_target(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    wait_frame(stage, &rig.body, frame)?;
    rig.body
        .take_source_target_visible(&target)
        .map_err(|cause| cause.to_string())?
        .ok_or("原链接夹具Source目标没有真实可见证明")?;
    rig.remember_receipt()
}

/// 只取当前真实GCB的窗口点；旧Scene在系统事件交付前归还，不占第二份prefix读取。
pub(super) fn point(rig: &Rig, index: usize) -> Result<NSPoint, String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let first = scene
        .first_rect(NSRange::new(index, 1))
        .map_err(|cause| cause.to_string())?;
    if first.actual_range != NSRange::new(index, 1) || first.rect.size.width <= 0.0 {
        return Err("原链接字符没有真实单元几何".into());
    }
    let local = NSPoint::new(
        first.rect.origin.x + first.rect.size.width * 0.25,
        first.rect.origin.y + first.rect.size.height * 0.5,
    );
    Ok(rig.view.convertPoint_toView(local, None))
}

/// 普通指针沿现有公开事件进入正文，用于定位和拖选；不预先解析或打开目标。
pub(super) fn mouse(rig: &Rig, point: NSPoint, kind: NSEventType) -> Result<(), String> {
    mouse_with_modifiers(rig, point, kind, NSEventModifierFlags::empty())
}

/// 链接激活使用产品约定的 Command 单击；仍由原 NSResponder 收到真实修饰键和窗口坐标。
pub(super) fn command_mouse(rig: &Rig, point: NSPoint, kind: NSEventType) -> Result<(), String> {
    mouse_with_modifiers(rig, point, kind, NSEventModifierFlags::Command)
}

/// 共用原三种公开鼠标事件的交付，普通点击与链接点击只在系统修饰键事实不同。
fn mouse_with_modifiers(
    rig: &Rig,
    point: NSPoint,
    kind: NSEventType,
    modifiers: NSEventModifierFlags,
) -> Result<(), String> {
    let window = rig.view.window().ok_or("原链接窗口已归还")?;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, modifiers, 1.0, window.windowNumber(), None, 1, 1, 1.0,
    ).ok_or("无法构造原链接鼠标事件")?;
    // SAFETY: 原主线程NSResponder、真实窗口点和同步活事件；没有虚构selector或跨线程对象。
    unsafe {
        if kind == NSEventType::LeftMouseDown {
            let _: () = objc2::msg_send![&**rig.view, mouseDown: &*event];
        } else if kind == NSEventType::LeftMouseDragged {
            let _: () = objc2::msg_send![&**rig.view, mouseDragged: &*event];
        } else if kind == NSEventType::LeftMouseUp {
            let _: () = objc2::msg_send![&**rig.view, mouseUp: &*event];
        } else {
            unreachable!("本夹具只交Down/Drag/Up")
        }
    }
    Ok(())
}

/// 此操作保持原完整帧，不会再签新paint通知；按真实调度/Font状态消费主runloop，仍受同一三秒限制。
pub(super) fn settle(rig: &Rig) -> Result<Option<ViewError>, String> {
    let started = Instant::now();
    while rig.body.is_preparing() {
        let remaining = Duration::from_secs(3)
            .checked_sub(started.elapsed())
            .ok_or("原链接回查超过三秒")?;
        // SAFETY: 只处理原default mode的实际待办；没有sleep、后台轮询或延长原异步操作期限。
        unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, remaining.as_secs_f64(), true) };
    }
    if let Some(cause) = rig.body.take_failure() {
        return Err(format!("原链接Body失败：{cause}"));
    }
    Ok(rig.view.take_failure()) // 原typed结果交调用者逐种核，不能把预期的anchor明确拒绝当成导航成功。
}

/// 未恢复的旧cache必须明确Pending；不能凭LINK位或旧字符串目录猜target。
fn pending_target(rig: &Rig) -> Result<(), String> {
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    let window_point = point(rig, LABEL)?;
    let local = rig.view.convertPoint_fromView(window_point, None);
    if scene
        .link_hit(local)
        .map_err(|cause| cause.to_string())?
        .is_none()
        || !matches!(scene.link_target(local), Err(ViewError::Pending))
    {
        return Err("旧sealed链接未复现真实可见命中与当前语义窗外Pending".into());
    }
    Ok(())
}

/// Source前缀回溯不改原选择/字体图；一次mouseup即可驱动真恢复，后继输入和失焦先到则撤旧。
fn verify(rig: &mut Rig) -> Result<(), String> {
    rig.panel.makeKeyWindow(); // 直接NSResponder夹具不会走NSApplication.sendEvent，明确恢复真实用户点击前的键盘窗口条件。
    if !rig.panel.isKeyWindow() {
        return Err("链接真实鼠标夹具没有取得原keyWindow，不能冒充已获点击焦点".into());
    }
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    reveal(rig, "root_leaf_link_far_prefix", 20_001)?;
    reveal(rig, "root_leaf_link_cached_top", LABEL)?;
    pending_target(rig)?;
    let cached = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let before = rig.body.progress().map_err(|cause| cause.to_string())?;
    if cached.source != initial.source || cached.request.input != initial.request.input {
        return Err("链接旧cache准备改变原Source或canonical选择".into());
    }
    let press = point(rig, LABEL)?;
    for focus in [false, true] {
        command_mouse(rig, press, NSEventType::LeftMouseDown)?;
        command_mouse(rig, press, NSEventType::LeftMouseUp)?;
        if focus {
            rig.panel.makeFirstResponder(Some(&*rig.bar.query));
        } else {
            rig.view
                .command(sel!(copy:))
                .map_err(|cause| cause.to_string())?;
        }
        if let Some(cause) = settle(rig)? {
            return Err(format!("已取消的click仍报告失败：{cause}"));
        }
        pending_target(rig)?;
        if rig
            .body
            .progress()
            .map_err(|cause| cause.to_string())?
            .semantic_work
            != before.semantic_work
        {
            return Err("后继菜单输入或失焦未撤旧click，仍推进了原链接来源".into());
        }
        rig.panel.makeFirstResponder(Some(&**rig.view));
    }
    // 松开 Command 再抬起鼠标必须取消激活，保持 canonical 选择和旧 cache 的真实 Pending。
    command_mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, press, NSEventType::LeftMouseUp)?;
    if let Some(cause) = settle(rig)? {
        return Err(format!("已松开 Command 的链接点击仍报告失败：{cause}"));
    }
    pending_target(rig)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != cached.request.input {
        return Err("取消链接跟随改变了原 canonical 输入".into());
    }
    command_mouse(rig, press, NSEventType::LeftMouseDown)?;
    if rig.view.input_state().map_err(|cause| cause.to_string())? != cached.request.input {
        return Err("原链接mousedown在确认单击前改写选择或抢走拖选".into());
    }
    command_mouse(rig, press, NSEventType::LeftMouseUp)?;
    if !matches!(settle(rig)?, Some(ViewError::LinkAnchorUnsupported)) {
        return Err("真实片段链接没有明确报告尚无稳定anchor导航".into());
    }
    let recovered = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if !recovered.same(&cached) {
        return Err("旧cache链接回查另建了Scene/Source/排版图".into());
    }
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &cached.request)
        .map_err(|cause| cause.to_string())?;
    let local = rig.view.convertPoint_fromView(press, None);
    if scene
        .link_target(local)
        .map_err(|cause| cause.to_string())?
        .as_deref()
        != Some("#cached")
    {
        return Err("第一次真实mouseup未恢复原Source解析的片段target".into());
    }
    drop(scene);
    // 标签上的普通单击必须真的把 caret 放进标签，不请求导航，也不返回片段拒绝错误。
    let frame = observe(&rig.body)?;
    mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, press, NSEventType::LeftMouseUp)?;
    wait_frame("root_leaf_link_plain_click_caret", &rig.body, frame)?;
    if let Some(cause) = settle(rig)? {
        return Err(format!("普通标签单击不应触发链接：{cause}"));
    }
    let positioned = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if positioned.generation != cached.request.input.generation
        || positioned.primary_range != NSRange::new(LABEL, 0)
        || positioned.marked.is_some()
    {
        return Err("普通标签单击没有定位到真实字符，或改变了正文与组合输入".into());
    }
    // 选择显露标记可能局部重排；拖选使用同一标签现在的真实点，不能借旧几何冒充命中。
    let press = point(rig, LABEL)?;
    // 随后真实拖动仍走原caret选择；即使从链接开始，也不能在mousedown提前激活外部目标。
    let frame = observe(&rig.body)?;
    let end = point(rig, PREFIX.len() + 12)?;
    mouse(rig, press, NSEventType::LeftMouseDown)?;
    mouse(rig, end, NSEventType::LeftMouseDragged)?;
    mouse(rig, end, NSEventType::LeftMouseUp)?;
    wait_frame("root_leaf_link_original_drag_selection", &rig.body, frame)?;
    let dragged = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if dragged.generation != cached.request.input.generation
        || dragged.primary_range.length == 0
        || dragged.primary_range.location > LABEL
    {
        return Err("链接开始的原拖动未保留真实连续选区及原正文generation".into());
    }
    rig.remember_receipt()?;
    println!(
        "native_body_root_leaf_link cached_scene_real_pending=true first_mouseup_restored_target=true anchor_typed_rejection=true command_click=true released_command_cancelled=true plain_click_caret=true successor_menu_and_focus_revoked=true original_drag_selection=true original_source=true external_url_open_tested=false internal_anchor_navigation_tested=false whole_product=false formal=false"
    );
    Ok(())
}

const DIRECT_HEAD: &str = "outside `code` **strong** *italic* ***both*** ";
const DIRECT_PADDING: &str = "plain paragraph keeps the same original source and native viewport. ";
const DIRECT_PREFIX_REPEATS: usize = 1_024;
const DIRECT_LABEL: &str = "native link label ";
const DIRECT_LABEL_REPEATS: usize = 192;
// 期望 URI 来自原固定 Markdown 的转义和实体语义：括号去反斜线，amp 实体还原。
// 它不从 Core 已解析目标、点击结果或当前 Scene 回填；末尾 fragment 亦须完整保留。
const DIRECT_CLOSE: &str = "](https://example.com/a\\(b\\)?first=1&amp;last=2#native-long) ";
const DIRECT_URI: &str = "https://example.com/a(b)?first=1&last=2#native-long";
const DIRECT_TAIL_REPEATS: usize = 256;
const DIRECT_LABEL_START: usize =
    DIRECT_HEAD.len() + DIRECT_PADDING.len() * DIRECT_PREFIX_REPEATS + 1;
const DIRECT_LABEL_UNITS: usize = DIRECT_LABEL.len() * DIRECT_LABEL_REPEATS;
const DIRECT_UNITS: usize = DIRECT_LABEL_START
    + DIRECT_LABEL_UNITS
    + DIRECT_CLOSE.len()
    + DIRECT_PADDING.len() * DIRECT_TAIL_REPEATS;

/// 所有片段均为固定 ASCII，字节数就是独立 canonical UTF16 长度；没有换行，始终是
/// 一个超过 64KiB 的 Root 段落。生成器只逐片借字面，不先聚合全文 String 或 attributed 图。
fn direct_segments() -> impl Iterator<Item = &'static str> {
    std::iter::once(DIRECT_HEAD)
        .chain(std::iter::repeat_n(DIRECT_PADDING, DIRECT_PREFIX_REPEATS))
        .chain(std::iter::once("["))
        .chain(std::iter::repeat_n(DIRECT_LABEL, DIRECT_LABEL_REPEATS))
        .chain(std::iter::once(DIRECT_CLOSE))
        .chain(std::iter::repeat_n(DIRECT_PADDING, DIRECT_TAIL_REPEATS))
}

/// 导航和 URI 查询只能读取原源；全文逐个固定栈片核 canonical 与独立文件句柄，
/// 同时保空历史、原 generation 和磁盘身份，不能靠删语法、截尾或保存后的新文件过门。
fn direct_unchanged(rig: &Rig) -> Result<(), String> {
    let owner = rig.document.upgrade().ok_or("长直接链接原 Doc 已归还")?;
    let document = owner.borrow();
    if document.generation() != 0
        || document.utf16_len() != DIRECT_UNITS
        || document.byte_len() != DIRECT_UNITS
        || document.selection() != MarkdownTextSelection::caret(0)
        || document.can_undo()
        || document.can_redo()
        || document.is_dirty()
        || !document.original_file_identity_is_unchanged()
    {
        return Err("长直接链接只读导航改变了原文稿、选择、历史或磁盘身份".into());
    }
    let mut expected = direct_segments().flat_map(str::bytes);
    let mut units = [0_u16; 256];
    for start in (0..DIRECT_UNITS).step_by(units.len()) {
        let used = units.len().min(DIRECT_UNITS - start);
        document
            .copy_utf16_code_units(start, &mut units[..used])
            .map_err(|cause| cause.to_string())?;
        if units[..used]
            .iter()
            .any(|actual| expected.next().map(u16::from) != Some(*actual))
        {
            return Err("长直接链接显示改变了原 canonical 字符".into());
        }
    }
    if expected.next().is_some() {
        return Err("长直接链接原 canonical 尾部未完整保留".into());
    }
    drop(document);
    drop(owner); // 原文稿借用和临时强边先归还，文件验证不跨任何 Native 或 runloop。
    let mut file = std::fs::File::open(rig._file.path()).map_err(|cause| cause.to_string())?;
    let mut bytes = [0_u8; 128];
    for segment in direct_segments() {
        file.read_exact(&mut bytes[..segment.len()])
            .map_err(|cause| cause.to_string())?;
        if &bytes[..segment.len()] != segment.as_bytes() {
            return Err("长直接链接只读流程改变了原磁盘字节".into());
        }
    }
    if file
        .read(&mut bytes[..1])
        .map_err(|cause| cause.to_string())?
        != 0
    {
        return Err("长直接链接原磁盘出现额外尾部".into());
    }
    Ok(())
}

/// 同一长物理行的代码和粗斜体先进入真实封存帧，再沿原 SourceTarget 滚动至链接首中尾。
/// 每次只借当前 Scene 的实际字符框、hit 与 link_target；不点击外链、不调用 NSWorkspace，
/// 也不借已知 URI 去补当前尚未具备的语义权限。任一 Pending 都是本用例的真实失败。
fn verify_direct(rig: &mut Rig) -> Result<(), String> {
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if rig.backing.mapped_bytes() == 0 {
        return Err("长直接链接没有保持原真实源映射".into());
    }
    let parts = rig
        .body
        .probe_root_leaf_parts()
        .map_err(|cause| cause.to_string())?;
    // 只借原首屏已经实际 sealed 的字体/样式事实，不能为检查远端链接先索取整篇 parts。
    // 固定 head 的四个字面范围给出独立期望；角色必须随真正 glyph 发布而非仅 Parser 分词。
    for (literal, styles) in [
        ("code", MarkdownPresentationStyles::INLINE_CODE),
        ("strong", MarkdownPresentationStyles::STRONG),
        ("italic", MarkdownPresentationStyles::EMPHASIS),
        ("both", MarkdownPresentationStyles::STRONG),
        ("both", MarkdownPresentationStyles::EMPHASIS),
    ] {
        let start = DIRECT_HEAD.find(literal).expect("固定长段落保原样式字面");
        for index in start..start + literal.len() {
            if parts
                .iter()
                .find(|part| part.source.contains(&index))
                .is_none_or(|part| !part.presentation.contains(styles))
            {
                return Err(format!(
                    "长直接链接前缀的真实封存样式缺失：source={index} expected={styles:?}"
                ));
            }
        }
    }
    drop(parts); // 字体弱观察只核本帧；后继导航不保存整段 Font/Source 图或第二份行目录。
    direct_unchanged(rig)?;
    let mut previous: Option<NativeMarkdownPagedSceneKey> = None;
    for (stage, index) in [
        ("root_leaf_long_direct_link_start", DIRECT_LABEL_START),
        (
            "root_leaf_long_direct_link_middle",
            DIRECT_LABEL_START + DIRECT_LABEL_UNITS / 2,
        ),
        (
            "root_leaf_long_direct_link_end",
            DIRECT_LABEL_START + DIRECT_LABEL_UNITS - 2,
        ),
    ] {
        // label 横跨数个真实 viewport；原首、中、尾字面位置独立于候选 run/可见范围。
        // 先登记原完整 CPU 通知再请求滚动，三秒内还须消费同一次 SourceTarget 可见证明。
        reveal(rig, stage, index)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        // 正常 SourceTarget 不编辑选择；跨窗口只换真实几何。旧 key 即使弱对象仍活，
        // 也必须被当前 Body 撤权，不能凭同文稿或相同 URI 将上一帧复用成这次成功。
        if key.source != initial.source
            || key.request.input != initial.request.input
            || previous.as_ref().is_some_and(|previous| {
                key.same(previous)
                    || key.request.virtual_viewport.document_origin
                        == previous.request.virtual_viewport.document_origin
                    || NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, previous)
            })
        {
            return Err("长链接跨窗口导航没有保原输入、Source或撤销旧几何".into());
        }
        let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
            .map_err(|cause| cause.to_string())?;
        let first = scene
            .first_rect(NSRange::new(index, 1))
            .map_err(|cause| cause.to_string())?;
        if first.actual_range != NSRange::new(index, 1)
            || first.rect.size.width <= 0.0
            || first.rect.size.height <= 1.0
        {
            return Err(format!("{stage}: 原标签字符没有完整非空 canonical 框"));
        }
        let local = NSPoint::new(
            first.rect.origin.x + first.rect.size.width * 0.25,
            first.rect.origin.y + first.rect.size.height * 0.5,
        );
        // 固定 ASCII label 的内部四分之一点应命中原左端 canonical 单元。查询沿原
        // Scene 的 Source/publication/完整 Input 许可进行；不直接调 Core 来替 Native 过门。
        if scene.hit(local).map_err(|cause| cause.to_string())? != index
            || scene
                .link_target(local)
                .map_err(|cause| cause.to_string())?
                .as_deref()
                != Some(DIRECT_URI)
        {
            return Err(format!(
                "{stage}: 长标签真实命中没有返回原转义和实体规范化后的完整 URI"
            ));
        }
        drop(scene); // Reader/Font/Source 的同步借用在下个原生导航与严格关闭前结束。
        previous = Some(key);
        direct_unchanged(rig)?;
    }
    rig.remember_receipt()?;
    println!(
        "native_root_long_direct_link original_root_bytes={DIRECT_UNITS} original_label_units={DIRECT_LABEL_UNITS} mapped_source=true code_strong_emphasis_combined=true actual_scene_hit_uri_start_middle_end=true native_viewport_navigation=true normalized_complete_uri=true canonical_disk_history_unchanged=true external_url_open_tested=false whole_product=false formal=false"
    );
    Ok(())
}

/// 短点击/撤销场景的整图已经释放后才装配这个长段；失败仍先完成相同的严格拆图，
/// 再报告业务错误。关闭票继续检查所有 Native、Doc/Input、Font、worker、FD、mmap 和像素。
fn direct_case(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        for segment in direct_segments() {
            file.write_all(segment.as_bytes())
                .map_err(|cause| cause.to_string())?;
        }
        file.flush().map_err(|cause| cause.to_string())?;
        let mut rig = Rig::attach_mode(
            mtm,
            file,
            DIRECT_UNITS,
            1,
            false,
            MarkdownPresentationMode::InlinePreview,
            false,
        )?;
        let result = verify_direct(&mut rig);
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result))
    })?;
    retirement.verify(false)?;
    if publication?.upgrade().is_some() {
        return Err("长直接链接关闭后原语义 publication 仍被强持有".into());
    }
    result
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, publication, result) = autoreleasepool(|_| -> Result<_, String> {
        let (file, units, lines) = fixture()?;
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
        let publication = rig
            .body
            .probe_root_leaf_publication()
            .map_err(|cause| cause.to_string());
        let retirement = release::close(rig, false)?;
        Ok((retirement, publication, result)) // 即使点击/帧断言失败，先按原完整资源图清理合成窗口和Source。
    })?;
    retirement.verify(false)?;
    if publication?.upgrade().is_some() {
        return Err("旧cache链接关闭后原有限Ready仍被持有".into());
    }
    result?;
    direct_case(mtm)
}
