//! Uses the complete original SCX oracle and real frozen-source suffix leases.
//! A synthetic ASCII source must not replace the preceding Unicode context.

use super::*;
use std::io::Write;

/// Advances only the original machine to settled Latin context, then requests
/// the same job's proof to its real source EOF.
fn begin(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
    position: usize,
    budget: MarkdownScriptBudget,
) {
    for _ in 0..32_768 {
        if job
            .machine
            .latin_suffix(position, job.source.utf16_len())
            .is_some()
        {
            job.prepare_ascii_suffix(
                authority,
                position,
                job.source.utf16_len(),
                job.source.utf16_len(),
            )
            .unwrap();
            return;
        }
        let step = job.step(authority, budget).unwrap();
        assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
        assert!(step.records <= budget.records && step.stack_ops <= budget.stack_ops);
        if step.status == MarkdownScriptStatus::Ready {
            return; // A finite counterexample may finish normally first; do not force it through the optimized path.
        }
    }
    panic!("有限前缀应在原Machine内取得真实Latn上下文");
}

#[test]
fn original_unicode_context_and_ascii_tail_match_complete_script_and_gcb_oracle() {
    // Mixed prefixes and disqualifying suffixes check both successful proof and
    // rejection. Both jobs retain one source identity and its original context.
    for text in [
        "中文😀! Xword (a)[b]{c} 9",
        "first\r\nsecond word\r\nXaaa!",
        "א)中😀! Xabc",
        "中 Xaaa\tbbb",
        "中 Xaaa\u{c}bbb",
        "中 Xaaa\u{7f}bbb",
        "中 Xaaaא",
        "中 Xaaa\u{2067}a\u{2069}",
        "中 Xaaaa\u{301}",
    ] {
        for budget in [
            ONE,
            MarkdownScriptBudget {
                bytes: 7,
                scalars: 3,
                records: 1,
                stack_ops: 1,
            },
        ] {
            let (_directory, document) = fixture(text);
            let source = MarkdownLayoutTextSource::capture(&document);
            let authority = source.authority();
            let position = text[..text.find('X').unwrap()].encode_utf16().count();
            let mut actual = MarkdownScriptJob::new(
                source.clone(),
                &authority,
                MarkdownScriptCancellation::new(),
            )
            .unwrap();
            let mut oracle = MarkdownScriptJob::new_streaming(
                source,
                &authority,
                MarkdownScriptCancellation::new(),
            )
            .unwrap();
            let receipt = actual.resource_observer();
            let oracle_receipt = oracle.resource_observer();
            begin(&mut actual, &authority, position, budget);
            finish(&mut actual, &authority, budget);
            finish(&mut oracle, &authority, budget);
            // Compare all closed runs and every UTF-16 unit, not just the Latin
            // candidate. A suffix proof cannot rewrite the preceding SCX result.
            assert_eq!(
                runs(&mut actual, &authority),
                runs(&mut oracle, &authority),
                "原完整SCX必须保留：{text:?}"
            );
            assert!(
                actual.machine.ready && actual.machine.complete_utf8(text.encode_utf16().count())
            );
            for offset in 0..=text.encode_utf16().count() {
                assert_eq!(
                    actual
                        .run_at(&authority, MarkdownScriptUnit::Utf16, offset)
                        .unwrap(),
                    oracle
                        .run_at(&authority, MarkdownScriptUnit::Utf16, offset)
                        .unwrap()
                );
            }
            // The original GCB scalar machine supplies actual cluster endpoints
            // for this finite fixture. Closed script runs must not cut a cluster.
            let mut gcb = crate::markdown_graphemes::MarkdownGraphemeScalarCursor::default();
            let mut boundaries = Vec::new();
            for (byte, scalar) in text.char_indices() {
                if gcb.push(scalar) {
                    boundaries.push(byte);
                }
            }
            boundaries.push(text.len());
            for run in runs(&mut actual, &authority) {
                let byte = document.byte_offset_for_utf16(run.end_utf16).unwrap();
                assert_eq!(byte, run.end_byte);
                assert!(boundaries.contains(&run.start_byte) && boundaries.contains(&run.end_byte));
            }
            drop(document);
            // Closing both real jobs after dropping the document must release
            // metadata and the shared frozen-source payload, not just tickets.
            actual.close().unwrap();
            oracle.close().unwrap();
            assert_released(&receipt);
            assert_released(&oracle_receipt);
        }
    }
}

