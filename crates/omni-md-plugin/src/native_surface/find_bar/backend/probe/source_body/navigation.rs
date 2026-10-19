//! 独立65,523B输入的真实文档头尾及中部字体回退；原四MiB Find、三秒门与关闭收据保留。

use super::*;
use objc2::runtime::Sel;
use omni_markdown_core::MarkdownLayoutTextAuthority;
use std::io::Write;

const UTF16: usize = 21_846;
const BYTES: usize = 65_523;
const EMOJI_PAIR: usize = 8_192;
const LATER_EMOJI: usize = 16_000;

/// 中部连续/后续emoji后恢复中文，末四units仍是😀e+U+0301；全部按固定小片生成。
fn write(file: &mut tempfile::NamedTempFile) -> Result<(), String> {
    fixture::fill(file, fixture::LOCAL_FIRST)?;
    file.write_all("局部".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fixture::fill(file, fixture::LOCAL_SECOND - fixture::LOCAL_FIRST - 2)?;
    file.write_all("局部".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fixture::fill(file, 128 - fixture::LOCAL_SECOND - 2)?;
    file.write_all("目标\r\n".as_bytes())
        .map_err(|cause| cause.to_string())?;
    fixture::fill(file, EMOJI_PAIR - 132)?;
    file.write_all("😀😀".as_bytes())
        .map_err(|cause| cause.to_string())?;
    // 两个emoji占四个UTF16单位；后续中文要求真实回到原字体，不能把回退字体延用到段尾。
    fixture::fill(file, LATER_EMOJI - EMOJI_PAIR - 4)?;
    file.write_all("😀".as_bytes())
        .map_err(|cause| cause.to_string())?;
    // 第二个独立回退段随后仍有中文，检验同一Source/plan可以再次暂停并接续。
    fixture::fill(file, UTF16 - LATER_EMOJI - 2 - 4)?;
    file.write_all("😀e\u{301}".as_bytes())
        .map_err(|cause| cause.to_string())?;
    file.flush().map_err(|cause| cause.to_string())?;
    if file
        .as_file()
        .metadata()
        .map_err(|cause| cause.to_string())?
        .len()
        != BYTES as u64
    {
        return Err("导航样本真实字节长度不符".into());
    }
    Ok(())
}

/// 从实际NSTextInputClient selector进入，与系统文本动作相同，不调用私有选择setter。
fn selector(rig: &Rig, selector: Sel) {
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
    // SAFETY: client来自仍由本Rig保活的真实View；selector只取下面固定Cocoa文本动作。
    unsafe { client.doCommandBySelector(selector) };
}

fn endpoint(
    rig: &mut Rig,
    stage: &str,
    action: Sel,
    expected: MarkdownTextSelection,
    source: &MarkdownLayoutTextAuthority,
    complete: bool,
) -> Result<(), String> {
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let event = observe(&rig.body)?;
    selector(rig, action);
    let post = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if post.generation != before.generation
        || post.utf16_len != UTF16
        || post.selection_range_count != 1
        || post.directed_selection() != Some(expected)
        || post.selection_serial == before.selection_serial
    {
        return Err(format!("{stage}: 原selector没有提交唯一真实文档端点"));
    }
    if complete {
        frame::wait_complete_frame(stage, &rig.body, event)?;
    } else {
        wait_frame(stage, &rig.body, event)?;
    }
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input != post || key.source != *source {
        return Err(format!("{stage}: 端点显示重领了后继输入或另一个Source"));
    }
    let proof = rig
        .view
        .verify_find_target_visible(&key, NSRange::new(expected.head_utf16, 0))
        .map_err(|cause| cause.to_string())?;
    if proof.range() != NSRange::new(expected.head_utf16, 0) || !proof.key().same(&key) {
        return Err(format!("{stage}: 没有原实际viewport上的端点可见证明"));
    }
    rig.remember_receipt()?;
    println!(
        "native_body_navigation stage={stage} fixture_bytes={BYTES} fixture_utf16={UTF16} actual_postcommit={post:?} actual_visible_rect={:?} actual_document_origin={:?} original_source=true actual_complete_frame=true whole_product=false formal=false",
        proof.first_rect(),
        key.request.virtual_viewport.document_origin
    );
    Ok(())
}

/// 未到EOF时四个垂直selector仍须交付真实完整帧；纯选择不改文字，Shift始终保留原anchor。
fn vertical(
    rig: &mut Rig,
    stage: &str,
    action: Sel,
    backwards: bool,
    anchor: Option<usize>,
    source: &MarkdownLayoutTextAuthority,
) -> Result<(usize, f64, f64), String> {
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let before_key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let event = observe(&rig.body)?;
    selector(rig, action);
    wait_frame(stage, &rig.body, event)?; // 完整像素帧不等于全文complete；这些动作只准备真实目标附近的前缀。
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let after = key.request.input;
    let selected = after.directed_selection().ok_or("垂直导航丢失原有向选择")?;
    if after.generation != before.generation
        || after.utf16_len != before.utf16_len
        || key.source != *source
        || after.selection_serial == before.selection_serial
        || if backwards {
            selected.head_utf16 >= before.selection.head_utf16
        } else {
            selected.head_utf16 <= before.selection.head_utf16
        }
        || selected.anchor_utf16 != anchor.unwrap_or(selected.head_utf16)
    {
        return Err(format!(
            "{stage}: 垂直selector没有提交正确方向/anchor/同源选择"
        ));
    }
    let proof = rig
        .view
        .verify_find_target_visible(&key, NSRange::new(selected.head_utf16, 0))
        .map_err(|cause| cause.to_string())?;
    if !proof.key().same(&key) {
        return Err("垂直导航没有真实after可见证明".into());
    }
    rig.remember_receipt()?;
    println!(
        "native_body_vertical stage={stage} before_head={} after_head={} anchor={} before_document_y={} after_document_y={} caret_x={} actual_complete_frame=true original_source=true formal=false whole_product=false",
        before.selection.head_utf16,
        selected.head_utf16,
        selected.anchor_utf16,
        before_key.request.virtual_viewport.document_origin.y,
        key.request.virtual_viewport.document_origin.y,
        proof.first_rect().origin.x
    );
    Ok((
        selected.head_utf16,
        key.request.virtual_viewport.document_origin.y,
        proof.first_rect().origin.x,
    ))
}

/// 原Pending页面在真实宽度变化后必须撤销；恢复尺寸只正常重排，不恢复旧导航意图。
fn vertical_width_cancel(rig: &mut Rig) -> Result<(), String> {
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let mut previous = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    for action in [sel!(pageDown:), sel!(scrollPageDown:)] {
        selector(rig, action);
        selector(rig, action); // 原宽度负控同时撤销尚未消费的整段，不能只取消最后一条。
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err("在途Page动作提前改变了原输入".into());
        }
        for width in [680.0, 720.0] {
            let event = observe(&rig.body)?;
            rig.panel.setContentSize(NSSize::new(width, 348.0));
            wait_frame("vertical_width_cancels_original_page", &rig.body, event)?; // 宽度改变只重排当前首屏，不得为测试强制全文EOF。
            let key = rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            if key.request.input != before
                || key.request.virtual_viewport.document_origin.y != 0.0
                || key.request.bounds_width == previous.request.bounds_width
                || key.source == previous.source
            {
                return Err("旧PageDown跨宽度重排补签了后来选择或滚动".into());
            }
            rig.remember_receipt()?;
            previous = key; // 每次真实宽度对应新排版Source；恢复旧尺寸不能复活先前Pending票。
        }
    }
    Ok(())
}

/// 真实Page往返与24次邻行往返跨越两个视口边界；保存的只有有限源位置，不缓存文字或行索引。
fn vertical_cases(rig: &mut Rig, source: &MarkdownLayoutTextAuthority) -> Result<(), String> {
    let end_frame = observe(&rig.body)?;
    selector(rig, sel!(moveToEndOfLine:));
    wait_frame("line_end_before_vertical_short_line", &rig.body, end_frame)?;
    let end_key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let end_head = end_key.request.input.selection.head_utf16;
    let end_x = rig
        .view
        .verify_find_target_visible(&end_key, NSRange::new(end_head, 0))
        .map_err(|cause| cause.to_string())?
        .first_rect()
        .origin
        .x;
    let mut line_heads = [end_head; 4];
    let mut line_x = [end_x; 3];
    for index in 0..3 {
        let (head, _, x) = vertical(
            rig,
            "vertical_goal_across_short_crlf_line",
            sel!(moveDown:),
            false,
            None,
            source,
        )?;
        line_heads[index + 1] = head;
        line_x[index] = x;
    }
    if line_x[1] >= end_x || line_x[2] != end_x {
        return Err("短CRLF行没有保留随后长行所需的原目标列".into());
    }
    for index in (0..3).rev() {
        let (head, _, _) = vertical(
            rig,
            "vertical_goal_returns_original_softline",
            sel!(moveUp:),
            true,
            None,
            source,
        )?;
        if head != line_heads[index] {
            return Err("Up没有返回同一原softline及换行侧".into());
        }
    }
    endpoint(
        rig,
        "vertical_short_line_reset_to_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        source,
        false,
    )?;
    let (page_head, page_y, x) = vertical(
        rig,
        "page_down_from_document_start",
        sel!(pageDown:),
        false,
        None,
        source,
    )?;
    if page_y <= 0.0 {
        return Err("PageDown没有真正滚过原viewport边缘".into());
    }
    let mut heads = [0_usize; 25];
    heads[0] = page_head;
    let mut last_y = page_y;
    let mut down_scrolled = false;
    for remembered_head in heads.iter_mut().skip(1) {
        let (head, y, actual_x) = vertical(
            rig,
            "down_across_viewport",
            sel!(moveDownAndModifySelection:),
            false,
            Some(page_head),
            source,
        )?;
        if actual_x != x {
            return Err("连续Down被短行重置了原目标列".into());
        }
        *remembered_head = head;
        down_scrolled |= y > last_y;
        last_y = y;
    }
    let mut up_scrolled = false;
    for &expected_head in heads[..heads.len() - 1].iter().rev() {
        let (head, y, actual_x) = vertical(
            rig,
            "up_across_viewport",
            sel!(moveUpAndModifySelection:),
            true,
            Some(page_head),
            source,
        )?;
        if head != expected_head || actual_x != x {
            return Err("连续Up没有逐条返回真实相邻视觉行或丢失目标列".into());
        }
        up_scrolled |= y < last_y;
        last_y = y;
    }
    if !down_scrolled || !up_scrolled {
        return Err("邻行往返未真正覆盖两个viewport边界".into());
    }
    let (top, _, _) = vertical(
        rig,
        "page_up_to_document_start",
        sel!(pageUp:),
        true,
        None,
        source,
    )?;
    if top != 0 {
        return Err("原目标列的Page往返未回到文首实际caret".into());
    }
    let event = observe(&rig.body)?;
    selector(rig, sel!(pageDown:)); // 仍未准备的页面只在途，不得跨后续明确DocStart复活。
    selector(rig, sel!(pageDown:)); // 连续已接受动作也必须整体归还，不能留下第二次旧导航。
    selector(rig, sel!(moveToBeginningOfDocument:));
    wait_frame("new_document_start_cancels_pending_page", &rig.body, event)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.request.input.directed_selection() != Some(MarkdownTextSelection::caret(0))
        || key.source != *source
        || key.request.virtual_viewport.document_origin.y != 0.0
    {
        return Err("旧Page意图抢回了后来的文首导航".into());
    }
    rig.remember_receipt()?;
    Ok(())
}

/// 另用20pt正文视口强制相邻行离屏；真实逐帧落点作为连发oracle，原240pt回归保持不变。
fn pending_vertical_sequence(
    rig: &mut Rig,
    source: &MarkdownLayoutTextAuthority,
) -> Result<(), String> {
    endpoint(
        rig,
        "burst_oracle_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        source,
        false,
    )?;
    let (first, _, _) = vertical(
        rig,
        "burst_oracle_first_visual_line",
        sel!(moveDown:),
        false,
        None,
        source,
    )?;
    let (second, _, _) = vertical(
        rig,
        "burst_oracle_second_visual_line",
        sel!(moveDown:),
        false,
        None,
        source,
    )?;
    endpoint(
        rig,
        "burst_oracle_restore_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        source,
        false,
    )?;
    let original = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let original_progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    let resized = observe(&rig.body)?;
    rig.panel.setContentSize(NSSize::new(720.0, 128.0)); // 原Find栏108pt保持原样，只把真实正文收为20pt。
    wait_frame("burst_twenty_point_viewport", &rig.body, resized)?;
    let small = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if small.request.viewport.size.height != 20.0
        || small.request.input != original
        || small.source != *source
    {
        return Err("连发控制没有取得同源20pt真实小视口或缩放改变了输入".into());
    }
    // 每个场景都在首条实际尚未提交时发送第二条；反向及Shift分别消费，不能用净delta替代。
    for (stage, actions, expected) in [
        (
            "pending_down_twice",
            [sel!(moveDown:), sel!(moveDown:)],
            MarkdownTextSelection::caret(second),
        ),
        (
            "pending_down_then_up",
            [sel!(moveDown:), sel!(moveUp:)],
            MarkdownTextSelection::caret(0),
        ),
        (
            "pending_shift_down_twice",
            [
                sel!(moveDownAndModifySelection:),
                sel!(moveDownAndModifySelection:),
            ],
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: second,
            },
        ),
        (
            "pending_down_then_shift_down",
            [sel!(moveDown:), sel!(moveDownAndModifySelection:)],
            MarkdownTextSelection {
                anchor_utf16: first,
                head_utf16: second,
            },
        ),
        (
            "pending_shift_down_then_down",
            [sel!(moveDownAndModifySelection:), sel!(moveDown:)],
            MarkdownTextSelection::caret(second),
        ),
        (
            "pending_bof_clamp_then_shift_down",
            [sel!(moveUp:), sel!(moveDownAndModifySelection:)],
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: first,
            },
        ),
    ] {
        endpoint(
            rig,
            "burst_reset_original_start",
            sel!(moveToBeginningOfDocument:),
            MarkdownTextSelection::caret(0),
            source,
            false,
        )?;
        let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
        let event = observe(&rig.body)?;
        selector(rig, actions[0]);
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err(format!(
                "{stage}: 首个动作已经提交，未覆盖真实Pending连续输入"
            ));
        }
        selector(rig, actions[1]); // 同一runloop不执行中间队列，第二条必须追加到原未提交输入。
        if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
            return Err(format!("{stage}: 连续请求提前发布了中间选择"));
        }
        let progress = wait_frame(stage, &rig.body, event)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let after = key.request.input;
        if after.generation != before.generation
            || after.utf16_len != UTF16
            || after.directed_selection() != Some(expected)
            || after.selection_range_count != 1
            || after.selection_serial == before.selection_serial
            || key.source != *source
            || progress.producer_serial != original_progress.producer_serial
        {
            return Err(format!(
                "{stage}: 连发最终选择没有逐步匹配原真实视觉行/Shift锚点"
            ));
        }
        let proof = rig
            .view
            .verify_find_target_visible(&key, NSRange::new(expected.head_utf16, 0))
            .map_err(|cause| cause.to_string())?;
        if !proof.key().same(&key) {
            return Err(format!("{stage}: 没有原最终选择的真实可见帧"));
        }
        rig.remember_receipt()?;
        println!(
            "native_body_vertical stage={stage} accepted_actions=2 first_action_uncommitted=true actual_selection={expected:?} serialized_first={first} serialized_second={second} viewport_height=20 actual_complete_frame=true source_complete=false original_source=true whole_product=false formal=false"
        );
    }
    let restored = observe(&rig.body)?;
    rig.panel.setContentSize(NSSize::new(720.0, 348.0));
    wait_frame("burst_restore_original_viewport", &rig.body, restored)?;
    endpoint(
        rig,
        "burst_restore_original_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        source,
        false,
    )?;
    if rig
        .document
        .upgrade()
        .ok_or("连发原文档已释放")?
        .borrow()
        .can_undo()
    {
        return Err("连续纯导航增加了文字Undo历史".into());
    }
    Ok(())
}

