//! 原已认证单格直接接同一Body/Source/绘图链；大草稿不经过NSTextField或完整NSString。

use super::*;
use crate::native_surface::paged_input::{
    NativeMarkdownPagedCellSession, NativeMarkdownPagedInput, NativeMarkdownPagedInputHistory,
    NativeMarkdownPagedInputOutcome,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_core_graphics::CGContext;
use omni_markdown_core::markdown_document_buffer::MarkdownTableCellEditStep;
use omni_markdown_core::{MarkdownCellDraftResourceObserver, MarkdownFormatAction};
use std::io::Write;

const SOURCE: &str = "| H | C |\n| --- | --- |\n| seed | other |\n";
const LARGE: usize = 2 * 1024 * 1024;
const SCRATCH: usize = 64 * 1024;

#[path = "cell/word.rs"]
mod word;

/// Cell的真实canonical宿主和原提交入口；只有这个owner保活Doc，显示Input仍只持Weak。
pub(super) struct Original {
    document: Rc<RefCell<MarkdownDocumentBuffer>>,
    canonical: Rc<NativeMarkdownPagedInput>,
    pub(super) session: Rc<NativeMarkdownPagedCellSession>,
    resources: MarkdownCellDraftResourceObserver,
}

impl Original {
    /// 读取原token的真实释放事实；只有提交/取消已完成才允许跳过旧草稿的正常辞任读取。
    pub(super) fn has_ended(&self) -> bool {
        self.resources.is_released()
    }

    pub(super) fn new(document: Rc<RefCell<MarkdownDocumentBuffer>>) -> Result<Self, String> {
        let canonical = NativeMarkdownPagedInput::new(Rc::downgrade(&document))
            .map_err(|cause| cause.to_string())?;
        let plan = {
            let doc = document.borrow();
            let mut request = doc
                .table_cell_edit_request(0, 1, 0)
                .map_err(|cause| format!("{cause:?}"))?;
            loop {
                // 同一个原格用例消费真实结构票和4KiB词法端点，不要求整行Parser或手造窗口。
                match request
                    .advance(&doc, false, 4096)
                    .map_err(|cause| format!("{cause:?}"))?
                {
                    MarkdownTableCellEditStep::Progress { .. } => {}
                    MarkdownTableCellEditStep::Ready { .. } => {
                        break request
                            .take_plan_for(&doc)
                            .map_err(|cause| format!("{cause:?}"))?
                            .ok_or("原Cell编辑计划未交付")?;
                    }
                    other => return Err(format!("原Cell编辑计划未Ready：{other:?}")),
                }
            }
        };
        let session =
            NativeMarkdownPagedCellSession::begin_prepared(Rc::downgrade(&document), plan)
                .map_err(|cause| cause.to_string())?;
        let resources = session
            .resource_observer()
            .map_err(|cause| cause.to_string())?;
        Ok(Self {
            document,
            canonical,
            session,
            resources,
        })
    }

    /// Body关闭已经取消/提交原token并drain；此时原canonical必须仍活且原映射仍健康。
    pub(super) fn verify_closed(&self) -> Result<(), String> {
        if !self.resources.is_released()
            || self
                .document
                .borrow()
                .original_backing_observer()
                .is_none_or(|backing| backing.failed() || backing.mapped_bytes() == 0)
        {
            return Err(format!(
                "Cell显示关闭后原草稿未归还或parent Doc提前释放：{:?}",
                self.resources.snapshot()
            ));
        }
        self.canonical.close().map_err(|cause| cause.to_string())?;
        println!(
            "native_body_cell stage=closed parent_doc_alive=true actual_cell_released=true resources={:?} formal=false whole_product=false",
            self.resources.snapshot()
        );
        Ok(())
    }
}

fn input(rig: &Rig) -> Result<Rc<NativeMarkdownPagedInput>, String> {
    rig.body
        .probe_find_input_weak()
        .upgrade()
        .ok_or("原Cell Input已释放".into())
}

/// 只设原Cell真实单选并消费原提交收据；没有为Cell伪造Body多选或离屏可见票。
fn select(rig: &Rig, range: std::ops::Range<usize>) -> Result<(), String> {
    let input = input(rig)?;
    let before = input.state().map_err(|cause| cause.to_string())?;
    let geometry = rig.view.find_geometry_epoch().ok_or("原Cell几何已释放")?;
    let current = |epoch| {
        rig.view
            .find_input_is_current(&input, epoch, &before, Some(geometry))
    };
    let permit = input
        .capture(EPOCH, &current)
        .map_err(|cause| cause.to_string())?;
    match input
        .set_selected_ranges(&permit, &[range], &current)
        .map_err(|cause| cause.to_string())?
    {
        NativeMarkdownPagedInputOutcome::Committed(commit) => rig
            .view
            .apply_find_commit(&input, &commit)
            .map_err(|cause| cause.to_string()),
        NativeMarkdownPagedInputOutcome::Unchanged(_) => Ok(()),
    }
}

fn client(rig: &Rig) -> &ProtocolObject<dyn NSTextInputClient> {
    ProtocolObject::<dyn NSTextInputClient>::from_ref(&**rig.view)
}

fn unspecified() -> NSRange {
    NSRange::new(objc2_foundation::NSNotFound as usize, 0)
}

fn local<const N: usize>(rig: &Rig, offset: usize) -> Result<[u16; N], String> {
    let input = input(rig)?;
    let permit = input
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let mut units = [0; N];
    input
        .copy_utf16_code_units(&permit, offset, &mut units, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    Ok(units)
}

/// 同一活动Scene上的复制必须归还本次临时reader；输出NSData独立于原Cell生命周期。
struct Copied {
    board: Retained<NSPasteboard>,
    data: Retained<objc2_foundation::NSData>,
}
impl Drop for Copied {
    fn drop(&mut self) {
        release_board(&self.board); // 后续任一步失败也只清理本case自有板，输出NSData随后真实Drop。
    }
}

fn copy(rig: &Rig, large: bool) -> Result<Copied, String> {
    let original = rig.cell.as_ref().ok_or("缺少原Cell owner")?;
    let input = input(rig)?;
    let before = input.state().map_err(|cause| cause.to_string())?;
    let resources = original.resources.snapshot();
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let board = NSPasteboard::pasteboardWithUniqueName();
    let result = (|| {
        rig.body
            .copy_source_selection_to(&key.request, &board)
            .map_err(|cause| cause.to_string())?;
        // SAFETY: 标准静态UTI与生产发布入口相同，不使用general board。
        let data = board
            .dataForType(unsafe { NSPasteboardTypeString })
            .ok_or("原Cell复制没有NSData")?;
        verify_output(&data, large)?;
        if input.state().map_err(|cause| cause.to_string())? != before
            || original.resources.snapshot().read_roots != resources.read_roots
            || !rig
                .view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?
                .same(&key)
        {
            return Err("Cell复制改变了原选择/Scene或遗留临时reader".into());
        }
        Ok(data)
    })();
    match result {
        Ok(data) => Ok(Copied { board, data }),
        Err(cause) => {
            release_board(&board);
            Err(cause)
        }
    }
}

fn verify_output(data: &objc2_foundation::NSData, large: bool) -> Result<(), String> {
    let expected = if large { LARGE } else { 3 }; // 小样本只选中“你”，大样本是实际两MiB ASCII草稿。
    if data.length() != expected {
        return Err("原Cell输出UTF8长度不符".into());
    }
    // SAFETY: Retained NSData由调用者覆盖本同步借用；不发任何修改data的Native消息。
    let bytes = unsafe { data.as_bytes_unchecked() };
    if large {
        for chunk in bytes.chunks(SCRATCH) {
            if chunk.iter().any(|byte| *byte != b'x') {
                return Err("原Cell大草稿输出字节不符".into());
            }
        } // 固定片检查，不构造第二份整格expected/String/NSString。
    } else if bytes != "你".as_bytes() {
        return Err("原Cell部分复制没有保持Unicode".into());
    }
    Ok(())
}

fn release_board(board: &NSPasteboard) {
    board.clearContents();
    // SAFETY: 只归还本case自己创建的唯一私有板，绝不改变用户general board。
    unsafe {
        let _: () = objc2::msg_send![board, releaseGlobally];
    }
}

fn unchanged(rig: &Rig) -> Result<(), String> {
    let original = rig.cell.as_ref().ok_or("缺少原Cell owner")?;
    let doc = original.document.borrow();
    let mut bytes = [0_u16; 64];
    let len = doc.utf16_len();
    if len > bytes.len() {
        return Err("Cell夹具canonical意外变长".into());
    }
    doc.copy_utf16_code_units(0, &mut bytes[..len])
        .map_err(|cause| cause.to_string())?;
    if doc.generation() != 0
        || doc.can_undo()
        || doc.can_redo()
        || bytes[..len].iter().copied().ne(SOURCE.encode_utf16())
    {
        return Err("局部Cell输入提前改变canonical或历史".into());
    }
    Ok(())
}

/// 同一canonical原位接收两MiB草稿；固定4KiB缓冲逐块核全文，不创建整格或整篇expected副本。
fn verify_large_canonical(original: &Original, generation: u64) -> Result<(), String> {
    let (prefix, suffix) = SOURCE.split_once("seed").expect("固定夹具含唯一原格");
    let prefix_units = prefix.encode_utf16().count();
    let suffix_units = suffix.encode_utf16().count();
    let document = original.document.borrow();
    if document.generation() != generation
        || document.byte_len() != prefix.len() + LARGE + suffix.len()
        || document.utf16_len() != prefix_units + LARGE + suffix_units
        || document.selection() != MarkdownTextSelection::caret(prefix_units + LARGE)
        || !document.can_undo()
        || document.can_redo()
    {
        return Err("两MiBCell的原canonical长度、代次、选择或单次历史状态不符".into());
    }
    let mut units = [0_u16; 2048];
    for (offset, expected) in [(0, prefix), (prefix_units + LARGE, suffix)] {
        // 两端仍须逐unit一致，不能只核中间x而漏掉邻格、分隔符或原EOL损坏。
        let length = expected.encode_utf16().count();
        document
            .copy_utf16_code_units(offset, &mut units[..length])
            .map_err(|cause| cause.to_string())?;
        if units[..length].iter().copied().ne(expected.encode_utf16()) {
            return Err("两MiBCell提交改变了原表头、邻格或行分隔符".into());
        }
    }
    for offset in (0..LARGE).step_by(units.len()) {
        // 包括首、中、尾的每一块都读取同一canonical；失败直接保原Source错误，不降为抽样通过。
        let length = units.len().min(LARGE - offset);
        document
            .copy_utf16_code_units(prefix_units + offset, &mut units[..length])
            .map_err(|cause| cause.to_string())?;
        if units[..length].iter().any(|unit| *unit != u16::from(b'x')) {
            return Err("两MiBCell提交没有完整保存原草稿字节".into());
        }
    }
    Ok(())
}

/// 活动端必须真实可见；折叠选择核插入线，原非空选择核高亮，均对照同一实际CPU挂载图。
fn selection_pixels(
    rig: &Rig,
    stage: &str,
    selected: MarkdownTextSelection,
) -> Result<NativeMarkdownPagedSceneKey, String> {
    let input = input(rig)?;
    let key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    let head = selected.head_utf16;
    let range = NSRange::new(head, 0);
    let primary = NSRange::new(
        selected.anchor_utf16.min(head),
        selected.anchor_utf16.abs_diff(head),
    );
    let empty_document = key.request.document.upgrade().is_none();
    let same_domain = input
        .target_domain()
        .map_err(|cause| cause.to_string())?
        .same(&key.request.target);
    println!(
        "native_body_cell stage={stage} actual_state={:?} expected_selection={selected:?} expected_primary={primary:?} canonical_weak_empty={empty_document} actual_domain_matches={same_domain}",
        key.request.input
    );
    if key.request.input.primary_range != primary
        || key.request.input.directed_selection() != Some(selected)
        || !empty_document
        || !same_domain
    {
        return Err(format!("{stage}: 原Cell caret或空DocWeak/真实domain不符"));
    }
    let proof = rig
        .view
        .verify_find_target_visible(&key, range)
        .map_err(|cause| cause.to_string())?;
    if !proof.key().same(&key) || proof.range() != range {
        return Err("原Cell可见证明未绑定同一Scene/范围".into());
    }
    let paints = rig.view.cpu_paints().ok_or("原Cell没有CPU绘制owner")?;
    let current = || {
        NativeMarkdownPagedTextViewDelegate::scene_current(&*rig.body, &key)
            && rig.view.cpu_paints() == Some(paints)
            && rig.view.cpu_paint_pending() == Some(false)
    };
    let scene = NativeMarkdownPagedTextViewDelegate::ready_scene(&*rig.body, &key.request)
        .map_err(|cause| cause.to_string())?;
    if !scene.key().same(&key) || !current() {
        return Err("原Cell不是已发布的同一完整帧".into());
    }
    let scale = rig.panel.backingScaleFactor();
    let actual = selection::Canvas::scaled(key.request.viewport, scale)?;
    let rendered = selection::Canvas::scaled(key.request.viewport, scale)?;
    let mask = selection::Canvas::scaled(key.request.viewport, scale)?;
    let steps = selection::paint_scene(scene.clone(), &key, &actual, &current)?;
    CGContext::set_rgb_fill_color(Some(&mask.context), 1.0, 0.0, 0.0, 1.0);
    if primary.length == 0 {
        let mut rect = proof.first_rect();
        rect.size.width = 1.0; // 真实caret允许零宽；生产Scene也在同位置绘制1pt插入线。
        CGContext::fill_rect(Some(&mask.context), rect);
    } else {
        let end = primary.location + primary.length;
        let mut cursor = primary.location;
        while cursor < end {
            // first_rect按原契约只返回首GCB，不把单字几何伪装整个选区；seed最多四个真实GCB。
            let first = scene
                .first_rect(NSRange::new(cursor, end - cursor))
                .map_err(|cause| cause.to_string())?;
            let next = first.actual_range.location + first.actual_range.length;
            if first.actual_range.location != cursor || next <= cursor || next > end {
                return Err("原Cell非空选择缺少同Source的完整GCB前进范围".into());
            }
            CGContext::fill_rect(Some(&mask.context), first.rect);
            cursor = next;
        }
    }
    let bounds = rig.view.bounds();
    let backing = rig.view.layer().ok_or("原Cell缺少实际CPU backing")?;
    // SAFETY: 本同步栈保活实际主线程CALayer；仅沿原View挂载读取子关系，不创建影子产品图。
    let children = unsafe { backing.sublayers() }.ok_or("原Cell没有CPU挂载")?;
    if children.len() != 1 {
        return Err("原Cell不是唯一CPU挂载".into());
    }
    let mount = children.objectAtIndex(0);
    if mount.frame() != bounds
        || mount.bounds() != NSRect::new(NSPoint::ZERO, bounds.size)
        || !current()
    {
        return Err("原Cell实际挂载/坐标已改变".into());
    }
    CGContext::translate_ctm(Some(&rendered.context), bounds.origin.x, bounds.origin.y);
    mount.renderInContext(&rendered.context);
    for canvas in [&actual, &rendered, &mask] {
        CGContext::flush(Some(&canvas.context));
    }
    scene
        .validate_ready(&current)
        .map_err(|cause| cause.to_string())?;
    let selection_color = selection::selection_pixel()?; // 原独立Native颜色校准，不以手算整数猜选择色。
    let mut locations = 0;
    let mut ink = 0;
    let mut mismatch = None;
    for (index, ((expected, output), marker)) in actual
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rendered.pixels.as_chunks::<4>().0)
        .zip(mask.pixels.as_chunks::<4>().0)
        .enumerate()
    {
        if *marker != [255, 0, 0, 255] {
            continue;
        } // 只比较独立Native遮罩内的真实可见caret，不取全文图。
        locations += 1;
        ink += usize::from(if primary.length == 0 {
            expected[3] == 255 && expected[..3].iter().any(|value| *value < 255)
        } else {
            *expected == selection_color
        });
        if expected != output && mismatch.is_none() {
            mismatch = Some((index, *expected, *output));
        }
    }
    if locations == 0 || ink == 0 || mismatch.is_some() || !current() {
        return Err(format!(
            "{stage}: 原Cell caret像素不符 mask={locations} ink={ink} first_mismatch={mismatch:?}"
        ));
    }
    println!(
        "native_body_cell stage={stage} actual_head_utf16={head} actual_selected_units={} actual_source_bytes={} actual_document_y={} cpu_scene_layer_rgba_equal=true visible_proof=true mask_pixels={locations} caret_ink_pixels={ink} paint_steps={steps} formal=false whole_product=false",
        primary.length,
        rig.body
            .progress()
            .map_err(|cause| cause.to_string())?
            .source_total_bytes,
        key.request.virtual_viewport.document_origin.y
    );
    Ok(key)
}

/// 格式动作经实际View入口触发，再以新Source完整CPU帧核局部选择与历史；不走裸Input代替产品调用。
fn format_selection(rig: &Rig) -> Result<(), String> {
    let selected = observe(&rig.body)?;
    select(rig, 0..1)?;
    frame::wait_complete_frame("cell_format_selected", &rig.body, selected)?;
    let before = selection_pixels(
        rig,
        "cell_format_selected",
        MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 1,
        },
    )?;
    let viewport = before.request.virtual_viewport.document_origin;
    let formatted = observe(&rig.body)?;
    rig.view
        .apply_format(MarkdownFormatAction::Bold)
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_format_bold", &rig.body, formatted)?;
    let after = selection_pixels(
        rig,
        "cell_format_bold",
        MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 3,
        },
    )?;
    if local::<5>(rig, 0)? != [42, 42, 0x4f60, 42, 42]
        || after.request.input.utf16_len != 5
        || after.request.input.generation == before.request.input.generation
        || after.source == before.source
        || after.request.virtual_viewport.document_origin != viewport
    {
        return Err("View格式没有形成原Cell新Source/选中inner，或无故改变原视口".into());
    }
    unchanged(rig)?; // 格式只能进入原Cell本地历史，不提前改canonical或邻格。
    for (selector, stage, formatted) in [
        (sel!(undo:), "cell_format_undo", false),
        (sel!(redo:), "cell_format_redo", true),
        (sel!(undo:), "cell_format_restore", false),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(selector)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let selection = if formatted {
            if local::<5>(rig, 0)? != [42, 42, 0x4f60, 42, 42]
                || rig
                    .view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .utf16_len
                    != 5
            {
                return Err("View格式Redo没有恢复真实局部标记".into());
            }
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 3,
            }
        } else {
            if local::<1>(rig, 0)? != [0x4f60]
                || rig
                    .view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    .utf16_len
                    != 1
            {
                return Err("View格式单次Undo没有只还原原中文选择".into());
            }
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            }
        };
        selection_pixels(rig, stage, selection)?;
        unchanged(rig)?;
    }
    // 其余行内动作也必须经同一个View和真实完整帧；单独的Core标记测试不能证明渲染和选区正确。
    format_inline_variants(rig)?;
    // 回到旧短样本的原caret，再继续既有IME和两MiB负载；不修改后续输入/EOF/关闭门。
    let restored = observe(&rig.body)?;
    select(rig, 1..1)?;
    frame::wait_complete_frame("cell_format_restored_caret", &rig.body, restored)?;
    selection_pixels(
        rig,
        "cell_format_restored_caret",
        MarkdownTextSelection::caret(1),
    )?;
    unchanged(rig)
}

