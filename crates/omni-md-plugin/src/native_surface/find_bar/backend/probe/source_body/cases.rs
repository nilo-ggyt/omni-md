//! 原实际搜索、长选择、替换与Undo；查询只改变原输入栏值，正文期望来自固定fixture。

use super::*;

pub(super) fn pending(
    rig: &mut Rig,
    action: MarkdownFindAction,
) -> Result<Rc<NativeMarkdownPagedFindRevealRequest>, String> {
    let request = match rig.action(action).map_err(|cause| cause.to_string())? {
        NativeMarkdownFindExecution::Pending(request) => request,
        NativeMarkdownFindExecution::Complete => {
            return Err("真实Body Find必须先完成原异步Source目标".into());
        }
    };
    if !request.pending() || request.completed_visible() || !request.is_current() {
        return Err("真实Find begin没有保存原在途请求".into());
    }
    rig.requests.push(request.clone());
    Ok(request)
}

pub(super) fn visible(
    rig: &mut Rig,
    stage: &str,
    action: MarkdownFindAction,
    range: NSRange,
    source_complete: bool,
) -> Result<NativeMarkdownPagedBodyProgress, String> {
    let frame = observe(&rig.body)?;
    let request = pending(rig, action)?;
    // 普通远端查找必须保持未读尾部；只有已走完真实 EOF 的导航样本才接受完成态。
    let progress = if source_complete {
        frame::wait_complete_frame(stage, &rig.body, frame)?
    } else {
        wait_frame(stage, &rig.body, frame)?
    };
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if request.range() != range
        || !request.completed_visible()
        || request.pending()
        || request.cancelled()
        || request.take_failure().is_some()
        || key.request.input != request.input_state()
        || key.request.input.primary_range != range
    {
        return Err(format!("{stage}: 原Find没有按完整选择/原票完成"));
    }
    rig.view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    rig.remember_receipt()?;
    println!(
        "native_body_find stage={stage} full_selection={range:?} original_request_visible=true actual_first_scalar_visible=true formal=false whole_product=false"
    );
    Ok(progress)
}

pub(super) fn local_first(rig: &mut Rig) -> Result<(), String> {
    rig.bar.query.setStringValue(ns_string!("局部"));
    visible(
        rig,
        "local_first",
        MarkdownFindAction::Next,
        NSRange::new(fixture::LOCAL_FIRST, 2),
        false,
    )?;
    if !rig.bar.probe_numbered_feedback(1, 2) {
        return Err("真实局部导航没有显示原完整源的第一处/共两处".into());
    }
    Ok(())
}