/// 原四MiB单段落尚未准备到远端时真正翻页往返；不能先走DocEnd再把已有行当懒加载。
pub(super) fn lazy_page(rig: &mut Rig) -> Result<(), String> {
    let original = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let initial = rig.body.progress().map_err(|cause| cause.to_string())?;
    if initial.complete
        || initial.source_total_bytes != fixture::BYTES
        || original.request.input.utf16_len != fixture::UTF16
        || original.request.input.directed_selection() != Some(MarkdownTextSelection::caret(0))
    {
        return Err("四MiB翻页控制没有从原未完成首屏及文首开始".into());
    }
    for (stage, action, backwards) in [
        ("lazy_four_mib_page_down", sel!(pageDown:), false),
        ("lazy_four_mib_page_up", sel!(pageUp:), true),
    ] {
        let (head, y, _) = vertical(rig, stage, action, backwards, None, &original.source)?;
        let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
        // 原首个远端Find位于70001；只翻一页不应让该处进入frontier/shaper，更不能排到全文EOF。
        if progress.complete
            || progress.producer_serial != initial.producer_serial
            || progress.frontier_source_bytes >= fixture::FIRST * 3
            || progress.shaped_input_bytes >= fixture::FIRST * 3
            || if backwards {
                head != 0 || y != 0.0
            } else {
                head == 0 || y <= 0.0
            }
        {
            return Err(format!("{stage}: 原未完成Source没有按真实视口翻页往返"));
        }
        println!(
            "native_body_vertical stage={stage} source_bytes={} frontier_source_bytes={} shaped_input_bytes={} source_complete=false actual_page_scroll=true original_producer=true formal=false whole_product=false",
            progress.source_total_bytes,
            progress.frontier_source_bytes,
            progress.shaped_input_bytes
        );
    }
    pure_page_scroll(rig)?;
    Ok(())
}

