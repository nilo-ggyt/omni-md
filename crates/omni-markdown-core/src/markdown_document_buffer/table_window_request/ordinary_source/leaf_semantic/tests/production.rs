//! Enter the original source owner through the public Leaf factory to verify publication,
//! continuation, and revocation, without directly constructing semantic jobs.

use super::*;

pub(super) fn finish_sources(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        // Return each original minimum quantum to the caller; production has no internal shortcut
        // that loops until EOF or Ready.
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(document, authority(document), 4096)?;
        assert_step(work);
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("公开 Leaf 来源请求未在有限累计预算内结束");
}

pub(super) fn leaf_sources(
    document: &MarkdownDocumentBuffer,
    requested: Range<usize>,
) -> (
    MarkdownViewportSourceRequest,
    VerifiedMarkdownViewportSources,
) {
    let mut request = document
        .viewport_leaf_source_request(requested, 57, document.selection())
        .unwrap();
    finish_sources(&mut request, document).unwrap();
    let sources = request
        .take_sources_for(document, authority(document))
        .unwrap()
        .unwrap();
    (request, sources)
}

#[test]
fn production_far_heading_survives_more_than_256_paragraph_and_blank_line_windows() {
    use crate::markdown_document_buffer::MarkdownHeadingAnchorStep as HeadingStep;
    use std::io::{Read, Write};

    const OPENING: &str = "[first](#deep-title) [duplicate](#deep-title-2) [top](#)\n\n";
    const PARAGRAPH: &str =
        "original bounded source paragraph keeps unchanged bytes for heading navigation.\n\n";
    const ATX: &str = "# Deep *Title*\n\n";
    const COLLISION: &str = "# Deep Title-1\n\n";
    const SETEXT: &str = "Deep **Title**\n===\n\n";
    const PARAGRAPHS: usize = 512;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("远端标题原来源续批.md");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(OPENING.as_bytes()).unwrap();
    for _ in 0..PARAGRAPHS {
        file.write_all(PARAGRAPH.as_bytes()).unwrap();
    }
    let first_heading = OPENING.len() + PARAGRAPH.len() * PARAGRAPHS;
    for heading in [ATX, COLLISION, SETEXT] {
        file.write_all(heading.as_bytes()).unwrap();
    }
    for _ in 0..PARAGRAPHS {
        file.write_all(PARAGRAPH.as_bytes()).unwrap();
    }
    file.sync_all().unwrap();
    drop(file); // Write fixture segments and close before production open, without a whole-document expected String.

    let mut doc = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let second_heading = first_heading + ATX.len() + COLLISION.len();
    doc.set_selection(MarkdownTextSelection::caret(first_heading))
        .unwrap();
    let mut request = doc
        .viewport_leaf_source_request(0..1, 57, doc.selection())
        .unwrap();
    super::inline::finish_inline(&mut request, &doc).unwrap();
    // Keep the first immutable public result across all continuations; reclaiming historical seeds
    // must not rewrite an existing reader.
    let first = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let mut previous = None;
    for (round, (fragment, expected_heading)) in [
        ("deep-title", first_heading),
        ("deep-title-2", second_heading),
    ]
    .into_iter()
    .enumerate()
    {
        let mut heading = doc.heading_anchor_request(fragment).unwrap();
        let target = (0..1_000_000)
            .find_map(|_| {
                let step = heading.advance(&doc, 4096).unwrap();
                let work = match &step {
                    HeadingStep::Progress { work }
                    | HeadingStep::Found { work, .. }
                    | HeadingStep::NotFound { work } => work,
                };
                // Original per-step limits still bound source/copy/storage work; finding an anchor
                // does not prove that the destination body has completed layout.
                assert!(work.source_bytes <= 4096 && work.copied_bytes <= 4096);
                assert!(work.storage_bytes <= 4096 && work.parser_input_bytes <= 16 * 1024);
                match step {
                    HeadingStep::Found { target, .. } => Some(target),
                    HeadingStep::Progress { .. } => None,
                    HeadingStep::NotFound { .. } => panic!("真实远端标题必须存在"),
                }
            })
            .expect("有限合成源上的真实标题查询必须结束");
        assert_eq!(target.position_for(&doc).unwrap(), expected_heading);
        if round != 0 {
            // Match link navigation: return to the first link on the original request, change full
            // selection, and reproject. Structural/semantic totals remain owned by that same
            // request; a second distant jump receives no fresh cumulative allowance.
            request
                .reposition(&doc, previous.as_ref().unwrap(), 0..1, authority(&doc))
                .unwrap();
            drop(previous.take());
            super::inline::finish_inline(&mut request, &doc).unwrap();
            let opening = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            doc.set_selection(MarkdownTextSelection::caret(expected_heading))
                .unwrap();
            assert!(
                request
                    .reproject_selection(&doc, &opening, 0..1, authority(&doc))
                    .unwrap()
            );
            assert_eq!(
                opening.ordinary_semantics_for(&doc, authority(&doc)).err(),
                Some(MarkdownTableRequestError::PublicationSuperseded)
            ); // The new selection cannot reuse the just-retired opening-line publication.
            drop(opening);
            super::inline::finish_inline(&mut request, &doc).unwrap();
            previous = request.take_sources_for(&doc, authority(&doc)).unwrap();
        }
        for paragraph in 0..PARAGRAPHS {
            let start = OPENING.len() + paragraph * PARAGRAPH.len();
            let blank = start + PARAGRAPH.len() - 1;
            for (range, content) in [
                (start..blank - 1, true),
                (blank - 1..blank, false),
                (blank..blank + 1, false),
            ] {
                // Follow Native's complete body/end-LF/blank-LF windows, not a one-character
                // paragraph sample. Blank space keeps source facts only; later body windows still
                // require actual current-block semantics from the original Global.
                request
                    .continue_forward(
                        &doc,
                        previous.as_ref().unwrap_or(&first),
                        range,
                        authority(&doc),
                    )
                    .unwrap();
                drop(previous.take()); // Retain the first and actual current reader, not every historical publication.
                super::inline::finish_inline(&mut request, &doc).unwrap();
                let current = request
                    .take_sources_for(&doc, authority(&doc))
                    .unwrap()
                    .unwrap();
                if content {
                    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
                        .ordinary_semantics_for(&doc, authority(&doc))
                        .unwrap()
                    else {
                        panic!("第{round}次导航第{paragraph}段不能因历史seed或重扫耗尽而缺失");
                    };
                    assert_eq!(
                        leaf.query_for(&doc, authority(&doc), start)
                            .unwrap()
                            .unwrap()
                            .styles,
                        MarkdownPresentationStyles::default()
                    );
                }
                previous = Some(current);
            }
        }
        if round != 0 {
            // Traverse both earlier ATX headings during the second navigation; the anchor coordinate
            // cannot bypass actual source continuation.
            let mut start = first_heading;
            for heading in [ATX, COLLISION] {
                let end = start + heading.len();
                for range in [start..end - 2, end - 2..end - 1, end - 1..end] {
                    request
                        .continue_forward(&doc, previous.as_ref().unwrap(), range, authority(&doc))
                        .unwrap();
                    drop(previous.take());
                    super::inline::finish_inline(&mut request, &doc).unwrap();
                    previous = request.take_sources_for(&doc, authority(&doc)).unwrap();
                }
                start = end;
            }
        }
        let heading_len = if round == 0 { ATX.len() } else { SETEXT.len() };
        request
            .continue_forward(
                &doc,
                previous.as_ref().unwrap(),
                expected_heading..expected_heading + heading_len - 1,
                authority(&doc),
            )
            .unwrap();
        drop(previous.take());
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let current = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("标题查询成功后，原正文仍须完成远端ATX与Setext的完整语义");
        };
        let title = expected_heading + if round == 0 { 8 } else { 7 };
        let styles = leaf
            .query_for(&doc, authority(&doc), title)
            .unwrap()
            .unwrap()
            .styles;
        assert!(styles.contains(MarkdownPresentationStyles::HEADING_1));
        assert!(styles.contains(if round == 0 {
            MarkdownPresentationStyles::EMPHASIS
        } else {
            MarkdownPresentationStyles::STRONG
        })); // Existing coordinates alone are insufficient; preserve the actual finite heading's inline styles.
        eprintln!(
            "far_heading_three_window_round={round} work={:?}",
            request.total_work()
        );
        previous = Some(current);
    }
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    // Restore the first result's full selection to read it. Cancelling the parent cannot invalidate
    // an independently transferred immutable reader.
    doc.set_selection(MarkdownTextSelection::caret(first_heading))
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(first_leaf) =
        first.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("原第一份不可变发布仍须有完整来源");
    };
    assert!(
        first_leaf
            .query_for(&doc, authority(&doc), 0)
            .unwrap()
            .is_some()
    );
    doc.set_selection(MarkdownTextSelection::caret(second_heading))
        .unwrap();
    assert_eq!(doc.generation(), 0);
    assert!(!doc.is_dirty() && !doc.can_undo() && !doc.can_redo());
    assert!(doc.original_file_identity_is_unchanged());
    // Independently compare disk segments without loading the complete expected body into String/Vec.
    let mut file = std::fs::File::open(&path).unwrap();
    let mut bytes = [0; 128];
    for segment in std::iter::once(OPENING)
        .chain(std::iter::repeat_n(PARAGRAPH, PARAGRAPHS))
        .chain([ATX, COLLISION, SETEXT])
        .chain(std::iter::repeat_n(PARAGRAPH, PARAGRAPHS))
    {
        file.read_exact(&mut bytes[..segment.len()]).unwrap();
        assert_eq!(&bytes[..segment.len()], segment.as_bytes());
    }
    assert_eq!(file.read(&mut bytes[..1]).unwrap(), 0);
    drop((file, request, first, previous, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
    assert_eq!(backing.first_errno(), None);
}

#[test]
fn selection_reprojection_reuses_true_prefix_and_stops_at_later_inline() {
    // Prepare the actual distant 83K window, change directed selection, then return to the beginning.
    // Later ** remains in the same closed seed but beyond the first Plain proof; reprojection
    // cannot enlarge proven_end into a whole-paragraph proof.
    let mut source = "中文abc😀".repeat(12_000);
    let suffix_start = source.encode_utf16().count();
    source.push_str(" **真实粗体**");
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let original_selection = MarkdownTextSelection {
        anchor_utf16: 83_055,
        head_utf16: 83_050,
    };
    doc.set_selection(original_selection).unwrap();
    let (mut request, old) = leaf_sources(&doc, 83_048..83_055);
    let MarkdownOrdinarySemanticState::RootLeafReady(old_leaf) =
        old.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("真实旧窗口必须已完成语义");
    };
    let new_selection = MarkdownTextSelection {
        anchor_utf16: 83_057,
        head_utf16: 83_069,
    };
    doc.set_selection(new_selection).unwrap();
    #[cfg(feature = "paged-line-compose")]
    assert!(
        request
            .selection_style_invariant(&doc, &old, authority(&doc))
            .unwrap()
            .is_some(),
        "原83K有限末端之前的Root Plain事实应能交给同源Composer核验"
    );
    assert!(
        request
            .reproject_selection(&doc, &old, 0..7, authority(&doc))
            .unwrap(),
        "真正完成来源坐标和发布的单物理行必须能沿原结构重投影"
    );
    // Even restoring old selection values cannot revive the retained old handle. Rejection must
    // reflect retired authority, not a temporary numeric mismatch; canonical text/history stay intact.
    doc.set_selection(original_selection).unwrap();
    assert!(matches!(
        old.ordinary_semantics_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    assert_eq!(
        old_leaf.query_for(&doc, authority(&doc), 83_050),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    doc.set_selection(new_selection).unwrap();
    drop(old);
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    // Complete every small window through 83K under the original 4096 quanta/cumulative limits.
    // Actual styles establish usable prefix reuse, beyond retaining an object or returning true once.
    for start in (0..83_076).step_by(7) {
        let range = start..start + 7;
        if start != 0 {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            drop(previous);
            finish_sources(&mut request, &doc).unwrap();
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("新选区真实普通前缀必须完成，range={range:?}");
        };
        for index in range {
            assert_eq!(
                leaf.query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
        }
    }
    let suffix = suffix_start..doc.utf16_len();
    request
        .continue_forward(&doc, &previous, suffix.clone(), authority(&doc))
        .unwrap();
    drop(previous);
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("原证明末端之后的真实粗体仍须独立完成");
    };
    #[cfg(feature = "paged-line-compose")]
    assert!(
        request
            .selection_style_invariant(&doc, &current, authority(&doc))
            .unwrap()
            .is_none(),
        "未知后缀遇真实特殊语法后不能沿已释放的旧Plain证明扩大能力"
    );
    assert_oracle(leaf, &doc, &source, &suffix);
    assert!(
        leaf.query_for(&doc, authority(&doc), suffix_start + 3)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.selection(), new_selection);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((current, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn selection_reprojection_recomputes_real_markers_links_and_underlines() {
    let source = "中文 **粗体** <u>下划线😀</u> [链接](https://example.com/a?x=1&amp;y=2)\n\n尾\n";
    let (directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let requested = units(source, 0..source.find('\n').unwrap() + 1);
    let outside = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(outside).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, outside)
        .unwrap();
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let bold = units(
        source,
        source.find("粗体").unwrap()..source.find("粗体").unwrap(),
    )
    .start;
    let underline = units(
        source,
        source.find("下划线").unwrap()..source.find("下划线").unwrap(),
    )
    .start;
    let marker = units(
        source,
        source.find("**").unwrap()..source.find("**").unwrap(),
    )
    .start;
    let link = units(
        source,
        source.find("链接").unwrap()..source.find("链接").unwrap(),
    )
    .start;
    for selection in [
        MarkdownTextSelection {
            anchor_utf16: bold + 2,
            head_utf16: bold,
        },
        MarkdownTextSelection::caret(underline),
        outside,
    ] {
        doc.set_selection(selection).unwrap();
        assert!(
            request
                .reproject_selection(&doc, &previous, requested.clone(), authority(&doc))
                .unwrap()
        );
        drop(previous);
        super::inline::finish_inline(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("新选择必须通过原Parser重新决定实际marker显隐");
        };
        assert_oracle(leaf, &doc, source, &requested);
        assert_eq!(
            leaf.query_for(&doc, authority(&doc), marker)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            selection.head_utf16 != bold,
        );
        assert_eq!(
            leaf.link_at_for(&doc, authority(&doc), link)
                .unwrap()
                .unwrap()
                .destination
                .clone()
                .into_string(),
            "https://example.com/a?x=1&y=2"
        );
    }
    assert_eq!(doc.selection(), outside);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn root_code_selection_invariant_reprojects_real_leaf_and_retires_previous_publication() {
    // Cover explicit closure/EOF and CRLF/indented code. Literal '</u>' and '**' cannot alter the
    // external underline owner or create inline markers; the full parser remains the independent oracle.
    for source in [
        "<u>外部\r\n\r\n  ```text\r\nliteral alpha 中文😀 **标记** </u><u> omega\r\n  ```\r\n\r\n外部关闭</u>\r\n",
        "前方 **真实粗体**\n\n```text\nliteral alpha 中文😀 **标记** omega",
        "前方 [链接](../target)\n\n    literal alpha 中文😀 **标记** omega\n\n后继段落\n",
        "前方\n\n    literal alpha 中文😀 **标记** omega",
    ] {
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let second = units(
            source,
            source.find("omega").unwrap()..source.find("omega").unwrap(),
        )
        .start;
        let requested = first..first + "literal".len();
        let original = MarkdownTextSelection::caret(first + 1);
        let changed = MarkdownTextSelection {
            anchor_utf16: second + 3,
            head_utf16: second,
        };
        doc.set_selection(original).unwrap();
        let (mut request, previous) = leaf_sources(&doc, requested.clone());

        // Independently parse the short fixture under both selections, including other root blocks.
        // Production capability borrows the same real Leaf; its seven-character requested range
        // cannot authorize the whole store.
        let mut before = MarkdownViewportPresentation::default();
        before.set_active_selection(ordered_selection(original));
        before.rebuild(0, 0..doc.utf16_len(), source).unwrap();
        doc.set_selection(changed).unwrap();
        let mut after = MarkdownViewportPresentation::default();
        after.set_active_selection(ordered_selection(changed));
        after.rebuild(0, 0..doc.utf16_len(), source).unwrap();
        for index in 0..doc.utf16_len() {
            assert_eq!(
                before.query(index).unwrap().styles,
                after.query(index).unwrap().styles
            );
        }
        // Cancellation, foreign documents, and wrong contexts retain typed errors rather than
        // degrading to an absent capability.
        let current = authority(&doc);
        let mut cancelled = current;
        cancelled.cancelled = true;
        assert!(matches!(
            request.selection_style_invariant(&doc, &previous, cancelled),
            Err(MarkdownTableRequestError::Cancelled)
        ));
        let mut wrong = current;
        wrong.context_revision += 1;
        assert!(matches!(
            request.selection_style_invariant(&doc, &previous, wrong),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        ));
        let (_foreign_directory, foreign) = document(source);
        assert!(matches!(
            request.selection_style_invariant(&foreign, &previous, authority(&foreign)),
            Err(MarkdownTableRequestError::SourceSuperseded)
        ));
        assert!(
            request
                .selection_style_invariant(&doc, &previous, current)
                .unwrap()
                .is_some()
        );
        assert!(
            request
                .reproject_selection(&doc, &previous, requested.clone(), current)
                .unwrap()
        );

        // New selection retires old publication permanently; restoring numeric values checks ABA
        // rejection rather than reauthorizing an old Rc.
        doc.set_selection(original).unwrap();
        assert!(matches!(
            previous.ordinary_semantics_for(&doc, authority(&doc)),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        ));
        doc.set_selection(changed).unwrap();
        drop(previous);
        finish_sources(&mut request, &doc).unwrap();
        let current = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一个真实Code叶必须沿RestartLeaf重新签出有限语义");
        };
        assert_oracle(leaf, &doc, source, &requested);
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), changed);
        assert!(!doc.is_dirty() && !doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((current, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn root_code_selection_invariant_declines_real_marker_owner_edges_and_container_prefixes() {
    // Closing-line spaces are not marker bytes but share its physical visibility owner. Indented
    // code end may be a following root paragraph; conservatively exclude EOF/end boundaries instead
    // of granting authority from a bare close_reason.
    for source in [
        "前方\r\n\r\n  ```text\r\nliteral alpha omega\r\n  ```\r\n\r\n后继\r\n",
        "前方\n\n```text\nliteral alpha omega",
        "前方\n\n    literal alpha omega\n\n后继\n",
        "前方\n\n    literal alpha omega",
    ] {
        let (_directory, mut doc) = document(source);
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let original = MarkdownTextSelection::caret(first + 1);
        doc.set_selection(original).unwrap();
        let (request, previous) = leaf_sources(&doc, first..first + 7);
        let last_body_byte = if let Some(closing) = source.rfind("  ```") {
            closing
        } else if let Some(next) = source.find("\n\n后继") {
            next + 1
        } else {
            source.len()
        };
        let body_end = units(source, last_body_byte..last_body_byte).start;
        for changed in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(body_end),
            MarkdownTextSelection {
                anchor_utf16: first + 1,
                head_utf16: 0,
            },
        ] {
            doc.set_selection(changed).unwrap();
            assert!(
                request
                    .selection_style_invariant(&doc, &previous, authority(&doc))
                    .unwrap()
                    .is_none()
            );
            // A read-only decline does not retire publication; restoring its real selection still
            // permits its original finite styles.
            doc.set_selection(original).unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("原Code窗口应保持Ready");
            };
            assert_oracle(leaf, &doc, source, &(first..first + 7));
        }
    }
    for source in [
        "> ```\n> literal alpha\n> omega\n> ```\n",
        "- ```\n  literal alpha\n  omega\n  ```\n",
    ] {
        let (_directory, mut doc) = document(source);
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let second = units(
            source,
            source.find("omega").unwrap()..source.find("omega").unwrap(),
        )
        .start;
        doc.set_selection(MarkdownTextSelection::caret(first))
            .unwrap();
        let (request, previous) = leaf_sources(&doc, first..first + 7);
        doc.set_selection(MarkdownTextSelection::caret(second))
            .unwrap();
        // Container prefixes change visibility with actual endpoints. Matching code/Leaf/root
        // parameters cannot replace the real scope's per-line obligations.
        assert!(
            request
                .selection_style_invariant(&doc, &previous, authority(&doc))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn selection_reprojection_declines_missing_records_and_keeps_real_failure_authority() {
    let source = "首段 **粗体**\n\n后段 中文😀 abc 尾\n";
    let (_directory, mut doc) = document(source);
    let original = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(original).unwrap();
    let start = units(
        source,
        source.find("后段").unwrap()..source.find("后段").unwrap(),
    )
    .start;
    let (mut request, previous) = leaf_sources(&doc, start..start + 2);
    doc.set_selection(MarkdownTextSelection::caret(start))
        .unwrap();
    assert!(
        !request
            .reproject_selection(&doc, &previous, 0..1, authority(&doc))
            .unwrap()
    );
    // Missing eligibility neither retires the ticket nor consumes its frontier; restore selection
    // and verify both old publication and original forward continuation.
    doc.set_selection(original).unwrap();
    assert!(
        previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .is_ok()
    );
    request
        .continue_forward(&doc, &previous, start + 2..start + 4, authority(&doc))
        .unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(
        current
            .ordinary_semantics_for(&doc, authority(&doc))
            .is_ok()
    );
    drop((current, request));

    // Cancellation, generation changes, and foreign documents revoke authority even if the requested
    // target lacks records. Once failure ends the request, restored numeric parameters cannot revive it.
    for cause in [
        MarkdownTableRequestError::Cancelled,
        MarkdownTableRequestError::SourceSuperseded,
    ] {
        let (_directory, mut current_doc) = document(source);
        let backing = current_doc.original_backing_observer().unwrap();
        current_doc.set_selection(original).unwrap();
        let (mut request, previous) = leaf_sources(&current_doc, start..start + 2);
        let mut current_authority = authority(&current_doc);
        if cause == MarkdownTableRequestError::Cancelled {
            current_authority.cancelled = true;
        } else {
            current_doc
                .replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            current_authority = authority(&current_doc);
        }
        assert_eq!(
            request.reproject_selection(&current_doc, &previous, 0..1, current_authority),
            Err(cause)
        );
        assert_eq!(
            request.reproject_selection(&current_doc, &previous, 0..1, authority(&current_doc)),
            Err(cause)
        );
        drop((previous, request, current_doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
    let (mut request, previous) = leaf_sources(&doc, start..start + 2);
    let (_foreign_directory, foreign) = document(source);
    assert_eq!(
        request.reproject_selection(&foreign, &previous, 0..1, authority(&foreign)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn plain_selection_prefix_declines_markup_and_preserves_original_publication() {
    // Every negative fixture completes actual Global/Leaf/parser output, rather than an empty
    // fabricated directory. Restored selection must preserve public semantics and reprojection,
    // showing that capability absence did not silently retire publication.
    for source in [
        "普通 **粗体** 尾\n",
        "<u>下划线😀</u> 尾\n",
        "<div>真实HTML</div>\n",
        "## 标题😀\n",
        "> 引用普通正文\n",
        "| 列 |\n| --- |\n| 值 |\n",
    ] {
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let original = MarkdownTextSelection::caret(doc.utf16_len());
        doc.set_selection(original).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, original)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let changed = MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 0,
        };
        doc.set_selection(changed).unwrap();
        assert!(
            request
                .selection_style_invariant(&doc, &previous, authority(&doc))
                .unwrap()
                .is_none(),
            "有真实marker/HTML/容器/table的语义不能保普通字形，source={source:?}"
        );
        doc.set_selection(original).unwrap();
        let semantic = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap();
        if let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = semantic {
            for covered in leaf.covered_ranges_for(&doc, authority(&doc)).unwrap() {
                assert_oracle(leaf, &doc, source, covered);
            }
        }
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((previous, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn plain_selection_prefix_keeps_real_source_cancellation_and_context_errors() {
    let source = "中文 abc😀 真实普通前缀 尾";
    let (_directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let original = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(original).unwrap();
    let (mut request, previous) = leaf_sources(&doc, 0..2);
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 0,
    })
    .unwrap();
    let current = authority(&doc);
    let (_foreign_directory, foreign) = document(source);
    let mut cancelled = current;
    cancelled.cancelled = true;
    let mut wrong_context = current;
    wrong_context.context_revision += 1;
    for (document, authority, expected) in [
        (&doc, cancelled, MarkdownTableRequestError::Cancelled),
        (
            &doc,
            wrong_context,
            MarkdownTableRequestError::PublicationSuperseded,
        ),
        (
            &foreign,
            authority(&foreign),
            MarkdownTableRequestError::SourceSuperseded,
        ),
    ] {
        assert!(matches!(
            request.selection_style_invariant(document, &previous, authority),
            Err(cause) if cause == expected
        ));
    }
    // Read-only capability checks leave the request intact; removing bad arguments still permits
    // reprojection of the same published window under the actual new selection.
    assert!(
        request
            .selection_style_invariant(&doc, &previous, current)
            .unwrap()
            .is_some()
    );
    assert!(
        request
            .reproject_selection(&doc, &previous, 0..2, current)
            .unwrap()
    );
    doc.set_selection(original).unwrap();
    assert!(matches!(
        previous.ordinary_semantics_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    doc.set_selection(current.selection).unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("原请求在拒绝外来参数之后仍须真实完成新选择发布");
    };
    assert_oracle(leaf, &doc, source, &(0..2));
    doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(matches!(
        request.selection_style_invariant(&doc, &current, authority(&doc)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    drop((current, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_small_leaf_windows_keep_far_directed_selection_under_original_work_limit() {
    // Each plain Chinese/ASCII/non-BMP group is 13 bytes/7 UTF-16 units with no Markdown markers.
    // Keep the distant directed user selection while scrolling small windows; its two authenticated
    // endpoints must not require rereading per batch.
    let source = "中文abc😀".repeat(16_384);
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 83_055,
        head_utf16: 83_050,
    }; // Fixed seven-unit groups put endpoints at a's start and after the complete emoji, not inside its surrogate pair.
    doc.set_selection(selection).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(0..7, 57, selection)
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    // Publish every small window through the complete group beyond the distant selection; short
    // 13K/18K runs cannot establish 83K behavior. Keep original quanta/totals without widening the
    // viewport or eager parsing; actual complete style results, not private locator/call counts,
    // establish this regression's outcome.
    for block in 0..=selection.anchor_utf16 / 7 {
        let range = block * 7..block * 7 + 7;
        if block != 0 {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            drop(previous); // Return old Ready after joint admission, without an artificial historical publication chain.
            finish_sources(&mut request, &doc).unwrap_or_else(|cause| {
                panic!(
                    "真实小窗未完成：range={range:?} cause={cause:?} work={:?}",
                    request.total_work(),
                );
            });
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一真实普通段落的小窗必须完成样式，range={range:?}");
        };
        // Structure proves only absence of tables. Each small window needs its own clipped Gap,
        // not the whole paragraph or previous coverage; actual default styles still require the
        // separate original Leaf query below.
        let table = previous.table_sources_for(&doc, authority(&doc)).unwrap();
        assert!(table.windows_for(&doc).unwrap().next().is_none());
        let [
            crate::markdown_document_buffer::MarkdownTableViewportCoverage::Gap {
                utf16_range, ..
            },
        ] = table.coverage_for(&doc).unwrap()
        else {
            panic!("普通段落小窗必须保留自己的真实无表分区");
        };
        assert_eq!(utf16_range, &range);
        for local in [0, 1, 2, 3, 4, 5] {
            let at = range.start + local;
            let query = leaf.query_for(&doc, authority(&doc), at).unwrap().unwrap();
            assert!(query.utf16_range.contains(&at));
            assert_eq!(query.styles, MarkdownPresentationStyles::default());
        } // Each fixed scalar needs real coverage; unknown pages cannot pass with empty styles or bare viewport bounds.
    }
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.selection(), selection);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_structure_and_gap_pruning_preserve_real_cell_semantics_after_reposition() {
    // The original Global yields a plain Gap, then an actual table. One-unit row-start requests must
    // retain full physical rows; returning from the tail Gap requires public reposition to restore
    // original source and cell semantics.
    let source =
        "前段甲😀乙\n\n| H |\n| --- |\n| **粗体** |\n| *斜体* |\n| `代码` |\n\n尾段甲😀乙xyz\n";
    let (directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let (mut request, mut previous) = leaf_sources(&doc, 0..1);
    let mut previous_start = 0;
    let header_byte = source.find("| H |").unwrap();
    let tail = source.find("尾段").unwrap();
    // None selects the actual plain tail; fixed table rows verify physical identity plus cell
    // strong/emphasis/code styles. Repeating the earlier bold row requires a real backward user
    // navigation, not false authorization from the forward cursor.
    for cell in [
        Some((
            "| **粗体** |\n",
            "粗体",
            1,
            MarkdownPresentationStyles::STRONG,
        )),
        Some((
            "| *斜体* |\n",
            "斜体",
            2,
            MarkdownPresentationStyles::EMPHASIS,
        )),
        Some((
            "| `代码` |\n",
            "代码",
            3,
            MarkdownPresentationStyles::INLINE_CODE,
        )),
        None,
        Some((
            "| **粗体** |\n",
            "粗体",
            1,
            MarkdownPresentationStyles::STRONG,
        )),
    ] {
        let byte_start = cell.map_or(tail, |(line, _, _, _)| source.find(line).unwrap());
        let start = units(source, byte_start..byte_start).start;
        let range = start..start + 1;
        if start < previous_start {
            request
                .reposition(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
        } else {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
        }
        drop(previous); // Return admitted old publication while retaining the unique request and cumulative allowance.
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(
            previous.requested_for(&doc, authority(&doc)).unwrap(),
            &range
        );
        let Some((line, word, ordinal, expected)) = cell else {
            // Leaving the table must actually prune its structure directory, even through an
            // otherwise empty phase-selection path.
            assert!(
                previous
                    .table_row_at_for(&doc, authority(&doc), start)
                    .unwrap()
                    .is_none()
            );
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("表后的真实普通段落必须有当前覆盖");
            };
            assert_eq!(
                leaf.query_for(&doc, authority(&doc), start)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
            previous_start = start;
            continue;
        };
        let row = previous
            .table_row_at_for(&doc, authority(&doc), start)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.coordinate_for(&doc, authority(&doc)).unwrap(),
            (header_byte, ordinal, 1)
        );
        assert_eq!(
            row.physical_utf16_range_for(&doc, authority(&doc)).unwrap(),
            units(source, byte_start..byte_start + line.len())
        );
        request
            .prepare_cell_inline(&doc, row, authority(&doc), 0)
            .unwrap();
        // Complete the cell through actual scope/raw source/parser work; structural row Ready is
        // not proof of character styles.
        let mut complete = false;
        for _ in 0..source.len() * 100 {
            let step = request
                .advance_cell_inline(&doc, authority(&doc), 4096)
                .unwrap();
            let (MarkdownTableCellInlineStep::Progress { work }
            | MarkdownTableCellInlineStep::Ready { work }) = step;
            assert!(work.source_bytes <= 4096 && work.structural_transitions <= 4096);
            if matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                complete = true;
                break;
            }
        }
        assert!(complete, "有限真实格必须完成全部内联语义");
        let inline = request
            .take_cell_inline_ready_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let word_byte = byte_start + line.find(word).unwrap();
        let at = units(source, word_byte..word_byte).start;
        let query = inline
            .query_for(&doc, authority(&doc), at)
            .unwrap()
            .unwrap();
        assert!(query.utf16_range.contains(&at) && query.styles.contains(expected));
        previous_start = start;
    }
    // Cancel a continuation from the restored nonempty table page: old publication stays readable,
    // while the ended task produces no new source result.
    request
        .continue_forward(
            &doc,
            &previous,
            units(source, tail..tail + "尾".len()),
            authority(&doc),
        )
        .unwrap();
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(
        previous
            .table_row_at_for(&doc, authority(&doc), previous_start)
            .unwrap()
            .is_some()
    );
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    // Actual edit/undo restores bytes in a new generation; neither the cancelled request nor old
    // table publication can authorize new source facts.
    doc.replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(doc.generation() > 0 && doc.can_redo() && !doc.is_dirty());
    assert_eq!(
        previous
            .table_row_at_for(&doc, authority(&doc), previous_start)
            .err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    // Check real mapping facts after dropping all original requests/publications/document, without
    // substituting private phase or handle counts for release.
    drop((previous, request, doc));
    assert!(backing.unmap_succeeded() && !backing.failed());
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_leaf_windows_keep_scalar_floor_and_ceil_across_pieces_and_reposition() {
    // Each 10-byte/four-unit group contains Chinese, a complete emoji, and Chinese. Windows end
    // inside a surrogate pair; coverage must clip to requested rather than treating the previous
    // ceil's already-read second surrogate as a new floor.
    let source = "甲😀乙".repeat(900);
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 3_599,
        head_utf16: 1,
    };
    doc.set_selection(selection).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(0..2, 57, selection)
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let mut previous_start = 0;
    // Groups 409/410 and 819/820 cross original 4-KiB read boundaries. Fixed literal lengths define
    // expected endpoints without inspecting private locator/candidate piece directories.
    for block in [0, 409, 410, 819, 820, 899] {
        let start = block * 4;
        for range in [
            start..start + 2,
            start + 1..start + 2,
            start + 1..start + 3,
            start + 2..start + 4,
        ] {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            finish_sources(&mut request, &doc).unwrap_or_else(|cause| {
                panic!(
                    "真实UTF16窗口未完成：range={range:?} cause={cause:?} work={:?}",
                    request.total_work()
                );
            });
            let current = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = current
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("固定普通正文必须有真实语义，range={range:?}");
            };
            for index in range.clone() {
                let query = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap();
                assert!(
                    range.start <= query.utf16_range.start && query.utf16_range.end <= range.end
                );
                assert!(query.utf16_range.contains(&index));
                assert_eq!(query.styles, MarkdownPresentationStyles::default());
            }
            // A retained old Ready must remain readable under its own coverage after new output;
            // owner reuse cannot rewrite results or retain a strong publication chain.
            let MarkdownOrdinarySemanticState::RootLeafReady(old) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("上一普通窗口必须仍可读");
            };
            assert_eq!(
                old.query_for(&doc, authority(&doc), previous_start)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
            previous = current;
            previous_start = range.start;
        }
    }
    // Actual backward navigation uses original reposition, not a reversed forward cursor; retain
    // the same complete directed selection.
    request
        .reposition(&doc, &previous, 1..3, authority(&doc))
        .unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = previous
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("逆向恢复必须重新取得真实emoji覆盖");
    };
    for index in 1..3 {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .utf16_range,
            1..3
        );
    }
    request
        .continue_forward(&doc, &previous, 1_636..1_640, authority(&doc))
        .unwrap();
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(ready.query_for(&doc, authority(&doc), 1).unwrap().is_some());
    assert_eq!(doc.selection(), selection);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn production_literal_html_and_metadata_keep_original_values_and_exact_underline_allowlist() {
    for source in [
        "<div>\n**字面粗体** `字面代码` [字面链接](https://example.com) <a href=\"https://example.com\">网页字面</a>\n<u>实际下划线😀</u> <U>大写字面</U> <u class=x>属性字面</u>\n</div>\n",
        "  <div>\n**字面粗体** `字面代码` [字面链接](https://example.com)\n<u>实际下划线😀</u>\n</div>\n",
        "<!-- **字面粗体** `字面代码` [字面链接](https://example.com) <u>实际下划线😀</u> -->\n",
        "<script>\n**字面粗体** `字面代码` [字面链接](https://example.com) <u>实际下划线😀</u>\n</script>\n",
        "> <div>\r\n> **字面粗体** `字面代码` [字面链接](https://example.com)\r\n> <u>实际下划线😀</u>\r\n> </div>\r\n",
        "- <div>\n  **字面粗体** `字面代码` [字面链接](https://example.com)\n  <u>实际下划线😀</u>\n  </div>\n",
        "> - <div>\n>   **字面粗体** `字面代码` [字面链接](https://example.com)\n>   <u>实际下划线😀</u>\n>   </div>\n",
        "---\ntitle: '**字面粗体** `字面代码` [字面链接](https://example.com)'\nvalue: '<u>实际下划线😀</u>'\n---\n",
        "---\r\ntitle: '**字面粗体** `字面代码` [字面链接](https://example.com)'\r\nvalue: '<u>实际下划线😀</u>'\r\n...  \r\n",
    ] {
        // The independent parser determines actual kind/end. A metadata closing EOL lies outside
        // that Leaf; consumed_end or a later prefix cannot become proven literal coverage merely
        // to make the whole viewport Ready.
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(event, Event::Start(Tag::HtmlBlock | Tag::MetadataBlock(_))).then_some(range)
        })
        .expect("夹具必须由原 Parser 真正识别为 HTML 或 metadata 叶");
        let requested = units(source, 0..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实字面叶须完成原字面与 allowlist 语义，source={source:?}");
            };
            assert_oracle(ready, &doc, source, &requested);

            // Stream raw values from the same public document. Ready styles cannot remove HTML,
            // YAML markers, Markdown spelling, or container prefixes, nor create false link targets.
            let mut actual = Vec::new();
            doc.write_utf16_range_to(requested.clone(), &mut actual)
                .unwrap();
            assert_eq!(actual, source.as_bytes()[..parsed.end]);
            for literal in ["字面粗体", "字面代码", "字面链接"] {
                let at = source.find(literal).unwrap();
                for index in units(source, at..at + literal.len()) {
                    let styles = ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles;
                    for unexpected in [
                        MarkdownPresentationStyles::STRONG,
                        MarkdownPresentationStyles::INLINE_CODE,
                        MarkdownPresentationStyles::LINK,
                    ] {
                        assert!(!styles.contains(unexpected));
                    }
                    assert!(
                        ready
                            .link_at_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            let underline = source.find("实际下划线😀").unwrap();
            for index in units(source, underline..underline + "实际下划线😀".len()) {
                assert!(
                    ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles
                        .contains(MarkdownPresentationStyles::UNDERLINE)
                );
            }
            // Nonexact tags never enter the allowlist; preserve their visible literal parser values.
            for literal in ["网页字面", "大写字面", "属性字面"] {
                if let Some(at) = source.find(literal) {
                    let index = units(source, at..at).start;
                    let styles = ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles;
                    assert!(!styles.contains(MarkdownPresentationStyles::UNDERLINE));
                    assert!(
                        ready
                            .link_at_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_giant_literal_html_keeps_real_underline_and_only_publishes_viewport() {
    // Only the test oracle owns the complete synthetic string. Production uses original mappings
    // and same-Global 4-KiB quanta; literal HTML permits no >2-MiB body copy to search for </u>.
    let opening = "<div><u>\n";
    let source = format!("{opening}{}\n</u></div>\n", "中😀".repeat(310_000));
    let (directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let byte_start = opening.len() + 170_000 * "中😀".len();
    let byte_end = byte_start + 19 * "中😀".len();
    let requested = units(&source, byte_start..byte_end);
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("巨型 HTML 中间窗口须等待真实 allowlist 配对后完成，不得直接签默认样式");
    };
    assert_oracle(ready, &doc, &source, &requested);
    for index in requested.clone() {
        assert!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::UNDERLINE)
        );
    }
    let mut actual = Vec::new();
    doc.write_utf16_range_to(requested, &mut actual).unwrap();
    assert_eq!(actual, source.as_bytes()[byte_start..byte_end]);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_scoped_leaf_prefixes_headings_and_code_match_original_parser() {
    for source in [
        "> ## 引用标题😀\n",
        "- ## 列表标题😀\n",
        "> - ## 引用列表标题😀\n",
        "- > ## 列表引用标题😀\n",
        "1. > ## 有序列表引用标题😀\n",
        "> -\t## Tab列表标题😀\n",
        "> ```text\r\n> 中文😀\r\n> 第二行\r\n> ```\r\n",
        "- ```text\n  中文😀\n  \t保留Tab\n  ```\n",
        ">     中文😀\n>     第二行\n",
        "> ````\n> 正文\n> ```",
        "> ```\n> 正文\n> ~~~",
        "> ```\n> 正文\n> ```不是closer",
        "> ```\n> 真实容器退出\n退出正文\n",
    ] {
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(event, Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_))).then_some(range)
        })
        .unwrap();
        // Verify actual first-line quote/item prefixes as well as body text after their markers.
        let requested = units(source, 0..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实容器 Leaf 必须完成，source={source:?}");
            };
            assert_oracle(ready, &doc, source, &requested);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_footnote_leaf_origins_keep_labels_literal_and_real_child_scope() {
    for source in [
        "[^123]: ## 中文标题😀\n",
        "> [^123]: ## 引用脚注标题😀\n",
        "[^外层]: > [^456]: ## 嵌套脚注标题😀\n",
        "[^123]: ```text\n    中文😀<u>代码字面</u>\n    ```\n",
        "> [^123]: ```text\r\n>     中文😀<u>代码字面</u>\r\n>     ```\r\n",
        "[^123]:\n        缩进代码😀\n",
        "[^123]: <div>\n    **字面粗体** <u>实际下划线😀</u>\n    </div>\n",
    ] {
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(
                event,
                Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::HtmlBlock)
            )
            .then_some(range)
        })
        .expect("独立原 Parser 必须确认真实脚注子叶");
        // An empty opening line is not part of the next code Leaf. Publish its label with the prefix
        // only when it shares the actual child's physical line.
        let physical = source[..parsed.start].rfind('\n').map_or(0, |at| at + 1);
        let requested = units(source, physical..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实脚注 child 必须保原 opening 和 Origin，source={source:?}")
            };
            assert_oracle(ready, &doc, source, &requested);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_giant_footnote_code_keeps_mapped_body_and_only_visible_semantics() {
    // The original producer/Origin proves the opening, four-column continuation, and fences. Only
    // the oracle holds the >2-MiB body; an offscreen first-line label permits no substitute root,
    // fresh quota, or whole-block buffer.
    let source = format!("[^123]: ```text\n    {}\n    ```\n", "中😀".repeat(310_000));
    let (directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let requested = 500_001..500_065;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("真实脚注代码必须沿原映射发布可见交集，不能复制整块或跳过其祖先");
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_root_eof_false_closers_remain_literal_without_losing_code_coverage() {
    for source in [
        "````\n正文\n```",
        "```\n正文\n~~~",
        "```\n正文\n```不是closer",
    ] {
        let (directory, doc) = document(source);
        let observer = doc.original_backing_observer().unwrap();
        let requested = units(source, first_leaf_range(source));
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("EOF 假围栏属于真实代码正文，不能因旧末行猜测拒绝整块");
        };
        assert_oracle(ready, &doc, source, &requested);
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop(sources);
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
fn production_scoped_giant_code_publishes_only_viewport_without_copying_canonical_body() {
    // Only the independent oracle holds this complete >2-MiB code fixture; production borrows
    // small frames from the original mapping.
    let source = format!("> ```text\n> {}\n> ```\n", "中😀".repeat(310_000));
    let (directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(0)).unwrap();
    let requested = 500_001..500_065;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("巨大容器代码仍须发布可见交集，不能整段复制或把合法块留作 Pending");
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_leaf_factory_uses_original_publication_and_matches_complete_projection() {
    for source in [
        "前言\n\n```rust\n中文😀<u>代码</u>\n```\n\n结尾\n",
        "前言\r\n\r\n  ~~~~text\r\n中文😀\r\n  ~~~~~ \r\n\r\n结尾\r\n",
        "前言\n\n    中文😀\n    第二行\n\n结尾\n",
        "前言\n\n  ***  \n\n结尾\n",
        "前言\n\n```text\n真实EOF中文😀",
        "<u>远处开标记\n\n```text\n中文😀\n```\n\n远处闭标记</u>\n",
    ] {
        let requested = units(source, first_leaf_range(source));
        for selection in [
            MarkdownTextSelection::caret(requested.start),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (_directory, mut doc) = document(source);
            doc.set_selection(selection).unwrap();
            let (request, mut sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("公开工厂的真实 Root 代码块或分隔线必须产生有限语义");
            };
            assert_oracle(ready, &doc, source, &requested);
            assert!(
                sources
                    .records
                    .iter()
                    .any(|record| Rc::ptr_eq(record, &ready.coverage.get(0).unwrap().record))
            );
            assert!(request.semantic.is_none());
            assert!(request.records.is_empty());
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);

            // End the original SourcePublication after extracting Ready. Isolated semantics cannot
            // reacquire authority merely from the same generation number.
            let state = std::mem::replace(
                &mut sources.semantic,
                MarkdownOrdinarySemanticState::SourcesOnly,
            );
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = state else {
                unreachable!();
            };
            drop(sources);
            assert_eq!(
                ready.query_for(&doc, authority(&doc), requested.start),
                Err(MarkdownTableRequestError::PublicationSuperseded)
            );
        }
    }
}

#[test]
/// Actual closing-EOL control tickets do not extend code semantics; an EOL-only request still uses
/// the public factory's same original root source.
fn production_closing_fence_separator_keeps_its_own_publication_and_original_plain_style() {
    for ending in ["\n", "\r\n"] {
        let source =
            format!("前言😀{ending}{ending}```text{ending}代码😀{ending}```  {ending}后文{ending}");
        let end = source.rfind("```  ").unwrap() + 5;
        let separator = units(&source, end..end + ending.len());
        for requested in [0..source.encode_utf16().count(), separator.clone()] {
            let (_directory, mut doc) = document(&source);
            let (request, sources) = leaf_sources(&doc, requested);
            assert_eq!(
                sources
                    .root_fence_separator_for(&doc, authority(&doc), &separator)
                    .unwrap(),
                Some(separator.clone())
            );
            let mut original = MarkdownViewportPresentation::default();
            original
                .rebuild(doc.generation(), 0..doc.utf16_len(), &source)
                .unwrap();
            for index in separator.clone() {
                // Outside the closing scanner the full parser gives plain style; do not inherit
                // the final code/hidden run's font.
                assert_eq!(
                    original.query(index).unwrap().styles,
                    MarkdownPresentationStyles::default()
                );
                if let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &sources.semantic {
                    assert!(
                        leaf.query_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            for other in [
                separator.start - 1..separator.end,
                separator.end..separator.end + 1,
                separator.end..separator.end,
            ] {
                assert!(
                    sources
                        .root_fence_separator_for(&doc, authority(&doc), &other)
                        .unwrap()
                        .is_none()
                );
            }
            if ending.len() == 2 {
                for half in [
                    separator.start..separator.start + 1,
                    separator.start + 1..separator.end,
                ] {
                    assert!(
                        sources
                            .root_fence_separator_for(&doc, authority(&doc), &half)
                            .unwrap()
                            .is_none()
                    );
                } // One CRLF grants no split control tickets, and the getter reads no body.
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
            doc.insert_utf16(0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            assert_eq!(
                sources.root_fence_separator_for(&doc, authority(&doc), &separator),
                Err(MarkdownTableRequestError::SourceSuperseded)
            ); // A new source generation cannot revive an old closing-EOL ticket with equal coordinates.
        }
    }

    let source = "> ```text\n> 代码\n> ```\n\n尾\n";
    let (_directory, doc) = document(source);
    let (_, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let end = source.rfind("```").unwrap() + 3;
    assert!(
        sources
            .root_fence_separator_for(&doc, authority(&doc), &units(source, end..end + 1))
            .unwrap()
            .is_none()
    ); // Container prefixes/newlines retain their original owner; a similar closer cannot authorize root control.
}

#[test]
/// Native's actual body/EOL/blank continuation order must not consume and lose the next fence or
/// heading fact.
fn production_native_control_windows_keep_the_original_next_leaf_across_blank_lines() {
    let source = "# Title\n\n```text\nlet value = 1;\n```\n\n## Next\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..7);
    let global = request.batch.source_probe_state().0;
    let mut ahead = None;
    for requested in [
        7..8,
        8..9,
        9..16,
        16..17,
        17..31,
        31..32,
        32..35,
        35..36,
        36..37,
        37..44,
        44..45,
    ] {
        request
            .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
            .unwrap();
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(request.batch.source_probe_state().0, global);
        if requested == (8..9) || requested == (36..37) {
            assert_eq!(previous.records.len(), 1);
            ahead = Some(Rc::downgrade(&previous.records[0]));
            assert!(
                previous
                    .ordinary_leaves_for(&doc, authority(&doc))
                    .unwrap()
                    .next()
                    .is_none()
            ); // Keep next-leaf metadata only for continuation; this blank viewport gains no getter/style authority for it.
            continue;
        }
        if requested == (9..16) || requested == (37..44) {
            let actual = previous.records.first().unwrap();
            assert!(ahead.take().unwrap().ptr_eq(&Rc::downgrade(actual)));
        } // Continuation borrows the same original-fact Rc, without reparsing/copying/admitting a replacement Leaf.
        if requested == (35..36) {
            assert_eq!(
                previous
                    .root_fence_separator_for(&doc, authority(&doc), &requested)
                    .unwrap(),
                Some(requested)
            );
            continue;
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            panic!("合法原 Native 范围 {requested:?} 未沿同一来源完成 RootLeaf 语义");
        };
        assert_oracle(leaf, &doc, source, &requested);
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// Preserve the following ordinary line's same semantic seed; coordinates alone would lose the
/// original inline source on the next viewport.
fn production_blank_control_preserves_the_next_ordinary_line_and_its_original_seed() {
    let source = "# Title\n\nplain words\n\n## Next\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..8);
    request
        .continue_forward(&doc, &previous, 8..9, authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_eq!(previous.records.len(), 1);
    let SourceRecord::Line(line) = previous.records[0].as_ref() else {
        panic!("该固定空行之后必须保原普通行，不是另一种叶块");
    };
    let seed = Rc::downgrade(line.semantic_block.as_ref().unwrap());
    assert!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_none()
    );
    request
        .continue_forward(&doc, &previous, 9..20, authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let line = current
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .next()
        .unwrap();
    assert!(seed.ptr_eq(&Rc::downgrade(line.semantic_block.as_ref().unwrap())));
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &current.semantic else {
        panic!("原普通行不能因上次窗口只有空白而失去语义 seed");
    };
    assert_oracle(leaf, &doc, source, &(9..20));
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
}

#[test]
/// When Native samples body/EOL/Setext-marker/EOL separately, the same terminal source must still
/// authorize the actual heading tail.
fn production_native_setext_control_windows_keep_the_original_terminal_owner_through_underline() {
    let source = "outside\n\nHeading alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\n---\n\nend\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..7);
    let global = request.batch.source_probe_state().0;
    let mut heading = None;
    for requested in [
        7..8,
        8..9,
        9..114,
        114..115,
        115..118,
        118..119,
        119..120,
        120..123,
        123..124,
    ] {
        request
            .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
            .unwrap();
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(request.batch.source_probe_state().0, global);
        if requested == (8..9) {
            heading = Some(Rc::downgrade(&previous.records[0]));
            continue;
        }
        if requested == (119..120) {
            continue; // Real blank lines have no heading style and cannot borrow later body coverage.
        }
        if requested.start >= 9 && requested.end <= 119 {
            assert!(
                heading
                    .as_ref()
                    .unwrap()
                    .ptr_eq(&Rc::downgrade(&previous.records[0]))
            );
        }
        if requested.start >= 115 && requested.end <= 119 {
            assert!(
                previous
                    .ordinary_lines_for(&doc, authority(&doc))
                    .unwrap()
                    .next()
                    .is_none()
            ); // The original Setext seed proves its underline; no invented body BlockOriginLine is needed.
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            panic!("原 Setext 分段范围 {requested:?} 丢失自己的 terminal owner");
        };
        assert_oracle(leaf, &doc, source, &requested);
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_leaf_giant_line_has_finite_runs_without_copy_or_parser_input() {
    let source = format!("前言\n\n```text\n{}\n```\n\n尾\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let requested = 50_000..50_017;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("巨型单行不能因为整个块超过窗口大小而失去有限代码语义");
    };
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    assert_eq!(ready.run_count(), 1);
    for index in requested {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            MarkdownPresentationStyles::FENCED_CODE
        );
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// The original root-code owner includes physical first-line whitespace; Composer cannot replace
/// visible indentation with a semantic hole.
fn production_indented_code_keeps_the_original_physical_prefix_in_semantic_coverage() {
    let source = "outside\n\n    <u>中文😀</u>\n\n";
    let (_directory, doc) = document(source);
    let (_request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let leaf = sources
        .records
        .iter()
        .find_map(|record| match record.as_ref() {
            SourceRecord::Leaf(leaf) if leaf.kind() == LeafBlockKind::IndentedCode => Some(leaf),
            _ => None,
        })
        .expect("真实Global必须识别缩进代码块");
    assert_eq!(leaf.physical_start(), 9);
    assert_eq!(leaf.source_range().start, 13);
    assert_eq!(leaf.block.scope_depth(), 0);
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("普通Root和缩进代码应完成真实有限语义");
    };
    for position in 9..13 {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), position)
                .unwrap()
                .expect("Composer实际请求的原四格缩进不能成为未授权空洞")
                .styles,
            MarkdownPresentationStyles::default()
        );
    }
}

#[test]
/// Prefix-only windows still need the original root-code ticket; clipping/tabs cannot lose
/// characters, borrow a neighboring block, or override the actual plain-prefix style.
fn production_root_code_prefix_clipping_matches_the_original_parser_without_copying() {
    for (source, prefix) in [
        ("outside\n\n    <u>中文😀</u>\n\n", 9..13),
        ("outside\n\n  ```text\n<u>中文😀</u>\n  ```\n\n", 9..11),
        ("outside\r\n\r\n\t<u>中文😀</u>\r\n\r\n", 11..12),
    ] {
        let (_directory, doc) = document(source);
        let prefix = units(source, prefix);
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        for requested in [
            0..doc.utf16_len(),
            prefix.clone(),
            prefix.end - 1..prefix.end,
        ] {
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
                panic!("首行物理前缀必须沿原Root代码来源完成");
            };
            for position in prefix.start.max(requested.start)..prefix.end.min(requested.end) {
                let query = ready
                    .query_for(&doc, authority(&doc), position)
                    .unwrap()
                    .expect("视口内真实缩进字符没有语义覆盖");
                assert_eq!(query.styles, MarkdownPresentationStyles::default());
                assert_eq!(query.styles, baseline.query(position).unwrap().styles);
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
        }
    }
}

#[test]
fn production_underline_probe_restores_future_code_facts_for_the_same_forward_owner() {
    let source = "<u>外部开标签\r\n\r\n标题一😀\r\n===\r\n\r\n```text\r\n</u><u>代码😀\r\n```\r\n\r\n标题二\r\n---\r\n\r\n    </u><u>缩进代码\r\n\r\n标题三\r\n===\r\n\r\n外部真关闭</u>\r\n";
    let (_directory, doc) = document(source);
    let options =
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
    // The full parser is only this finite fixture's oracle. Original production Global must deliver
    // later ranges again rather than reusing cached lookahead facts as source output.
    let ranges = Parser::new_ext(source, options)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_)))
                .then(|| units(source, range))
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges.len(), 5);
    let (mut request, mut previous) = leaf_sources(&doc, ranges[0].clone());
    let global = request.batch.source_probe_state().0;
    for (index, requested) in ranges.iter().enumerate() {
        if index != 0 {
            // Resume the main source at its original checkpoint; code visited during lookahead
            // must neither disappear nor become an ordinary paragraph.
            request
                .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
                .unwrap();
            finish_sources(&mut request, &doc).unwrap();
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let ready = match &previous.semantic {
            MarkdownOrdinarySemanticState::RootLeafReady(ready) => ready,
            MarkdownOrdinarySemanticState::LeafPending(reason) => {
                panic!("视口{index}原Root来源仍未完成：{reason:?}")
            }
            _ => panic!("视口{index}没有原Root语义结果"),
        };
        assert_oracle(ready, &doc, source, requested);
        assert_eq!(request.batch.source_probe_state().0, global);
        assert!(
            !request
                .batch
                .source_global_mut()
                .unwrap()
                .semantic_boundary_retained()
        );
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
    }
}

#[test]
fn production_underline_probe_source_reclaim_failure_releases_the_saved_global_and_scratch() {
    let source = format!(
        "<u>原开标签\n\n标题😀\n===\n\n{}\n\n原真关闭</u>\n",
        "中".repeat(100_000)
    );
    let heading = source.find("标题").unwrap();
    let requested = units(&source, heading..heading + "标题😀\n===\n".len());
    let (_directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    while !request
        .batch
        .source_global_mut()
        .unwrap()
        .semantic_boundary_retained()
    {
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("远端close尚未证明，不能先发布语义");
        };
        assert_step(work);
    }
    // Establish the actual main restore point before failing a later page return; do not directly
    // manufacture a business error or present cancellation as I/O failure.
    let attempts = observer.reclaim_attempts();
    observer.fail_next_reclaim_for_test();
    let error = loop {
        match request.advance(&doc, authority(&doc), 4096) {
            Ok(MarkdownViewportSourceStep::Progress { work }) => assert_step(work),
            Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => {
                panic!("未完成真实回收不能取得Ready")
            }
            Err(error) => break error,
        }
    };
    assert_eq!(
        error,
        MarkdownTableRequestError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        )
    );
    assert!(observer.reclaim_attempts() > attempts);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert!(request.leaf_semantic.is_none() && request.semantic.is_none());
    assert!(request.batch.source_global_mut().is_err());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(error)
    );
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_cancel_during_underline_probe_releases_the_main_boundary_without_poisoning_source() {
    let source = format!(
        "<u>开标签\n\n标题\n===\n\n{}\n\n真关闭</u>\n",
        "中".repeat(100_000)
    );
    let heading = source.find("标题").unwrap();
    let requested = units(&source, heading..heading + "标题\n===\n".len());
    let (_directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    while !request
        .batch
        .source_global_mut()
        .unwrap()
        .semantic_boundary_retained()
    {
        request.advance(&doc, authority(&doc), 4096).unwrap();
    }
    // Cancellation owns the entire source operation: end both lookahead and main restore duties
    // while leaving the healthy document available to future requests.
    request.cancel();
    assert!(request.batch.source_global_mut().is_err());
    assert!(request.leaf_semantic.is_none() && request.semantic.is_none());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(observer.first_errno(), None);
    assert!(doc.original_backing_health().is_ok());
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_leaf_forward_charges_old_pages_and_runs_without_retaining_old_publication() {
    let (source, expected) = code_page_fixture(40);
    let (_directory, doc) = document(&source);
    let first_range = 0..expected[29].end;
    let (mut request, previous) = leaf_sources(&doc, first_range.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须有真实页和 run");
    };
    assert!(old.run_count() > PAGE_RUNS);
    let actual_metadata = previous.records.len() * record_metadata()
        + publication_metadata()
        + old.retained_metadata()
        + previous.blocks.len() * block_metadata()
        + previous.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
    assert_eq!(previous.retained_metadata, actual_metadata);
    let old_runs = old.run_count();
    let old_publication = Rc::downgrade(&previous.publication);
    let global = request.batch.source_probe_state().0;
    assert_ne!(
        global, 0,
        "必须实际保留原结构前沿，两个空槽不能证明同一 Global"
    );
    let next = expected[10].start..expected[39].end;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    assert_eq!(quota.remaining_runs(), 8192 - old_runs);
    assert!(
        quota.metadata_used()
            >= previous.retained_metadata
                + source_workspace_metadata()
                + publication_reservation_metadata()
    );
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    let next_sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &next_sources.semantic else {
        panic!("原 Global 续批必须完成同源代码交集");
    };
    super::partial::assert_partial_oracle(&source, &doc, ready, next.clone(), &expected[10..]);
    super::partial::assert_partial_oracle(&source, &doc, old, first_range, &expected[..30]);
    // Continuation shares immutable Leaf records only, without retaining a parent publication
    // chain or the old Ready pages.
    drop(previous);
    assert!(old_publication.upgrade().is_none());
    assert!(
        ready
            .query_for(&doc, authority(&doc), next.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_forward_quota_failure_preserves_old_ready_and_releases_new_owner() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须先取得真正 Ready");
    };
    request
        .continue_forward(
            &doc,
            &previous,
            requested.start + 1..requested.end,
            authority(&doc),
        )
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    request
        .batch
        .admit_source_runs(quota.remaining_runs())
        .unwrap();
    let expected = MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit);
    assert_eq!(finish_sources(&mut request, &doc), Err(expected));
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert!(request.ready.is_none());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(expected)
    );
    assert!(
        old.query_for(&doc, authority(&doc), requested.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_edit_undo_and_cancel_cannot_reauthorize_same_root_or_keep_document_alive() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, mut doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("原发布必须有真正 Ready");
    };
    let root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &std::sync::Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert_eq!(
        ready.query_for(&doc, authority(&doc), requested.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.leaf_semantic.is_none());
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "请求和外带 Ready 都不能强持 piece root/mmap"
    );

    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    request.advance(&doc, authority(&doc), 4096).unwrap();
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request.advance(&doc, cancelled, 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
}

#[test]
fn production_leaf_proves_direct_links_and_keeps_unsupported_scope_pending() {
    // Keep the original giant Pending fixture and independently verify its now-supported semantics
    // under outside/inside/reverse selections. Replacing the sample would avoid marker-visibility
    // behavior rather than establish the actual capability.
    let complex = format!(
        "只有**{}**，[未知链接](https://example.com)，没有已证代码或分隔线\n",
        "复杂行内正文".repeat(140)
    );
    let label = complex.find("未知链接").unwrap();
    let label = units(&complex, label..label + "未知链接".len());
    for selection in [
        MarkdownTextSelection::caret(complex.encode_utf16().count()),
        MarkdownTextSelection::caret(label.start + 1),
        MarkdownTextSelection {
            anchor_utf16: label.end,
            head_utf16: label.start,
        },
    ] {
        let (directory, mut doc) = document(&complex);
        let backing = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原长direct-link样本必须拥有真实语义，不能维持过期Pending");
        };
        assert_oracle(ready, &doc, &complex, &requested);
        assert_short_link_payload_work(&request, &doc, &complex, &requested, ready);
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            complex.as_bytes()
        );
        drop((request, sources, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }

    // A nonroot thematic break still lacks supported scope semantics; link support cannot give it
    // default styles.
    let source = "> ***\n\n尾\n";
    let (_directory, doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let requested = units(source, first_leaf_range(source));
    let (request, sources) = leaf_sources(&doc, requested);
    assert!(matches!(
        sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap(),
        MarkdownOrdinarySemanticState::LeafPending(MarkdownRootLeafPendingReason::NonRootScope)
    ));
    // Pending may retain same-source lookup state but grants no semantics or strong document/mmap
    // ownership. Check actual mapping release after document destruction rather than implementation
    // details of private slots or metadata totals.
    drop(doc);
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
    assert_eq!(backing.first_errno(), None);
    drop((request, sources));

    // SourcesOnly remains a source-fact factory; adding Leaf mode cannot silently upgrade its semantics.
    let source = "```\n正文\n```\n";
    let (_directory, doc) = document(source);
    let (_, sources) = source_request(&doc, 0..doc.utf16_len());
    assert!(matches!(
        sources.semantic,
        MarkdownOrdinarySemanticState::SourcesOnly
    ));
}

#[test]
fn production_joint_plain_blocks_keep_exact_ranges_and_original_leaf_identity() {
    let first = "中文普通段落😀。\r\n第二行仍是普通文本。\r\n";
    let setext = "第二个普通闭合块😀\n=====\n";
    let last = "末段中文与 emoji 😀。\n";
    let complex = format!(
        "**复杂正文仍未知{}** [未知链接](https://example.com)",
        "中".repeat(700)
    );
    let source =
        format!("# 标题\n\n{first}\r\n{setext}\n{complex}\n\n```text\n代码\n```\n\n{last}");
    let (directory, doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let requested = 0..doc.utf16_len();
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("同一真实来源必须发布普通块与 Leaf 的已证交集");
    };
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(doc.selection()));
    baseline
        .rebuild(doc.generation(), requested.clone(), &source)
        .unwrap();
    for text in [first, setext, complex.as_str(), last] {
        let start = source.find(text).unwrap();
        // The full parser supplies the actual closed-block end, including EOL/Setext markers.
        // Substring-search ends cannot stand in for paragraph coverage; the complex literal itself
        // intentionally excludes its final newline.
        let range = Parser::new_ext(
            &source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            (matches!(event, Event::Start(Tag::Paragraph | Tag::Heading { .. }))
                && range.start == start)
                .then(|| units(&source, range))
        })
        .unwrap();
        assert!(
            ready
                .covered_ranges_for(&doc, authority(&doc))
                .unwrap()
                .any(|actual| actual == &range)
        );
        for index in range {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles
            );
            assert!(
                ready
                    .block_kind_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .is_none(),
                "真正普通块不能冒充 LeafBlockKind"
            );
        }
    }
    assert_short_link_payload_work(&request, &doc, &source, &requested, ready);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((request, sources, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn production_joint_giant_plain_forward_uses_same_global_and_never_copies_paragraph() {
    let source = format!("{}\n", "中文😀".repeat(30_000));
    let (_directory, doc) = document(&source);
    let first = 50_000..50_019;
    let (mut request, previous) = leaf_sources(&doc, first.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("巨型单段落必须提供有限普通语义");
    };
    assert_oracle(old, &doc, &source, &first);
    let original_global = request.batch.source_probe_state().0;
    assert_ne!(original_global, 0);
    let next = 50_010..50_031;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, original_global);
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &current.semantic else {
        panic!("原 seed/Global 续批不能丢失巨型段落的有限交集");
    };
    assert_oracle(ready, &doc, &source, &next);
    assert_oracle(old, &doc, &source, &first);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_joint_active_plain_cancel_and_run_limit_release_real_scratch() {
    for cancel in [true, false] {
        let (_directory, doc) = document("中文闭合普通段落😀。\n");
        let mut request = doc
            .viewport_leaf_source_request(0..doc.utf16_len(), 57, doc.selection())
            .unwrap();
        // Wait until actual Plain scratch owns the original underline before cancelling or
        // exhausting the joint inventory.
        for _ in 0..10_000 {
            if request.semantic.is_some() {
                break;
            }
            request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(
                request.ready.is_none(),
                "普通块未证明时不能提前发布默认样式"
            );
        }
        assert!(request.semantic.is_some());
        let expected = if cancel {
            request.cancel();
            MarkdownTableRequestError::Cancelled
        } else {
            let quota = request.batch.source_semantic_quota().unwrap();
            quota.admit_retained(quota.remaining_runs(), 0).unwrap();
            MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
        };
        assert_eq!(finish_sources(&mut request, &doc), Err(expected));
        assert!(request.leaf_semantic.is_none());
        assert!(request.records.is_empty());
        assert!(request.blocks.is_empty());
        assert!(request.ready.is_none());
        assert_eq!(request.advance(&doc, authority(&doc), 4096), Err(expected));
    }
}

#[test]
/// Short cold endpoints can fit one original quantum; a surrogate-interior viewport end still
/// rounds up through the actual scalar boundary.
fn joint_bounds_coalesce_short_endpoints_inside_original_quantum() {
    let source = "中文😀普通段落。\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 9,
    })
    .unwrap();
    let mut request = doc
        .viewport_leaf_source_request(2..3, 57, doc.selection())
        .unwrap();
    let MarkdownViewportSourceStep::Progress { work } =
        request.advance(&doc, authority(&doc), 4096).unwrap()
    else {
        panic!("坐标齐备不等于语义或来源已经发布");
    };
    assert_step(work);
    assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
    let job = request.leaf_semantic.as_ref().unwrap();
    assert!(!job.preparing());
    assert_eq!(job.position_index, 4);
    assert_eq!(job.byte_coverage, Some(6..10));
    assert_eq!(job.selection_bytes, Some(22..25));
    assert!(request.ready.is_none());
    assert_eq!(
        request.batch.source_probe_state().1,
        0,
        "原量子只完成四端点，不顺带推进Global"
    );
}

#[test]
/// A distant piece prefix may span quanta, preserving actual byte progress/Source errors without
/// enlarging read windows or budgets for cold display.
fn joint_bounds_long_prefix_remains_pending_and_reclaims_source_errors() {
    for fault in [false, true] {
        let source = format!("{}\n", "a".repeat(4000));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 3000,
            head_utf16: 3001,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(0..2, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("未完成的远端前缀不能发布来源");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // Bounds already read this page and retains unfinished endpoints. Returning a pin does
            // not evict cached pages; an armed flag alone cannot establish I/O failure.
            let observer = doc.original_backing_observer().unwrap();
            let attempts = observer.reclaim_attempts();
            let successes = observer.reclaim_successes();
            doc.fail_next_original_reclaim_for_test();
            assert_eq!(
                doc.reclaim_original_read_windows_for_test(),
                Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
            );
            assert!(observer.reclaim_attempts() > attempts);
            assert!(observer.reclaim_successes() > successes);
            assert_eq!(observer.first_errno(), Some(libc::EIO));
            // Actual reclaim failure between quanta revokes Source health first. The next entry
            // must retain SourceSuperseded and return unpublished resources.
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert_eq!(
                request.failed,
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            continue; // Actual read-window return failure stays primary; equal coordinates cannot retry or publish partial sources.
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds不取得后继结构或语义的发布权限");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
            assert!(work.structural_transitions > 0);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(0..2));
        assert_eq!(job.selection_bytes, Some(3000..3001));
        assert_eq!(request.batch.source_probe_state().1, 0);
    }
}

#[test]
/// Reuse actual local piece prefixes for viewport/distant-selection endpoints, reading less than
/// four independent prefix decodes without scanning the body between them.
fn joint_bounds_reuse_actual_piece_prefixes_with_far_selection_and_preserve_failure() {
    let source = "a".repeat(100_000);
    for fault in [false, true] {
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 31215,
            head_utf16: 31226,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(161..501, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("有限端点不能提前发布结构和语义");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // Both endpoints belong to the same cancellable request; authenticated piece scalars
            // cannot restore reading or publication after it ends.
            request.cancel();
            assert_eq!(request.failed, Some(MarkdownTableRequestError::Cancelled));
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096),
                Err(MarkdownTableRequestError::Cancelled)
            );
            continue;
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds只交付端点，不前移Global或发布Ready");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(161..501));
        assert_eq!(job.selection_bytes, Some(31215..31226));
        assert_eq!(request.batch.source_probe_state().1, 0);
        let mut independent_bytes = 0;
        for (target, ceil) in [(161, false), (501, true), (31215, false), (31226, true)] {
            let mut locator = PositionLocator::new(target, ceil);
            for _ in 0..100 {
                if locator.result().is_some() {
                    break;
                }
                independent_bytes += locator.advance(&doc, 4096).unwrap().source_bytes;
            }
            assert_eq!(locator.result(), Some(target));
        }
        assert!(request.total_work().source_bytes < independent_bytes);
        assert_eq!(request.total_work().source_bytes, 501 + (31226 - 7 * 4096));
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
    }
}