/// 固定短夹具覆盖其余七种行内格式；每次单步Undo还原原中文，不复制或缩小后续大草稿。
fn format_inline_variants(rig: &Rig) -> Result<(), String> {
    use MarkdownFormatAction as Action;
    for (action, expected, opening_units, stage) in [
        (Action::Italic, "*你*", 1, "cell_format_italic"),
        (Action::Underline, "<u>你</u>", 3, "cell_format_underline"),
        (Action::Strikethrough, "~~你~~", 2, "cell_format_strike"),
        (Action::InlineCode, "`你`", 1, "cell_format_inline_code"),
        (
            Action::InlineFormula,
            "$你$",
            1,
            "cell_format_inline_formula",
        ),
        (
            Action::Superscript,
            "<sup>你</sup>",
            5,
            "cell_format_superscript",
        ),
        (Action::Link, "[你](https://)", 1, "cell_format_link"),
    ] {
        // 上次真实Undo应仍选中原中文；每个动作都从同一个局部状态取得新权限，不能继承旧Scene。
        let before = selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            },
        )?;
        let rendered = observe(&rig.body)?;
        rig.view
            .apply_format(action)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, rendered)?;
        let after = selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: opening_units,
                head_utf16: opening_units + 1,
            },
        )?;
        // 只读取固定短结果到16单位栈块；真实正文、邻格和全部标记均不能由画面或格式器自身反推。
        let input = input(rig)?;
        let current = |epoch| epoch == EPOCH;
        let permit = input
            .capture(EPOCH, &current)
            .map_err(|cause| cause.to_string())?;
        let length = expected.encode_utf16().count();
        let mut actual = [0_u16; 16];
        input
            .copy_utf16_code_units(&permit, 0, &mut actual[..length], &current)
            .map_err(|cause| cause.to_string())?;
        if actual[..length].iter().copied().ne(expected.encode_utf16())
            || after.request.input.utf16_len != length
            || after.source == before.source
            || after.request.input.generation == before.request.input.generation
            || after.request.virtual_viewport.document_origin
                != before.request.virtual_viewport.document_origin
        {
            return Err(format!("{stage}: 行内格式的实际源、选区或视口不符"));
        }
        unchanged(rig)?; // 局部格式不能提前污染canonical或正文历史。

        // 实际Undo必须一次还原全部首尾标记；随后检查同一个CPU挂载中的中文选区，而非只核模型长度。
        let undone = observe(&rig.body)?;
        rig.view
            .command(sel!(undo:))
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, undone)?;
        selection_pixels(
            rig,
            stage,
            MarkdownTextSelection {
                anchor_utf16: 0,
                head_utf16: 1,
            },
        )?;
        if local::<1>(rig, 0)? != [0x4f60]
            || rig
                .view
                .input_state()
                .map_err(|cause| cause.to_string())?
                .utf16_len
                != 1
        {
            return Err(format!("{stage}: 行内格式没有在一次Undo后还原中文原文"));
        }
        unchanged(rig)?;
    }
    Ok(())
}