/// 同一未完成4MiB正文连续两次纯Page，不改caret/原目标列；动作不能被覆盖成最后一次。
fn pure_page_scroll(rig: &mut Rig) -> Result<(), String> {
    let source = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?
        .source;
    // 先用真实邻行往返建立非空垂直目标列；纯滚动不能把它清掉或换成当前可见行的列。
    vertical(
        rig,
        "pure_page_seed_vertical_goal",
        sel!(moveDown:),
        false,
        None,
        &source,
    )?;
    vertical(
        rig,
        "pure_page_restore_original_caret",
        sel!(moveUp:),
        true,
        None,
        &source,
    )?;
    let original = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let initial = rig.body.progress().map_err(|cause| cause.to_string())?;
    let input = rig
        .body
        .probe_find_input_weak()
        .upgrade()
        .ok_or("原Input已释放")?;
    let body = rig.body.clone(); // 原权限闭包只保活当前Body到本控制返回，不借住整个Rig妨碍归还真实收据。
    let current = |epoch| {
        epoch == EPOCH
            && NativeMarkdownPagedTextViewDelegate::current(&*body, epoch)
            && input
                .state()
                .is_ok_and(|state| state == original.request.input)
    };
    let permit = input
        .capture(EPOCH, &current)
        .map_err(|cause| cause.to_string())?;
    let facts = input
        .navigation_selection_facts(&permit, &current)
        .map_err(|cause| cause.to_string())?;
    if facts.1.is_none() {
        return Err("纯滚动控制未取得真实非空目标列".into());
    }
    for (stage, action, down) in [
        ("pure_page_two_down", sel!(scrollPageDown:), true),
        ("pure_page_two_up", sel!(scrollPageUp:), false),
    ] {
        let event = observe(&rig.body)?;
        selector(rig, action);
        selector(rig, action); // 不运行中间队列，两次真实已接受动作必须逐次交标准Native滚动。
        if rig.view.input_state().map_err(|cause| cause.to_string())? != original.request.input {
            return Err("裸Page在排版等待前改变了原Input".into());
        }
        let progress = wait_frame(stage, &rig.body, event)?;
        let key = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let y = key.request.virtual_viewport.document_origin.y;
        if key.source != original.source
            || key.request.input != original.request.input
            || progress.producer_serial != initial.producer_serial
            || progress.frontier_source_bytes >= fixture::FIRST * 3
            || progress.shaped_input_bytes >= fixture::FIRST * 3
            || input
                .navigation_selection_facts(&permit, &current)
                .map_err(|cause| cause.to_string())?
                != facts
            || if down {
                y <= key.request.viewport.size.height
            } else {
                y != 0.0
            }
        {
            return Err(format!(
                "{stage}: 连续纯Page丢动作或改变了原Source/Input/目标列"
            ));
        }
        rig.remember_receipt()?;
        println!(
            "native_body_page_scroll stage={stage} accepted_page_requests=2 actual_document_y={y} viewport_height={} original_input_and_goal=true source_complete=false formal=false whole_product=false",
            key.request.viewport.size.height
        );
    }
    Ok(())
}

