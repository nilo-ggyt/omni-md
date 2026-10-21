//! Publish a prefix without consuming the production worker. Its snapshot reads
//! only the original sealed LineEnd while the producer continues the same cursor.

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
    // Advance the actual original composition/response chain until a committed line
    // or real completion, without constructing a Store or a publication identity.
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

#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn plain_selection_change_keeps_real_glyph_prefix_and_continues_fresh_publication() {
    use crate::markdown_document_buffer::MarkdownTableRequestError;
    use crate::{
        MarkdownOrdinarySemanticState, MarkdownTextSelection, MarkdownViewportSourceAuthority,
    };

    // Prepare actual semantics and readable glyphs from one real document first.
    // Selection changes only inside a proven plain range. Original factories/Source
    // issue all capabilities; no private composer state is manufactured.
    let text = "office abc office xyz ".repeat(20);
    let mut fixture = Fixture::new(&text);
    let backing = fixture.source.original_backing_observer().unwrap();
    let source = fixture.source.resource_observer();
    let original = fixture.document.selection();
    let authority = |document: &MarkdownDocumentBuffer| MarkdownViewportSourceAuthority {
        context_revision: 73,
        selection: document.selection(),
        cancelled: false,
    };
    let finish = |request: &mut crate::MarkdownViewportSourceRequest,
                  document: &MarkdownDocumentBuffer| {
        for _ in 0..text.len() * 100 + 10000 {
            request
                .advance(document, authority(document), 4096)
                .unwrap();
            if request
                .sources_for(document, authority(document))
                .unwrap()
                .is_some()
            {
                return;
            }
        }
        panic!("真实短段来源未在原量子内发布");
    };
    let mut request = fixture
        .document
        .viewport_leaf_source_request(0..64, 73, original)
        .unwrap();
    finish(&mut request, &fixture.document);
    let mut previous = request
        .take_sources_for(&fixture.document, authority(&fixture.document))
        .unwrap()
        .unwrap();
    assert!(matches!(
        previous
            .ordinary_semantics_for(&fixture.document, authority(&fixture.document))
            .unwrap(),
        MarkdownOrdinarySemanticState::RootLeafReady(_)
    ));
    let (mut worker, font) = composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let writer = worker.resource_observer().unwrap();
    let first = next_line(&mut worker, &font).unwrap();
    let mut old = worker.publish_ready_prefix().unwrap().unwrap();
    let old_receipt = old.resource_observer();
    let old_line = old.last_line().unwrap().unwrap();
    let old_part = old.part(&old_line, 0).unwrap().unwrap();
    let mut expected = vec![ZERO; old_part.facts().glyphs];
    old.read_batch(&old_part, 0..expected.len(), &mut expected)
        .unwrap();
    assert!(expected.iter().all(|glyph| glyph.glyph_id != 0));

    let changed = MarkdownTextSelection {
        anchor_utf16: 27,
        head_utf16: 11,
    };
    fixture.document.set_selection(changed).unwrap();
    {
        // The original item prefetched later text for Measure. A visible first
        // line inside 64 characters does not extend that finite semantic proof to
        // the paragraph. After refusal, restore the real original selection so its
        // same publication can continue normal forward preparation.
        assert!(first.source_utf16.end <= 64);
        let capability = request
            .selection_style_invariant(&fixture.document, &previous, authority(&fixture.document))
            .unwrap()
            .expect("真实首窗已有有限 Plain 证明");
        assert!(!worker.can_retain_selection_styles(&capability).unwrap());
    }
    fixture.document.set_selection(original).unwrap();
    request
        .continue_forward(
            &fixture.document,
            &previous,
            64..text.len(),
            authority(&fixture.document),
        )
        .unwrap();
    drop(previous);
    finish(&mut request, &fixture.document);
    previous = request
        .take_sources_for(&fixture.document, authority(&fixture.document))
        .unwrap()
        .unwrap();
    fixture.document.set_selection(changed).unwrap();
    {
        let capability = request
            .selection_style_invariant(&fixture.document, &previous, authority(&fixture.document))
            .unwrap()
            .expect("原生产 Plain 证明必须覆盖完整实际段落");
        assert!(
            worker.can_retain_selection_styles(&capability).unwrap(),
            "真实已发布字形必须可在同一能力内继续，不重造 writer"
        );
    }
    assert!(
        request
            .reproject_selection(
                &fixture.document,
                &previous,
                64..text.len(),
                authority(&fixture.document),
            )
            .unwrap()
    );
    // Retire the old publication itself. Returning to equal anchor/head coordinates
    // cannot revive its superseded semantic authority.
    fixture.document.set_selection(original).unwrap();
    assert!(matches!(
        previous.ordinary_semantics_for(&fixture.document, authority(&fixture.document)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    fixture.document.set_selection(changed).unwrap();
    drop(previous);
    old.close().unwrap();
    assert!(!old_receipt.snapshot().file_open);
    finish(&mut request, &fixture.document);
    let current = request
        .take_sources_for(&fixture.document, authority(&fixture.document))
        .unwrap()
        .unwrap();

    // The same Store publishes an independent reader with identical actual glyphs.
    // An old weak Line must not be accepted by that new output identity.
    let mut fresh = worker.publish_ready_prefix().unwrap().unwrap();
    let fresh_receipt = fresh.resource_observer();
    let line = fresh.last_line().unwrap().unwrap();
    assert_eq!(line.facts(), &first);
    let part = fresh.part(&line, 0).unwrap().unwrap();
    let mut actual = vec![ZERO; part.facts().glyphs];
    fresh
        .read_batch(&part, 0..actual.len(), &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    assert!(matches!(
        fresh.part(&old_line, 0),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    // This actual foreign-Line error retires that reader. Do not continue reading
    // the failed output or report a normal explicit close for it.
    drop(fresh);
    assert!(!fresh_receipt.snapshot().file_open);
    let second = next_line(&mut worker, &font).unwrap();
    assert_eq!(second.source_utf16.start, first.source_utf16.end);
    while next_line(&mut worker, &font).is_some() {}
    let final_output = worker.into_output().unwrap();
    assert_eq!(final_output.source_range(), 0..text.len());
    final_output.close().unwrap();
    assert!(!writer.snapshot().file_open);
    assert_eq!(fixture.document.generation(), 0);
    assert_eq!(fixture.document.selection(), changed);
    assert!(
        !fixture.document.is_dirty()
            && !fixture.document.can_undo()
            && !fixture.document.can_redo()
    );
    assert_eq!(
        std::fs::read(fixture._directory.path().join("真实分项.md")).unwrap(),
        text.as_bytes()
    );
    drop((current, request, font, fixture));
    assert!(resources.is_released());
    assert!(source.payload_released());
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn root_code_selection_keeps_preceding_real_glyphs_with_fresh_output_authority() {
    use crate::markdown_document_buffer::MarkdownTableRequestError;
    use crate::{MarkdownTextSelection, MarkdownViewportSourceAuthority};

    // The actual finite semantic window lies in later Code body while published
    // glyphs/the complete natural item lie in preceding plain text. This proves
    // cross-owner prefix retention rather than merely a Code-body intersection.
    let prefix = "office abc office xyz ".repeat(20);
    let text = format!("{prefix}\n\n```text\nliteral alpha omega\n```\n");
    let first = text.find("literal").unwrap();
    let second = text.find("omega").unwrap();
    let mut fixture = Fixture::new(&text);
    let mapping = fixture.source.original_backing_observer().unwrap();
    let source = fixture.source.resource_observer();
    let original = MarkdownTextSelection::caret(first + 1);
    fixture.document.set_selection(original).unwrap();
    let authority = |document: &MarkdownDocumentBuffer| MarkdownViewportSourceAuthority {
        context_revision: 73,
        selection: document.selection(),
        cancelled: false,
    };
    let finish = |request: &mut crate::MarkdownViewportSourceRequest,
                  document: &MarkdownDocumentBuffer| {
        for _ in 0..text.len() * 100 + 10000 {
            request
                .advance(document, authority(document), 4096)
                .unwrap();
            if request
                .sources_for(document, authority(document))
                .unwrap()
                .is_some()
            {
                return;
            }
        }
        panic!("真实Code窗口没有沿原有限Source量子完成");
    };
    let mut request = fixture
        .document
        .viewport_leaf_source_request(first..first + 7, 73, original)
        .unwrap();
    finish(&mut request, &fixture.document);
    let previous = request
        .take_sources_for(&fixture.document, authority(&fixture.document))
        .unwrap()
        .unwrap();
    let (mut worker, font) = composer(&fixture, 80 * 64, MarkdownLineComposeCancellation::new());
    let resources = worker.worker_resource_observer();
    let writer = worker.resource_observer().unwrap();
    let first_line = next_line(&mut worker, &font).unwrap();
    assert!(first_line.source_utf16.end < prefix.len());
    let mut old = worker.publish_ready_prefix().unwrap().unwrap();
    let old_receipt = old.resource_observer();
    let old_line = old.last_line().unwrap().unwrap();
    let old_part = old.part(&old_line, 0).unwrap().unwrap();
    let mut expected = vec![ZERO; old_part.facts().glyphs];
    old.read_batch(&old_part, 0..expected.len(), &mut expected)
        .unwrap();
    assert!(expected.iter().all(|glyph| glyph.glyph_id != 0));

    // Move selection to another real word in the same Code body. SourceRequest
    // checks original root/scope/body/full selection; composer separately verifies
    // its quiescent boundary, fonts, and all ahead support. No Store is mutated.
    let changed = MarkdownTextSelection {
        anchor_utf16: second + 3,
        head_utf16: second,
    };
    fixture.document.set_selection(changed).unwrap();
    {
        let capability = request
            .selection_style_invariant(&fixture.document, &previous, authority(&fixture.document))
            .unwrap()
            .expect("同一Root Code正文内的选择应有真实不变能力");
        assert!(worker.can_retain_selection_styles(&capability).unwrap());
    }
    assert!(
        request
            .reproject_selection(
                &fixture.document,
                &previous,
                first..first + 7,
                authority(&fixture.document)
            )
            .unwrap()
    );
    fixture.document.set_selection(original).unwrap();
    assert!(matches!(
        previous.ordinary_semantics_for(&fixture.document, authority(&fixture.document)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    fixture.document.set_selection(changed).unwrap();
    drop(previous);
    old.close().unwrap();
    assert!(!old_receipt.snapshot().file_open);
    finish(&mut request, &fixture.document);
    let current = request
        .take_sources_for(&fixture.document, authority(&fixture.document))
        .unwrap()
        .unwrap();

    // A new reader from the same writer must return identical glyph records but
    // reject old weak Line identity. Public output proves reuse; retention counts,
    // private directories, or an old reader staying readable do not establish it.
    let mut fresh = worker.publish_ready_prefix().unwrap().unwrap();
    let fresh_receipt = fresh.resource_observer();
    let line = fresh.last_line().unwrap().unwrap();
    assert_eq!(line.facts(), &first_line);
    let part = fresh.part(&line, 0).unwrap().unwrap();
    let mut actual = vec![ZERO; part.facts().glyphs];
    fresh
        .read_batch(&part, 0..actual.len(), &mut actual)
        .unwrap();
    assert_eq!(actual, expected);
    assert!(matches!(
        fresh.part(&old_line, 0),
        Err(MarkdownLineComposeError::SourceMismatch)
    ));
    drop(fresh);
    assert!(!fresh_receipt.snapshot().file_open);
    let next = next_line(&mut worker, &font).unwrap();
    assert_eq!(next.source_utf16.start, first_line.source_utf16.end);
    let output = worker.into_output().unwrap();
    assert_eq!(output.source_range().end, next.source_utf16.end);
    assert!(output.source_range().end < prefix.len());
    output.close().unwrap();
    assert!(!writer.snapshot().file_open);
    assert_eq!(fixture.document.selection(), changed);
    assert_eq!(fixture.document.generation(), 0);
    assert!(
        !fixture.document.is_dirty()
            && !fixture.document.can_undo()
            && !fixture.document.can_redo()
    );
    assert_eq!(
        std::fs::read(fixture._directory.path().join("真实分项.md")).unwrap(),
        text.as_bytes()
    );
    drop((current, request, font, fixture));
    assert!(resources.is_released());
    assert!(source.payload_released());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert_eq!(mapping.managed_resident_bytes(), 0);
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
    assert_eq!(writer_receipt.snapshot().mapped_bytes, 0); // Actual publication checkpointed the writer page.

    // Actual later LineEnds append to the same file. Growing its inode cannot
    // extend an old reader's sealed last/count or logical EOF.
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

    // Normal writer retirement preserves already published immutable readers.
    // Their actual Source claims remain alive until the final explicit close.
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
    assert!(!receipt.snapshot().file_open); // Retire only the misused reader; other real owners stay healthy.
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
    // Make a real original backing page resident, then exercise reclaim with the
    // existing EIO injection. No private output permission is flipped; this is not
    // a naturally induced kernel failure.
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
    )); // Full reader slots cannot disguise original Source failure as SnapshotBusy.
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