#[test]
fn mixed_and_255_byte_crlf_prefix_keep_four_mebibyte_suffix_bounded_under_quota_one() {
    // Fixed-size writes construct the long real suffix without a full test-side
    // copy. Mixed and boundary-length CRLF prefixes retain original context.
    for prefix in [
        "one two 中文😀! ".to_owned(),
        format!("{}\r\nsecond word\r\n", "alpha ".repeat(40)),
    ] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(prefix.as_bytes()).unwrap();
        file.write_all(b"X").unwrap();
        file.write_all(&[b'a'; 4095]).unwrap();
        let block = [b'a'; 4096];
        for _ in 0..1023 {
            file.write_all(&block).unwrap();
        }
        file.flush().unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let observer = source.resource_observer();
        let position = prefix.encode_utf16().count();
        for budget in [
            ONE,
            MarkdownScriptBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                stack_ops: 256,
            },
        ] {
            let mut job = MarkdownScriptJob::new(
                source.clone(),
                &authority,
                MarkdownScriptCancellation::new(),
            )
            .unwrap();
            let receipt = job.resource_observer();
            begin(&mut job, &authority, position, budget);
            assert!(job.printable.is_some());
            // Proof work includes actual boundary-leaf reads and metadata. The
            // aggregate suffix may be skipped, but individual leases stay bounded.
            let work = finish(&mut job, &authority, budget);
            let profile = job.storage_profile();
            assert!(work.bytes < 8192 && profile.source_bytes_read < 16_384);
            assert!(profile.source_borrow_max_bytes <= 4096);
            assert_eq!(job.machine.byte, source.byte_len());
            assert!(job.machine.complete_utf8(source.utf16_len()));
            let final_run = match job
                .run_at(&authority, MarkdownScriptUnit::Utf16, position)
                .unwrap()
            {
                MarkdownScriptAnswer::Known(Some(run)) => run,
                _ => panic!("原Latn后缀必须真实完成"),
            };
            assert_eq!(final_run.script.iso15924_tag(), *b"Latn");
            assert_eq!(
                (final_run.end_byte, final_run.end_utf16),
                (source.byte_len(), source.utf16_len())
            );
            job.close().unwrap();
            // Job closure relinquishes its own metadata; source/document clones
            // intentionally remain live until both budget variants have run.
            let closed = receipt.snapshot();
            assert!(
                !closed.run_file_open
                    && !closed.stack_file_open
                    && closed.mapped_bytes == 0
                    && closed.buffer_bytes == 0
                    && !closed.cleanup_failed
            );
        }
        drop((source, document));
        assert!(observer.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn unsettled_bracket_context_does_not_gain_ascii_closure() {
    // A settled local prefix alone is insufficient: an actual unresolved opener
    // can still affect following script resolution and forbids Latin suffix proof.
    let text = "中(Xaaaa";
    let (_directory, document) = fixture(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let position = text[..text.find('X').unwrap()].encode_utf16().count();
    let mut job = MarkdownScriptJob::new(
        source.clone(),
        &authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    while job
        .machine
        .stable_open_prefix()
        .is_none_or(|run| run.end_utf16 <= position)
    {
        job.step(&authority, ONE).unwrap();
    }
    assert!(
        job.machine
            .latin_suffix(position, source.utf16_len())
            .is_none()
    );
    assert!(
        !job.prepare_ascii_suffix(&authority, position, source.utf16_len(), source.utf16_len())
            .unwrap()
    );
    assert!(job.printable.is_none()); // Printable continuation cannot erase the original live bracket stack.
    finish(&mut job, &authority, ONE);
    job.close().unwrap();
}

#[test]
fn pending_suffix_keeps_original_foreign_cancel_and_real_source_error_priority() {
    // Refuse a foreign capture, observe cancellation, and preserve the source's
    // EIO priority while a real suffix preparation is still Pending.
    for failure in [0, 1, 2] {
        let text = format!("中文😀! X{}", "a".repeat(16 * 1024));
        let (_directory, document) = fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let cancellation = MarkdownScriptCancellation::new();
        let mut job =
            MarkdownScriptJob::new(source.clone(), &authority, cancellation.clone()).unwrap();
        let receipt = job.resource_observer();
        let position = "中文😀! ".encode_utf16().count();
        begin(&mut job, &authority, position, ONE);
        assert!(job.printable.is_some());
        let foreign = MarkdownLayoutTextSource::capture(&document).authority();
        if failure == 2 {
            // The existing fault hook targets reclamation after a real source
            // visit; it does not represent a naturally occurring kernel failure.
            source.visit_byte_range(0..1, |_| Ok(())).unwrap();
            document.fail_next_original_reclaim_for_test();
            assert_eq!(
                document.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
        }
        if failure != 0 {
            cancellation.cancel();
        }
        let error = job
            .step(if failure == 1 { &authority } else { &foreign }, ONE)
            .unwrap_err();
        match (failure, error) {
            (0, MarkdownScriptError::SourceMismatch)
            | (1, MarkdownScriptError::Cancelled)
            | (
                2,
                MarkdownScriptError::Source(
                    MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO),
                ),
            ) => {}
            (_, error) => panic!("原Source首因和取消/外来权限顺序必须保留：{error:?}"),
        }
        job.close().unwrap();
        drop((source, document));
        assert_released(&receipt);
    }
}

#[test]
fn actual_frozen_suffix_partial_leaf_foreign_identity_and_surrogate_follow_each_quantum() {
    // Starting inside a mixed leaf requires actual source bytes. One-unit quotas
    // also charge directory work and cannot hide a whole boundary-leaf lease.
    let text = "中😀 prefix Xaaa";
    let (_directory, document) = fixture(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let mut cursor = source
        .begin_printable_ascii_suffix(4..source.utf16_len())
        .unwrap();
    let mut bytes = 0;
    let mut maximum = 0;
    loop {
        let step = source
            .advance_printable_ascii_suffix(&mut cursor, 1, 1, 1)
            .unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
        maximum = maximum.max(step.maximum_borrow);
        bytes += step.bytes;
        if let Some(answer) = step.answer {
            assert!(answer);
            break;
        }
    }
    assert!(bytes > 0 && maximum == 1); // The mixed leaf was read byte by byte, without an uncharged four-KiB boundary lease.
    let foreign = MarkdownLayoutTextSource::capture(&document);
    // Completion does not let another capture resume the cursor, even when its
    // document and bytes are unchanged.
    assert!(matches!(
        foreign.advance_printable_ascii_suffix(&mut cursor, 1, 1, 1),
        Err(MarkdownDocumentBufferError::RangeOutsideDocument)
    ));
    let mut split = source
        .begin_printable_ascii_suffix(2..source.utf16_len())
        .unwrap();
    // UTF-16 coordinate 2 cuts the original emoji's surrogate pair. A later
    // printable suffix cannot make that invalid start into a Unicode boundary.
    loop {
        match source.advance_printable_ascii_suffix(&mut split, 1, 1, 1) {
            Ok(step) => {
                assert!(step.answer.is_none());
                assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            }
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary) => break,
            _ => panic!("低代理项不能由后续ASCII证书变成有效起点"),
        }
    }
    for tail in [
        "\t", "\n", "\u{c}", "\u{7f}", "א", "\u{661}", "\u{2067}", "\u{301}",
    ] {
        // Controls, non-Latin scalars, bidi isolates, and combining marks are
        // genuine counterexamples, not a reason to truncate the requested range.
        let (_directory, mixed) = fixture(&format!("中 Xaaa{tail}"));
        let mixed = MarkdownLayoutTextSource::capture(&mixed);
        let mut cursor = mixed
            .begin_printable_ascii_suffix(2..mixed.utf16_len())
            .unwrap();
        loop {
            let step = mixed
                .advance_printable_ascii_suffix(&mut cursor, 1, 1, 1)
                .unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            if let Some(answer) = step.answer {
                assert!(!answer);
                break;
            }
        }
    }
}

/// Receives the current B range's actual endpoints from the same source. Only
/// the original machine advances to Latin context sufficient to begin proof.
fn begin_before_b(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
    position: usize,
    content_end: usize,
    paragraph_end: usize,
    budget: MarkdownScriptBudget,
) {
    for _ in 0..32_768 {
        if job.machine.latin_suffix(position, content_end).is_some() {
            assert!(
                job.prepare_ascii_suffix(authority, position, content_end, paragraph_end)
                    .unwrap()
            );
            return;
        }
        assert_eq!(
            job.step(authority, budget).unwrap().status,
            MarkdownScriptStatus::Pending
        );
    }
    panic!("有限首段必须在真实B之前取得原稳定Latn上下文");
}

#[test]
fn non_eof_ascii_proof_keeps_original_b_crlf_and_following_unicode_script_oracle() {
    // Prove only the content before a real paragraph separator. The following
    // B/CRLF and Unicode text still belong to the original machine's full oracle.
    for prefix in ["中😀 ", "🇨🇳👩\u{301}\u{200d}💻 क् "] {
        for separator in [
            "\r\n", "\n", "\r", "\u{85}", "\u{2029}", "\u{1c}", "\u{1d}", "\u{1e}", "\r\n\r\n",
        ] {
            let ascii = format!("X{}[()]", "a".repeat(64));
            let text = format!("{prefix}{ascii}{separator}中文🇺🇸👩\u{200d}💻क्Xend");
            let (_directory, document) = fixture(&text);
            let source = MarkdownLayoutTextSource::capture(&document);
            let authority = source.authority();
            let position = prefix.encode_utf16().count();
            let content_end = position + ascii.len();
            let paragraph_end = content_end + separator.chars().next().unwrap().len_utf16();
            for budget in [
                ONE,
                MarkdownScriptBudget {
                    bytes: 7,
                    scalars: 3,
                    records: 1,
                    stack_ops: 1,
                },
            ] {
                let mut actual = MarkdownScriptJob::new(
                    source.clone(),
                    &authority,
                    MarkdownScriptCancellation::new(),
                )
                .unwrap();
                let mut oracle = MarkdownScriptJob::new_streaming(
                    source.clone(),
                    &authority,
                    MarkdownScriptCancellation::new(),
                )
                .unwrap();
                begin_before_b(
                    &mut actual,
                    &authority,
                    position,
                    content_end,
                    paragraph_end,
                    budget,
                );
                let before_runs = actual.store.run_count();
                // Exhaust only the suffix preparation. Successful non-EOF proof
                // advances the cursor but cannot publish Ready or a closed run.
                while actual.printable.is_some() {
                    let step = actual.step(&authority, budget).unwrap();
                    assert!(
                        step.bytes <= budget.bytes
                            && step.scalars <= budget.scalars
                            && step.records <= budget.records
                            && step.stack_ops <= budget.stack_ops
                    );
                    assert_eq!(step.status, MarkdownScriptStatus::Pending);
                }
                assert_eq!(actual.store.run_count(), before_runs); // Proving content before B must not append an early closed run.
                assert_eq!(actual.machine.byte, prefix.len() + ascii.len());
                assert!(actual.machine.complete_utf8(content_end) && !actual.machine.ready);
                assert!(matches!(
                    actual
                        .run_at(&authority, MarkdownScriptUnit::Utf16, position)
                        .unwrap(),
                    MarkdownScriptAnswer::Pending
                ));
                finish(&mut actual, &authority, budget);
                // Now let the original machine consume the actual separator,
                // preserving CRLF grapheme continuity and subsequent script state.
                finish(&mut oracle, &authority, budget);
                assert_eq!(
                    runs(&mut actual, &authority),
                    runs(&mut oracle, &authority),
                    "原B/CRLF和后继上下文：{prefix:?}/{separator:?}"
                );
                assert!(actual.storage_profile().source_bytes_read < text.len() * 2);
                actual.close().unwrap();
                oracle.close().unwrap();
            }
        }
    }
}

#[test]
fn four_mebibyte_non_eof_paragraph_consumes_true_crlf_and_next_chinese_without_full_read() {
    // The real long paragraph ends before EOF. Fixed-size writes avoid making a
    // second complete body merely to test bounded suffix proof and B handoff.
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"X").unwrap();
    file.write_all(&[b'a'; 4095]).unwrap();
    for _ in 0..1023 {
        file.write_all(&[b'a'; 4096]).unwrap();
    }
    file.write_all("\r\n\r\n中文😀后继".as_bytes()).unwrap();
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let observer = source.resource_observer();
    let content_end = 4 * 1024 * 1024;
    for budget in [
        ONE,
        MarkdownScriptBudget {
            bytes: 4096,
            scalars: 256,
            records: 256,
            stack_ops: 256,
        },
    ] {
        let mut job = MarkdownScriptJob::new(
            source.clone(),
            &authority,
            MarkdownScriptCancellation::new(),
        )
        .unwrap();
        let resources = job.resource_observer();
        begin_before_b(
            &mut job,
            &authority,
            0,
            content_end,
            content_end + 1,
            budget,
        );
        finish(&mut job, &authority, budget);
        // The first actual CRLF closes the Latin run. The second belongs to the
        // original following paragraph, whose Hani text must still be itemized.
        let first = match job
            .run_at(&authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap()
        {
            MarkdownScriptAnswer::Known(Some(run)) => run,
            _ => panic!("真实CRLF后必须已封闭首Latn run"),
        };
        assert_eq!(
            (first.end_byte, first.end_utf16),
            (content_end + 2, content_end + 2)
        );
        assert_eq!(first.script.iso15924_tag(), *b"Latn");
        let following = match job
            .run_at(&authority, MarkdownScriptUnit::Utf16, content_end + 4)
            .unwrap()
        {
            MarkdownScriptAnswer::Known(Some(run)) => run,
            _ => panic!("连续CRLF后的真实中文run不能遗漏"),
        };
        assert_eq!(following.script.iso15924_tag(), *b"Hani");
        assert_eq!(following.start_utf16, content_end + 4);
        assert!(
            job.storage_profile().source_bytes_read < 8192
                && job.storage_profile().source_borrow_max_bytes <= 4096
        );
        // This bound concerns actual script-source reads and leases; mapping
        // observers separately check final ownership after all source clones drop.
        job.close().unwrap();
        let closed = resources.snapshot();
        assert!(
            !closed.run_file_open
                && !closed.stack_file_open
                && closed.mapped_bytes == 0
                && closed.buffer_bytes == 0
                && !closed.cleanup_failed
        );
    }
    drop((source, document));
    assert!(observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn actual_range_counterexample_still_validates_its_low_surrogate_end() {
    let (_directory, document) = fixture("中 X\t😀tail");
    let source = MarkdownLayoutTextSource::capture(&document);
    // TAB already disproves printable ASCII, but an end inside the emoji still
    // requires a boundary error instead of prematurely publishing false.
    let mut proof = source.begin_printable_ascii_suffix(2..5).unwrap();
    loop {
        match source.advance_printable_ascii_suffix(&mut proof, 1, 1, 1) {
            Ok(step) => {
                assert!(step.answer.is_none());
                assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            }
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary) => break,
            other => panic!("范围末端必须验证：{}", other.is_ok()),
        }
    }
}

#[test]
fn non_eof_pending_and_actual_b_handoff_preserve_foreign_cancel_eio_and_release() {
    // Cover both in-flight proof and its successful non-EOF handoff. Neither
    // phase can bypass original identity, cancellation, or source-health checks.
    for resumed in [false, true] {
        for failure in [0, 1, 2] {
            let prefix = "中😀 X";
            let text = format!("{prefix}{}\r\n中文后继", "a".repeat(4096));
            let (_directory, document) = fixture(&text);
            let mapping = document.original_backing_observer().unwrap();
            let source = MarkdownLayoutTextSource::capture(&document);
            let authority = source.authority();
            let cancellation = MarkdownScriptCancellation::new();
            let mut job =
                MarkdownScriptJob::new(source.clone(), &authority, cancellation.clone()).unwrap();
            let receipt = job.resource_observer();
            let position = "中😀 ".encode_utf16().count();
            let content_end = prefix.encode_utf16().count() + 4096;
            begin_before_b(
                &mut job,
                &authority,
                position,
                content_end,
                content_end + 1,
                ONE,
            );
            if resumed {
                // Consume only preparation: the actual separator is unread and
                // the Latin run must still be Pending under the public query.
                while job.printable.is_some() {
                    job.step(&authority, ONE).unwrap();
                }
                assert_eq!(job.machine.byte, prefix.len() + 4096);
                assert!(
                    !job.machine.ready
                        && matches!(
                            job.run_at(&authority, MarkdownScriptUnit::Utf16, position)
                                .unwrap(),
                            MarkdownScriptAnswer::Pending
                        )
                );
            }
            let foreign = MarkdownLayoutTextSource::capture(&document).authority();
            if failure == 2 {
                // Keep the existing reclaim injection explicit and require its
                // real error path before testing priority against other failures.
                source.visit_byte_range(0..1, |_| Ok(())).unwrap();
                document.fail_next_original_reclaim_for_test();
                assert_eq!(
                    document.reclaim_original_read_windows_for_test(),
                    Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                        libc::EIO
                    ))
                );
            }
            if failure != 0 {
                cancellation.cancel();
            }
            match (
                failure,
                job.step(if failure == 1 { &authority } else { &foreign }, ONE)
                    .unwrap_err(),
            ) {
                (0, MarkdownScriptError::SourceMismatch)
                | (1, MarkdownScriptError::Cancelled)
                | (
                    2,
                    MarkdownScriptError::Source(
                        MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO),
                    ),
                ) => {}
                (_, error) => panic!("原B交接仍保来源首错/取消/权限：{error:?}"),
            }
            job.close().unwrap();
            // Error priority is separate from cleanup. Drop the final source and
            // document owners, then inspect actual mapping release and first errno.
            drop((source, document));
            assert_released(&receipt);
            assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
            assert_eq!(mapping.first_errno(), (failure == 2).then_some(libc::EIO));
        }
    }
}