/// 实际编辑前留下未消费的原纯滚动；后继Replace必须撤它，而非在新Source重放。
pub(super) fn pending_scroll_before_edit(rig: &Rig) -> Result<(), String> {
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    selector(rig, sel!(scrollPageDown:));
    selector(rig, sel!(scrollPageDown:));
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("纯滚动候选提前修改了编辑前原Input".into());
    }
    Ok(())
}

/// 只有原Composer已经到真实EOF才允许系统底部clamp；期望取实际空滚动文档frame，不估计全文行高。
fn pure_page_eof_clamp(rig: &mut Rig) -> Result<(), String> {
    let before = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let document = rig.scroll.documentView().ok_or("EOF原滚动文档已断开")?;
    let maximum_y = (document.frame().size.height
        - rig.scroll.contentView().documentVisibleRect().size.height)
        .max(0.0);
    let initial = rig.body.progress().map_err(|cause| cause.to_string())?;
    if !initial.complete || before.request.input.selection.head_utf16 != UTF16 {
        return Err("EOF纯滚动控制没有取得原已完成文档末端".into());
    }
    let event = observe(&rig.body)?;
    selector(rig, sel!(scrollPageDown:));
    selector(rig, sel!(scrollPageDown:));
    let progress = frame::wait_complete_frame("pure_page_eof_clamp", &rig.body, event)?;
    let after = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if after.request.input != before.request.input
        || after.source != before.source
        || after.request.virtual_viewport.document_origin.y != maximum_y
        || progress.producer_serial != initial.producer_serial
        || progress.frontier_source_bytes != initial.frontier_source_bytes
        || progress.shaped_input_bytes != initial.shaped_input_bytes
    {
        return Err("EOF重复纯Page未沿实际文档clamp或改变了原Source/Input".into());
    }
    rig.remember_receipt()?;
    println!(
        "native_body_page_scroll stage=pure_page_eof_clamp accepted_page_requests=2 actual_maximum_y={maximum_y} actual_eof=true original_input_and_source=true formal=false whole_product=false"
    );
    Ok(())
}

