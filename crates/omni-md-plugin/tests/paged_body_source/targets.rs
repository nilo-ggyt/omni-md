//! 实际远端目标、旧前缀回溯和输入/宽度撤权；仍用原三秒帧事件与完整关闭账本。

use super::*;
use omni_md_plugin::native_surface::{
    paged_body::{
        NativeMarkdownPagedBodyError, NativeMarkdownPagedBodyResourceReceipts,
        NativeMarkdownPagedBodySourceTarget,
    },
    paged_text_view::NativeMarkdownPagedTextView,
};

pub(super) fn run(
    body: &NativeMarkdownPagedBody,
    window: &NSWindow,
    scroll: &NSScrollView,
    view: &NativeMarkdownPagedTextView,
    document: &std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    receipts: &mut Vec<NativeMarkdownPagedBodyResourceReceipts>,
    fixture: SourceFixture,
) -> Vec<NativeMarkdownPagedBodySourceTarget> {
    let mut retained = Vec::new();
    let original = view.capture_ready_scene_key().unwrap();
    let before = body.progress().unwrap().prepared_lines;
    for (stage, start) in [
        ("source_target_beyond_64k", 70_001),
        ("source_target_older_prefix", 35_001),
        ("source_target_return_to_top", 1),
    ] {
        let frame = observe(body, Instant::now());
        let range = NSRange::new(start, 9_000);
        let target = body.request_source_target(range).unwrap();
        assert!(target.is_pending());
        let progress = deliver_frame(stage, body, frame);
        let proof = body.take_source_target_visible(&target).unwrap().unwrap();
        assert!(!target.is_pending());
        assert_eq!(proof.range(), range);
        let key = view.capture_ready_scene_key().unwrap();
        assert!(proof.key().same(&key));
        assert!(key.source == original.source);
        assert_eq!(key.request.input, original.request.input); // Reveal不能偷偷改选区或制造编辑事务。
        view.verify_find_target_visible(proof.key(), range).unwrap();
        assert!(!progress.complete);
        assert!(progress.prepared_lines > before);
        assert!(
            progress.frontier_source_bytes < SOURCE_BYTES
                && progress.shaped_input_bytes < SOURCE_BYTES
        );
        if start == 70_001 {
            // 此时此前只准备了顶部数屏。真正首scalar进入可见区后，原9000单位匹配
            // 尾端仍未进入frontier/shaper，不能用“少于整篇”替代首位置懒准备的证据。
            let match_end = start + range.length;
            let match_end_bytes = fixture.edited_byte_end(match_end);
            assert!(progress.frontier_source_bytes < match_end_bytes);
            assert!(progress.shaped_input_bytes < match_end_bytes);
            println!(
                "paged_body_source stage={stage} fixture={} lazy_match_end_utf16={match_end} lazy_match_end_bytes={match_end_bytes} only_first_scalar_required=true formal=false",
                fixture.name(),
            );
            assert!(progress.target_checkpoint_attempts > 0);
            assert!(progress.target_checkpoint_attempts < progress.prepared_lines - before);
            assert!(progress.target_max_batch_lines > 1 && progress.target_max_batch_lines <= 32);
            assert_eq!(progress.target_unmeasured_lines, 0);
        }
        if start == 1 {
            assert!(key.request.virtual_viewport.document_origin.y < 20.0);
        } else {
            assert!(key.request.virtual_viewport.document_origin.y > 0.0);
        }
        println!(
            "paged_body_source stage={stage} actual_target_start={start} actual_target_first_scalar_visible=true original_source_preserved=true target_lookup_records={} target_metric_parts={} target_walk_lines={} target_checkpoint_attempts={} target_checkpoint_millis={:.3} target_max_batch_lines={} target_batch_index_records={} formal=false whole_product=false",
            progress.target_lookup_records,
            progress.target_metric_parts,
            progress.target_walk_lines,
            progress.target_checkpoint_attempts,
            progress.target_checkpoint_millis,
            progress.target_max_batch_lines,
            progress.target_batch_index_records,
        );
        receipts.push(body.resource_receipts().unwrap());
        retained.push(target); // 已完成票保留到Body关闭以后，不能靠先drop外部票通过资源门。
    }
    let edit_origin = view.capture_ready_scene_key().unwrap();
    let before_edit = view.input_state().unwrap();
    let before_edit_serial = body.progress().unwrap().producer_serial;
    let editing = observe(body, Instant::now());
    let obsolete = body.request_source_target(NSRange::new(80_001, 1)).unwrap();
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(view);
    // SAFETY: 合成数据的真实原client和同步NSString；仍经过原prepared canonical事务。
    unsafe {
        client.insertText_replacementRange(&NSString::from_str("R"), NSRange::new(0, 0));
    }
    deliver("source_target_revoked_by_edit", body, editing);
    assert!(matches!(
        body.take_source_target_visible(&obsolete),
        Err(NativeMarkdownPagedBodyError::View(
            NativeMarkdownPagedTextViewError::Revoked
        ))
    ));
    assert!(!obsolete.is_pending());
    let edited = view.capture_ready_scene_key().unwrap();
    assert!(edited.source != edit_origin.source);
    assert!(!edited.same(&edit_origin));
    assert_eq!(
        edited.request.input.generation,
        before_edit.generation.checked_add(1).unwrap()
    );
    assert_eq!(edited.request.input.utf16_len, before_edit.utf16_len + 1);
    assert_eq!(edited.request.input, view.input_state().unwrap());
    assert!(body.progress().unwrap().producer_serial > before_edit_serial);
    view.verify_find_target_visible(&edited, NSRange::new(0, 1))
        .unwrap();
    let actual_document = document.upgrade().unwrap();
    let mut first = [0];
    actual_document
        .borrow()
        .copy_utf16_code_units(0, &mut first)
        .unwrap();
    assert_eq!(first, [b'R' as u16]);
    drop(actual_document);
    receipts.push(body.resource_receipts().unwrap());
    let resize_origin = view.capture_ready_scene_key().unwrap();
    let old_width = view.bounds().size.width;
    let resizing = observe(body, Instant::now());
    let obsolete_width = body.request_source_target(NSRange::new(80_001, 1)).unwrap();
    let content = scroll.frame().size;
    window.setContentSize(NSSize::new(content.width - 80.0, content.height));
    window.displayIfNeeded();
    deliver("source_target_revoked_by_resize", body, resizing);
    assert!(matches!(
        body.take_source_target_visible(&obsolete_width),
        Err(NativeMarkdownPagedBodyError::View(
            NativeMarkdownPagedTextViewError::Revoked
        ))
    ));
    assert!(!obsolete_width.is_pending());
    assert!(view.bounds().size.width < old_width);
    let resized = view.capture_ready_scene_key().unwrap();
    assert!(resized.source != resize_origin.source);
    assert!(!resized.same(&resize_origin));
    assert_eq!(resized.request.bounds_width, view.bounds().size.width);
    assert!(resized.request.bounds_width < resize_origin.request.bounds_width);
    assert_eq!(resized.request.input, resize_origin.request.input); // 只有布局变更，不能额外编辑或改变选择。
    view.verify_find_target_visible(&resized, NSRange::new(0, 1))
        .unwrap();
    receipts.push(body.resource_receipts().unwrap());
    // 旧请求一直保留到撤权后的真实后继帧，证明失败不是把仍待完成的意图静默丢掉。
    retained.extend([obsolete, obsolete_width]);
    let closing = body.request_source_target(NSRange::new(90_001, 1)).unwrap();
    assert!(closing.is_pending());
    retained.push(closing); // 原队列尚未运行即close；外部原pending票也必须随真实业务关闭准确撤权。
    retained
}