fn exercise(rig: &mut Rig, cancel: bool) -> Result<Option<Copied>, String> {
    let first = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    if first.request.input.utf16_len != 4 || local::<4>(rig, 0)? != [115, 101, 101, 100] {
        return Err("原Cell首次真实完整帧没有读取seed单格".into());
    }
    // 原NSTextInputClient先逐次marked候选，再确认中文；每次只借固定小载荷，不写canonical。
    select(rig, 0..4)?;
    let before_ime = rig.view.input_state().map_err(|cause| cause.to_string())?;
    for candidate in ["n", "ni"] {
        // SAFETY: 真实View/Input保活；选区相对有限marked载荷，NSNotFound消费原Cell选区。
        unsafe {
            client(rig).setMarkedText_selectedRange_replacementRange(
                &NSString::from_str(candidate),
                NSRange::new(candidate.len(), 0),
                unspecified(),
            );
        }
    }
    let ime = observe(&rig.body)?;
    // SAFETY: 与上方同一实际client确认输入，载荷仅一个中文scalar。
    unsafe {
        client(rig).insertText_replacementRange(&NSString::from_str("你"), unspecified());
    }
    frame::wait_complete_frame("cell_ime_frame", &rig.body, ime)?;
    if local::<1>(rig, 0)? != [0x4f60]
        || rig
            .view
            .input_state()
            .map_err(|c| c.to_string())?
            .marked
            .is_some()
    {
        return Err("Cell真实IME确认没有替换原marked候选".into());
    }
    selection_pixels(rig, "cell_ime_caret", MarkdownTextSelection::caret(1))?;
    unchanged(rig)?;
    // 同一原生标准命令只操作局部历史，不允许冒泡到canonical或建立第二Undo manager。
    for (selector, stage) in [
        (sel!(undo:), "cell_local_undo"),
        (sel!(redo:), "cell_local_redo"),
    ] {
        let frame = observe(&rig.body)?;
        // SAFETY: 真实NSTextInputClient把标准selector交同一View命令入口，没有新快捷键。
        unsafe {
            client(rig).doCommandBySelector(selector);
        }
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let actual = rig.view.input_state().map_err(|cause| cause.to_string())?;
        if (selector == sel!(undo:)
            && (actual.utf16_len != 4 || local::<4>(rig, 0)? != [115, 101, 101, 100]))
            || (selector == sel!(redo:)
                && (actual.utf16_len != 1 || local::<1>(rig, 0)? != [0x4f60]))
        {
            return Err("Cell原生Undo/Redo没有恢复实际局部内容".into());
        }
        let restored = if selector == sel!(undo:) {
            before_ime.selection
        } else {
            MarkdownTextSelection::caret(1)
        };
        selection_pixels(rig, stage, restored)?;
        unchanged(rig)?;
    }
    if local::<1>(rig, 0)? != [0x4f60] {
        return Err("Cell本地Redo没有恢复中文草稿".into());
    }
    format_selection(rig)?;
    let marked = observe(&rig.body)?;
    // SAFETY: 有限同字marked输入仍是实际IME路径；后续cancelOperation只能unmark，不关闭Cell。
    unsafe {
        client(rig).setMarkedText_selectedRange_replacementRange(
            &NSString::from_str("你"),
            NSRange::new(1, 0),
            NSRange::new(0, 1),
        );
    }
    frame::wait_complete_frame("cell_marked_caret", &rig.body, marked)?;
    selection_pixels(rig, "cell_marked_caret", MarkdownTextSelection::caret(1))?;
    let unmarked = observe(&rig.body)?;
    rig.view
        .command(sel!(cancelOperation:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_unmarked_caret", &rig.body, unmarked)?;
    selection_pixels(rig, "cell_unmarked_caret", MarkdownTextSelection::caret(1))?;
    if rig
        .view
        .input_state()
        .map_err(|cause| cause.to_string())?
        .marked
        .is_some()
    {
        return Err("原Cell unmark未结束组合".into());
    }
    let context = observe(&rig.body)?;
    select(rig, 0..1)?;
    // SAFETY: 真实原生入口先形成含前后邻字的有限草稿，避免整格复制也能通过部分复制期待。
    unsafe {
        client(rig).insertText_replacementRange(&NSString::from_str("前你后"), unspecified());
    }
    frame::wait_complete_frame("cell_partial_context", &rig.body, context)?;
    select(rig, 2..3)?;
    let deleted = observe(&rig.body)?;
    rig.view
        .command(sel!(deleteBackward:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_deleted_caret", &rig.body, deleted)?;
    selection_pixels(rig, "cell_deleted_caret", MarkdownTextSelection::caret(2))?;
    if local::<2>(rig, 0)? != [0x524d, 0x4f60] {
        return Err("原Cell删除没有只删除选中的后字".into());
    }
    let restored = observe(&rig.body)?;
    rig.view
        .command(sel!(undo:))
        .map_err(|cause| cause.to_string())?;
    frame::wait_complete_frame("cell_delete_undo_caret", &rig.body, restored)?;
    if local::<3>(rig, 0)? != [0x524d, 0x4f60, 0x540e] {
        return Err("原Cell删除Undo没有恢复前你后".into());
    }
    // 删除Undo恢复的是原非空选择，活动端仍由同receipt reveal；这里只核范围真实可见，不伪caret。
    let restored_key = rig
        .view
        .capture_ready_scene_key()
        .map_err(|cause| cause.to_string())?;
    rig.view
        .verify_find_target_visible(
            &restored_key,
            NSRange::new(restored_key.request.input.selection.head_utf16, 0),
        )
        .map_err(|cause| cause.to_string())?;
    let selected = observe(&rig.body)?;
    select(rig, 1..2)?;
    frame::wait_complete_frame("cell_partial_selected", &rig.body, selected)?;
    drop(copy(rig, false)?);

    let large = observe(&rig.body)?;
    select(rig, 0..3)?; // 下一次真实输入替换完整有限旧草稿，保留两MiB负载的纯x精确期待。
    let payload = NSString::from_str(&"x".repeat(SCRATCH)); // 唯一固定64KiB新输入载荷，不读取或复制旧草稿。
    for _ in 0..LARGE / SCRATCH {
        // SAFETY: 原client每次消费当前实际选区；第一次替换中文，后续追加同一固定载荷。
        unsafe {
            client(rig).insertText_replacementRange(&payload, unspecified());
        }
    }
    drop(payload); // 新输入载荷在等待首帧前归还，产品仅保原delta段。
    frame::wait_complete_frame("cell_two_mebibyte_eof_frame", &rig.body, large)?;
    let eof = selection_pixels(
        rig,
        "cell_two_mebibyte_input_eof",
        MarkdownTextSelection::caret(LARGE),
    )?;
    let state = rig.view.input_state().map_err(|cause| cause.to_string())?;
    if state.utf16_len != LARGE {
        return Err("真实Cell没有完整接受两MiB合法草稿".into());
    }
    for offset in [0, LARGE / 2, LARGE - 4096] {
        if local::<4096>(rig, offset)?
            .iter()
            .any(|unit| *unit != u16::from(b'x'))
        {
            return Err("大Cell有限首中尾读取不符".into());
        }
    }
    unchanged(rig)?;
    // 真实标准端点命令沿同一大草稿Source往返，不能靠整段重读或文首前缀冒充EOF。
    for (selector, stage, head) in [
        (sel!(moveToBeginningOfDocument:), "cell_document_start", 0),
        (sel!(moveToEndOfDocument:), "cell_document_end", LARGE),
    ] {
        let frame = observe(&rig.body)?;
        rig.view
            .command(selector)
            .map_err(|cause| cause.to_string())?;
        frame::wait_complete_frame(stage, &rig.body, frame)?;
        let key = selection_pixels(rig, stage, MarkdownTextSelection::caret(head))?;
        if key.source != eof.source || key.request.input.generation != eof.request.input.generation
        {
            return Err("Cell端点往返没有沿原大草稿Source与内容版本".into());
        }
    }
    rig.remember_receipt()?;
    let original = rig.cell.as_ref().ok_or("原Cell owner已释放")?;
    unchanged(rig)?;
    let selected = observe(&rig.body)?;
    select(rig, 0..LARGE)?;
    frame::wait_complete_frame("cell_two_mebibyte_selected", &rig.body, selected)?;
    let output = copy(rig, true)?;
    println!(
        "native_body_cell stage=two_mebibyte actual_utf8_bytes={LARGE} input=NstextInputClient fixed_payload_bytes={SCRATCH} canonical_unchanged=true temporary_copy_reader_returned=true resources={:?} formal=false whole_product=false",
        original.resources.snapshot()
    );
    if cancel {
        // 真实取消只结束原Cell；关闭由统一Rig释放检查承担，不向用户要求退出应用。
        rig.view
            .command(sel!(cancelOperation:))
            .map_err(|cause| cause.to_string())?;
        if !original.resources.is_released() {
            return Err("原生Cell取消没有归还实际Scene/Producer/Font读取者和草稿".into());
        }
        unchanged(rig)?;
        return Ok(Some(output));
    }
    // 完整长草稿保持原样进入实际提交；不能先缩短再把小格成功冒充大格已经可保存。
    rig.remember_receipt()?;
    let original = rig.cell.as_ref().ok_or("原Cell owner已释放")?;
    let before = original
        .canonical
        .state()
        .map_err(|cause| cause.to_string())?;
    let permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let outcome = rig
        .body
        .commit_cell_to_body(&original.canonical, &permit, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
        return Err("两MiBCell没有沿原canonical Input交付真实提交".into());
    };
    commit
        .check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    verify_large_canonical(original, 1)?;
    if !original.resources.is_released() {
        return Err("两MiB提交后原草稿、读取者或准入候选未实际归还".into());
    }
    let undo_permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let undo = original
        .canonical
        .history(
            &undo_permit,
            NativeMarkdownPagedInputHistory::Undo,
            &|epoch| epoch == EPOCH,
        )
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(undo) = undo else {
        return Err("原canonical observed Undo没有真实提交".into());
    };
    undo.check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    {
        let doc = original.document.borrow();
        let mut restored = [0_u16; 64];
        let len = doc.utf16_len();
        if len != SOURCE.encode_utf16().count() {
            return Err("一次Undo没有恢复原有限canonical长度".into());
        } // 先核真实长度，再借有限原文缓冲；失败不能越界读取或分配长篇副本。
        doc.copy_utf16_code_units(0, &mut restored[..len])
            .map_err(|cause| cause.to_string())?;
        if doc.generation() != 2
            || doc.byte_len() != SOURCE.len()
            || doc.selection() != before.selection
            || doc.can_undo()
            || !doc.can_redo()
            || restored[..len].iter().copied().ne(SOURCE.encode_utf16())
        {
            return Err("局部Cell历史被错误发布为多次canonical Undo或未还原原正文".into());
        }
    }
    let redo_permit = original
        .canonical
        .capture(EPOCH, &|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    let redo = original
        .canonical
        .history(
            &redo_permit,
            NativeMarkdownPagedInputHistory::Redo,
            &|epoch| epoch == EPOCH,
        )
        .map_err(|cause| cause.to_string())?;
    let NativeMarkdownPagedInputOutcome::Committed(redo) = redo else {
        return Err("原canonical observed Redo没有真实提交".into());
    };
    redo.check_current(&|epoch| epoch == EPOCH)
        .map_err(|cause| cause.to_string())?;
    verify_large_canonical(original, 3)?;
    if !original.resources.is_released() {
        return Err("canonical Undo/Redo重新保活了已归还的Cell草稿或准入候选".into());
    }
    println!(
        "native_body_cell stage=two_mebibyte_committed actual_utf8_bytes={LARGE} canonical_bounded_exact=true canonical_single_undo_redo=true parent_doc_alive=true actual_cell_released=true resources={:?} formal=false whole_product=false",
        original.resources.snapshot(),
    );
    Ok(Some(output))
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    for cancel in [false, true] {
        let (retirement, result, output) = autoreleasepool(|_| -> Result<_, String> {
            let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
            file.write_all(SOURCE.as_bytes())
                .map_err(|cause| cause.to_string())?;
            let mut rig = Rig::attach_target(mtm, file, SOURCE.encode_utf16().count(), 4, true)?;
            let result = word::run(&mut rig).and_then(|()| exercise(&mut rig, cancel));
            let (result, output) = match result {
                Ok(output) => (Ok(()), output),
                Err(cause) => (Err(cause), None),
            };
            let retirement = release::close(rig, false)?;
            Ok((retirement, result, output))
        })?;
        retirement.verify(false)?;
        if let Some(output) = output {
            // 原Doc/Cell/Reader/Native图已实际归还，用户显式输出仍必须保持完整可读。
            let retained = verify_output(&output.data, true);
            drop(output);
            retained?;
        }
        result?;
    }
    Ok(())
}