/// 原全CJK/emoji源已取得全源UniformZero；标准Word现在必须真实移动，不能保留旧Unsupported负控。
fn known_word_move(rig: &mut Rig) -> Result<(), String> {
    let before = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let progress = rig.body.progress().map_err(|cause| cause.to_string())?;
    if before.request.input.directed_selection() != Some(MarkdownTextSelection::caret(0))
        || rig.copy::<2>(130)? != [13, 10]
    {
        return Err("原Word移动前提不是BOF与实际首CRLF".into());
    }
    let history = {
        let document = rig.document.upgrade().ok_or("原导航文稿已释放")?;
        let document = document.borrow();
        (
            document
                .current_revision()
                .map_err(|cause| cause.to_string())?,
            document.can_undo(),
            document.can_redo(),
        )
    };
    let frame = observe(&rig.body)?;
    selector(rig, sel!(moveWordForward:));
    if rig.view.input_state().map_err(|cause| cause.to_string())? != before.request.input {
        return Err("已接纳Word在原异步准备前擅自发布选择".into());
    }
    let after_progress = wait_frame("navigation_word_forward_first_crlf", &rig.body, frame)?;
    let after = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if after.request.input.directed_selection() != Some(MarkdownTextSelection::caret(130))
        || after.request.input.generation != before.request.input.generation
        || after.request.input.utf16_len != before.request.input.utf16_len
        || after.request.input.selection_serial == before.request.input.selection_serial
        || after.source != before.source
        || after_progress.producer_serial != progress.producer_serial
        || rig.copy::<2>(130)? != [13, 10]
    {
        return Err("标准Word没有在同原Source/Producer上到达实际首CRLF前的词末".into());
    }
    {
        let document = rig.document.upgrade().ok_or("原导航文稿已释放")?;
        let document = document.borrow();
        if (
            document
                .current_revision()
                .map_err(|cause| cause.to_string())?,
            document.can_undo(),
            document.can_redo(),
        ) != history
        {
            return Err("纯Word选择错误改变了原字节版本或字符Undo/Redo".into());
        }
    }
    rig.remember_receipt()?;
    endpoint(
        rig,
        "navigation_word_restore_bof",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        &before.source,
        false,
    )?;
    println!(
        "native_body_navigation uniform_word_forward=true actual_head=130 actual_first_crlf=true same_source_and_producer=true no_character_undo=true mixed_bidi_word_complete=false formal=false whole_product=false"
    );
    Ok(())
}

