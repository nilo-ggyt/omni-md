//! 正式worker的不可消费前缀交付；快照只读旧LineEnd，producer仍从原cursor继续。

use super::{engine::composer, *};
use crate::MarkdownShapedGlyph;

const ZERO: MarkdownShapedGlyph = MarkdownShapedGlyph {
    glyph_id: 0,
    source_utf16_cluster: 0,
    flags: 0,
    x_advance: 0,
    y_advance: 0,
    x_offset: 0,
    y_offset: 0,
};

fn next_line(
    worker: &mut MarkdownLineComposer,
    font: &MarkdownLineRegisteredFont,
) -> Option<MarkdownComposedLineFacts> {
    for _ in 0..100_000 {
        match worker.step(budget()).unwrap().answer {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(request.respond_registered(range, font, style()).unwrap())
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(facts) => return Some(facts),
            MarkdownLineComposeAnswer::Complete => return None,
            _ => panic!("有限无控制自然换行夹具不能遗漏真实待处理阶段"),
        }
    }
    panic!("有限worker未签出下一条LineEnd");
}

#[test]
fn committed_prefixes_survive_append_and_normal_writer_close_with_independent_fd_receipts() {
    let fixture = Fixture::new(&"office office office office ".repeat(8));
    let backing = fixture.source.original_backing_observer().unwrap();
    let source = fixture.source.resource_observer();
    let (mut worker, font) = composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    let writer_receipt = worker.resource_observer().unwrap();
    assert!(worker.publish_ready_prefix().unwrap().is_none());
    let first = next_line(&mut worker, &font).unwrap();
    let mut old = worker.publish_ready_prefix().unwrap().unwrap();
    let old_receipt = old.resource_observer();
    let old_line = old.last_line().unwrap().unwrap();
    let old_part = old.part(&old_line, 0).unwrap().unwrap();
    let mut glyphs = vec![ZERO; old_part.facts().glyphs];
    old.read_batch(&old_part, 0..glyphs.len(), &mut glyphs)
        .unwrap();
    assert_eq!(old.source_range(), first.source_utf16);
    assert_eq!(old_receipt.snapshot().mapped_bytes, 0);
    assert!(old_receipt.snapshot().file_open);
    assert_eq!(writer_receipt.snapshot().mapped_bytes, 0); // 发布确实checkpoint原页，不是假Reader零映射。

    // 后继LineEnd真实追加到同文件；旧reader的last/count与逻辑EOF不能随inode长度变大。
    let second = next_line(&mut worker, &font).unwrap();
    assert_eq!(second.source_utf16.start, first.source_utf16.end);
    let mut candidate = worker.publish_ready_prefix().unwrap().unwrap();
    let candidate_receipt = candidate.resource_observer();
    assert_eq!(candidate.source_range().end, second.source_utf16.end);
    assert!(matches!(
        worker.publish_ready_prefix(),
        Err(MarkdownLineComposeError::SnapshotBusy)
    ));
    let third = next_line(&mut worker, &font).unwrap();
    assert_eq!(third.source_utf16.start, second.source_utf16.end);
    let unchanged = old.last_line().unwrap().unwrap();
    assert_eq!(unchanged.facts(), &first);
    assert!(old.previous_line(&unchanged).unwrap().is_none());
    let mut after = vec![ZERO; glyphs.len()];
    old.read_batch(&old_part, 0..after.len(), &mut after)
        .unwrap();
    assert_eq!(glyphs, after);
    old.close().unwrap();
    assert!(!old_receipt.snapshot().file_open);
    let newer = worker.publish_ready_prefix().unwrap().unwrap();
    let newer_receipt = newer.resource_observer();
    assert_eq!(newer.source_range().end, third.source_utf16.end);

    // writer正常终结不撤已发布的不可变reader；真实源一直由这些值持有到最后close。
    while next_line(&mut worker, &font).is_some() {}
    let final_output = worker.into_output().unwrap();
    let final_receipt = final_output.resource_observer();
    final_output.close().unwrap();
    assert!(!writer_receipt.snapshot().file_open);
    drop(fixture);
    assert!(!source.payload_released());
    assert!(backing.mapped_bytes() > 0);
    let actual = candidate.last_line().unwrap().unwrap();
    assert_eq!(actual.facts(), &second);
    let part = candidate.part(&actual, 0).unwrap().unwrap();
    candidate.read_batch(&part, 0..0, &mut []).unwrap();
    candidate.close().unwrap();
    newer.close().unwrap();
    for receipt in [candidate_receipt, newer_receipt, final_receipt] {
        let state = receipt.snapshot();
        assert!(!state.file_open && state.mapped_bytes == 0);
        assert!(!state.cleanup_failed && !state.reader_failed);
    }
    assert!(source.payload_released());
    assert!(backing.unmap_succeeded());
    assert_eq!(backing.mapped_bytes(), 0);
    assert!(!backing.failed());
}

#[test]
fn each_prefix_has_fresh_output_authority_and_cancel_revokes_worker_and_old_reader() {
    let fixture = Fixture::new("office office office office office");
    let cancellation = MarkdownLineComposeCancellation::new();
    let (mut worker, font) = composer(&fixture, 80 * 64, cancellation.clone());
    next_line(&mut worker, &font).unwrap();
    let mut first = worker.publish_ready_prefix().unwrap().unwrap();
    let mut second = worker.publish_ready_prefix().unwrap().unwrap();
    let line = first.last_line().unwrap().unwrap();
    let receipt = second.resource_observer();
    assert!(matches!(
        second.part(&line, 0),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    assert!(!receipt.snapshot().file_open); // 错用只归还该reader，健康writer及另一份输出仍有效。
    first.last_line().unwrap().unwrap();
    let replacement = worker.publish_ready_prefix().unwrap().unwrap();
    cancellation.cancel();
    assert!(matches!(
        worker.publish_ready_prefix(),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    assert!(matches!(
        first.last_line(),
        Err(MarkdownLineComposeError::Cancelled)
    ));
    drop(replacement);
}

#[test]
fn source_first_error_overrides_reader_backpressure_and_old_prefix_cache_hits() {
    let fixture = Fixture::new("office office office office office");
    let (mut worker, font) = composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    next_line(&mut worker, &font).unwrap();
    let mut first = worker.publish_ready_prefix().unwrap().unwrap();
    let second = worker.publish_ready_prefix().unwrap().unwrap();
    let first_receipt = first.resource_observer();
    let second_receipt = second.resource_observer();
    first.last_line().unwrap().unwrap();
    // 从同一个真实Backing装页后触发实际回收；不是仅把输出权限位改成失败。
    fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
    fixture.document.fail_next_original_reclaim_for_test();
    assert!(matches!(
        fixture.document.reclaim_original_read_windows_for_test(),
        Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
    ));
    assert!(matches!(
        worker.publish_ready_prefix(),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    )); // 名额仍满时也不能用SnapshotBusy掩盖原Source首错。
    assert!(matches!(
        first.last_line(),
        Err(MarkdownLineComposeError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop(second);
    for receipt in [first_receipt, second_receipt] {
        let state = receipt.snapshot();
        assert!(!state.file_open && state.mapped_bytes == 0);
        assert_eq!(state.first_source_errno, Some(libc::EIO));
    }
}