pub(super) fn run(rig: &mut Rig) -> Result<(), String> {
    navigation::lazy_page(rig)?; // 原四MiB首屏未到EOF时验证真实翻页，随后保留所有远端Find与事务门。
    selection::run(rig)?; // 同一真实 Body 的多选图像完成后恢复单选，再沿原完整事务序列。
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.bar.query.setStringValue(ns_string!("目标"));
    visible(
        rig,
        "remote_next_70001",
        MarkdownFindAction::Next,
        NSRange::new(fixture::FIRST, 2),
        false,
    )?;
    if !rig.bar.probe_numbered_feedback(1, 2) {
        return Err("远端首目标的真实可见结果没有显示第一处/共两处".into());
    }
    let long_query = fixture::long_query();
    rig.bar
        .query
        .setStringValue(&NSString::from_str(&long_query));
    drop(long_query); // 只有当前Find字段保留显式查询；不将查询副本藏进Body/Source历史。
    let progress = visible(
        rig,
        "long_match_first_scalar",
        MarkdownFindAction::Next,
        NSRange::new(fixture::FIRST, fixture::LONG),
        false,
    )?;
    if progress.frontier_source_bytes >= fixture::LONG_END_BYTES
        || progress.shaped_input_bytes >= fixture::LONG_END_BYTES
    {
        return Err("9000unit完整选择错误地促使匹配尾端进入frontier/shaper".into());
    }
    if !rig.bar.probe_numbered_feedback(1, 1) {
        return Err("跨视口长匹配没有按原完整扫描显示唯一结果".into());
    }
    for (stage, action, start, ordinal) in [
        (
            "remote_next_140001",
            MarkdownFindAction::Next,
            fixture::SECOND,
            2,
        ),
        (
            "remote_previous",
            MarkdownFindAction::Previous,
            fixture::FIRST,
            1,
        ),
        (
            "remote_next_again",
            MarkdownFindAction::Next,
            fixture::SECOND,
            2,
        ),
        ("remote_wrap", MarkdownFindAction::Next, fixture::FIRST, 1),
    ] {
        rig.bar.query.setStringValue(ns_string!("目标"));
        visible(rig, stage, action, NSRange::new(start, 2), false)?;
        // 同一四MiB mmap Source的两处预声明目标；远端、反向与末尾回绕都必须带真实次序。
        if !rig.bar.probe_numbered_feedback(ordinal, 2) {
            return Err(format!("{stage}: 原导航次序或全文总数反馈错误"));
        }
        let actual = rig
            .view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if actual.source != initial.source {
            return Err("纯Find选择/Reveal错误地捕获第二Source".into());
        }
    }
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let before_serial = rig
        .body
        .progress()
        .map_err(|cause| cause.to_string())?
        .producer_serial;
    rig.bar.query.setStringValue(ns_string!("不存在的查询"));
    if !matches!(
        rig.action(MarkdownFindAction::Next),
        Ok(NativeMarkdownFindExecution::Complete)
    ) || rig.view.input_state().map_err(|cause| cause.to_string())? != before
        || rig
            .body
            .progress()
            .map_err(|cause| cause.to_string())?
            .producer_serial
            != before_serial
        || !rig
            .bar
            .status
            .stringValue()
            .isEqualToString(ns_string!("没有匹配结果"))
    {
        return Err("无匹配错误地改选择、捕Source或发布错误反馈".into());
    }
    transactions(rig)?;
    interleave::before_binding(rig)?;
    Ok(())
}

/// 独立事务控制仍用相同4MiB原Doc和真实Body；不替代前面的远端/9000选择门。
pub(super) fn transactions(rig: &mut Rig) -> Result<(), String> {
    let initial = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    local_first(rig)?;
    let before = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let revision = rig
        .document
        .upgrade()
        .ok_or("原文档已释放")?
        .borrow()
        .current_revision()
        .map_err(|cause| cause.to_string())?;
    rig.bar.replacement.setStringValue(ns_string!("替换文"));
    navigation::pending_scroll_before_edit(rig)?; // 两条尚未消费的纯滚动必须由随后真实g+1编辑撤销。
    visible(
        rig,
        "replace_next",
        MarkdownFindAction::ReplaceNext,
        NSRange::new(fixture::LOCAL_SECOND + 1, 2),
        false,
    )?;
    let replaced = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if replaced.source == initial.source
        || replaced.request.input.generation != before.generation + 1
        || replaced.request.input.utf16_len != fixture::UTF16 + 1
        || rig.copy::<5>(0)? != [0x4E2D, 0x4E2D, 0x66FF, 0x6362, 0x6587]
        || rig.copy::<2>(fixture::LOCAL_SECOND + 1)? != [0x5C40, 0x90E8]
    {
        return Err("真实ReplaceNext没有仅提交一次并按原spool位移下一个匹配".into());
    }
    let undo = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    wait_frame("replace_undo", &rig.body, undo)?;
    let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
    let document = rig.document.upgrade().ok_or("Undo文档已经释放")?;
    let document = document.borrow();
    if actual.generation != before.generation + 2
        || actual.utf16_len != fixture::UTF16
        || actual.selection != before.selection
        || document
            .current_revision()
            .map_err(|cause| cause.to_string())?
            != revision
        || document.can_undo()
        || !document.can_redo()
    {
        return Err("真实Undo没有恢复原字节/选择或替换被重放".into());
    }
    drop(document);
    if rig.copy::<4>(0)? != [0x4E2D, 0x4E2D, 0x5C40, 0x90E8] {
        return Err("Undo未恢复固定原中文前缀".into());
    }
    rig.remember_receipt()?;
    Ok(())
}