fn cases(rig: &mut Rig) -> Result<(), String> {
    known_word_move(rig)?;
    vertical_width_cancel(rig)?;
    let original = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if rig.copy::<4>(0)? != [0x4e2d, 0x4e2d, 0x5c40, 0x90e8]
        || rig.copy::<2>(130)? != [13, 10]
        || rig.copy::<6>(EMOJI_PAIR - 1)? != [0x4e2d, 0xd83d, 0xde00, 0xd83d, 0xde00, 0x4e2d]
        || rig.copy::<4>(LATER_EMOJI - 1)? != [0x4e2d, 0xd83d, 0xde00, 0x4e2d]
        || rig.copy::<4>(UTF16 - 4)? != [0xd83d, 0xde00, 0x65, 0x301]
    {
        return Err("导航样本真实首部/CRLF/中部emoji回退/组合尾部不符".into());
    }
    cases::local_first(rig)?;
    let multi_frame = observe(&rig.body)?;
    if !matches!(
        rig.action(MarkdownFindAction::SelectAll),
        Ok(NativeMarkdownFindExecution::Complete)
    ) {
        return Err("导航前真实Find SelectAll没有同步接受两个范围".into());
    }
    wait_frame("navigation_unknown_multi", &rig.body, multi_frame)?;
    let unknown = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if unknown.selection_range_count != 2
        || unknown.directed_selection().is_some()
        || unknown.selection.head_utf16 != fixture::LOCAL_FIRST + 2
    {
        return Err("导航控制没有取得真实未知方向多选及原非零memento".into());
    }
    endpoint(
        rig,
        "unknown_multi_document_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        &original.source,
        false,
    )?;
    vertical_cases(rig, &original.source)?;
    pending_vertical_sequence(rig, &original.source)?;
    cases::local_first(rig)?;
    endpoint(
        rig,
        "extend_document_end",
        sel!(moveToEndOfDocumentAndModifySelection:),
        MarkdownTextSelection {
            anchor_utf16: fixture::LOCAL_FIRST,
            head_utf16: UTF16,
        },
        &original.source,
        true,
    )?; // 非零anchor必须原样保留；真正EOF只能由complete后的末行签发。
    // 前缀计算到EOF不能替代中部真实绘制；通过原Find把两个回退段分别带进实际视口。
    rig.bar.query.setStringValue(ns_string!("😀😀"));
    cases::visible(
        rig,
        "middle_emoji_pair_visible",
        MarkdownFindAction::Next,
        NSRange::new(EMOJI_PAIR, 4),
        true,
    )?;
    selection::emoji_pixels(rig, "middle_emoji_pair_pixels", NSRange::new(EMOJI_PAIR, 4))?;
    rig.bar.query.setStringValue(ns_string!("😀中"));
    cases::visible(
        rig,
        "later_emoji_and_restored_chinese_visible",
        MarkdownFindAction::Next,
        NSRange::new(LATER_EMOJI, 3),
        true,
    )?;
    selection::emoji_pixels(rig, "later_emoji_pixels", NSRange::new(LATER_EMOJI, 3))?;
    endpoint(
        rig,
        "complete_document_start",
        sel!(moveToBeginningOfDocument:),
        MarkdownTextSelection::caret(0),
        &original.source,
        true,
    )?;
    endpoint(
        rig,
        "complete_document_end",
        sel!(moveToEndOfDocument:),
        MarkdownTextSelection::caret(UTF16),
        &original.source,
        true,
    )?;
    pure_page_eof_clamp(rig)?;
    let immediate = observe(&rig.body)?;
    selector(rig, sel!(selectAll:));
    let all = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if all.directed_selection()
        != Some(MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: UTF16,
        })
    {
        return Err("原生selectAll没有提交真实有向全文选择".into());
    }
    selector(rig, sel!(moveToBeginningOfDocument:)); // 不运行队列、不等Ready，立即消费下一原生动作。
    let top = rig.view.input_state().map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("select_all_immediate_document_start", &rig.body, immediate)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if top.directed_selection() != Some(MarkdownTextSelection::caret(0))
        || key.request.input != top
        || key.source != original.source
        || top.generation != original.request.input.generation
    {
        return Err("SelectAll后立即DocStart没有沿同Source完成文首单选".into());
    }
    rig.view
        .verify_find_target_visible(&key, NSRange::new(0, 0))
        .map_err(|cause| cause.to_string())?;
    let document = rig.document.upgrade().ok_or("导航文档已经释放")?;
    if document.borrow().can_undo() {
        return Err("纯导航错误地增加了文字Undo历史".into());
    }
    drop(document);
    rig.remember_receipt()?;

    // 返回顶部后走真实NSTextInputClient输入，不能只证明一个逻辑head或用Core直接改字。
    let edited = observe(&rig.body)?;
    selector(rig, sel!(pageDown:)); // 原页面仍在途时实际输入必须撤它，不能在新Source里补签旧离屏目标。
    selector(rig, sel!(pageDown:)); // 整串原输入一起撤权，后继Source不能接手第二次动作。
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view);
    // SAFETY: 原client仍保活；NSNotFound要求它消费当前真实caret，载荷只有单个合成ASCII。
    unsafe {
        client.insertText_replacementRange(
            &NSString::from_str("N"),
            NSRange::new(objc2_foundation::NSNotFound as usize, 0),
        );
    }
    wait_frame("input_after_document_start", &rig.body, edited)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if key.source == original.source
        || key.request.input.generation != top.generation + 1
        || key.request.input.utf16_len != UTF16 + 1
        || rig.copy::<5>(0)? != [0x4e, 0x4e2d, 0x4e2d, 0x5c40, 0x90e8]
    {
        return Err("文首导航后原生输入没有只提交一次并保留原文本".into());
    }
    let edited_source = key.source;
    rig.bar.query.setStringValue(ns_string!("局部"));
    cases::visible(
        rig,
        "find_after_document_start_input",
        MarkdownFindAction::Next,
        NSRange::new(fixture::LOCAL_FIRST + 1, 2),
        false,
    )?;
    if rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?
        .source
        != edited_source
    {
        return Err("导航后Find错误地再捕Source".into());
    }
    Ok(())
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        metrics::sample("before_navigation_rig")?;
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        write(&mut file)?;
        let mut rig = Rig::attach_fixture(mtm, file, UTF16, 2)?;
        let result = cases(&mut rig);
        let pending = release::hold_target_fixture_pending(&mut rig); // 本导航源真实含“目标”，保留原跨close未绑定票负控。
        let retirement = release::close(rig, false)?; // 任一动作失败也先走原完整关闭/最后资源事件。
        Ok((retirement, pending.and(result)))
    })?;
    retirement.verify(false)?; // 与四MiB相同的Native/Source/Font/worker/像素门，没有新豁免。
    result
}
