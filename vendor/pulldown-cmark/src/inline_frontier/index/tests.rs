// Copyright 2017 Google Inc. All rights reserved.
// 原Parser事件与真实分段index的差分；MIT许可见LICENSE。

use super::*;
use crate::inline_frontier::{CodeSpanStep, InlineStepStatus};
use crate::{Event, Options, Parser};

fn frame(text: &str, at: usize, width: usize) -> SourceFrame<'_> {
    let end = (at + width).min(text.len());
    SourceFrame {
        source_revision: 11,
        byte_start: at,
        bytes: &text.as_bytes()[at..end],
        real_eof: end == text.len(),
    }
}

fn build(text: &str, width: usize) -> (CodeLastRunIndex, CodeIndexWork) {
    let mut builder =
        CodeLastRunIndexBuilder::new(11, text.len(), 0..text.len(), MAX_CODE_INDEX_METADATA_BYTES)
            .unwrap();
    assert!(builder.take_complete().unwrap().is_none());
    let mut total = CodeIndexWork::default();
    loop {
        let step = builder
            .advance(
                frame(text, builder.next_byte(), width),
                MIN_CODE_INDEX_STEP_WORK,
            )
            .unwrap();
        assert!(step.work.total() <= MIN_CODE_INDEX_STEP_WORK);
        assert!(step.work.actual_key_comparisons <= step.work.key_comparisons_bound);
        assert!(step.work.live_metadata_bytes_bound <= MAX_CODE_INDEX_METADATA_BYTES);
        total.lexical.byte_reads += step.work.lexical.byte_reads;
        total.key_comparisons_bound += step.work.key_comparisons_bound;
        total.actual_key_comparisons += step.work.actual_key_comparisons;
        total.live_metadata_bytes_bound = total
            .live_metadata_bytes_bound
            .max(step.work.live_metadata_bytes_bound);
        if step.status == CodeIndexStatus::Complete {
            break;
        }
        assert!(builder.take_complete().unwrap().is_none());
    }
    let index = builder.take_complete().unwrap().unwrap();
    assert!(matches!(
        builder.take_complete(),
        Err(CodeIndexError::Closed)
    ));
    (index, total)
}

fn runs(text: &str, width: usize) -> Vec<CodeDelimiterCandidate> {
    let mut scanner = InlineLexicalCursor::new(11, text.len(), 0..text.len()).unwrap();
    let mut runs = Vec::new();
    loop {
        let step = scanner
            .advance(frame(text, scanner.next_byte(), width), 4096)
            .unwrap();
        match step.status {
            InlineStepStatus::Candidate(InlineCandidate::CodeDelimiter(run)) => runs.push(run),
            InlineStepStatus::Complete => break,
            _ => {}
        }
    }
    runs
}

fn indexed_code(text: &str, width: usize) -> (Vec<Range<usize>>, usize, usize) {
    let (index, work) = build(text, width);
    assert!(work.lexical.byte_reads <= 2 * text.len());
    let mut ranges = Vec::new();
    let mut previous_commit = 0;
    let mut previous_search_end = 0;
    let mut searched_bytes = 0;
    let mut unmatched = 0;
    // 这个oracle driver仅用于无HTML/math/link前置消费的语法域；不是生产优先序替身。
    for opener in runs(text, width) {
        if opener.start < previous_commit {
            continue;
        }
        let lookup = index.lookup(opener, 4096).unwrap();
        assert!(lookup.work.actual_key_comparisons <= lookup.work.key_comparisons_bound);
        match lookup.decision {
            IndexedCodeDecision::Literal => {
                previous_commit = opener.end;
            }
            IndexedCodeDecision::Unmatched { replay_from } => {
                unmatched += 1;
                previous_commit = replay_from;
                assert_eq!(lookup.work.lexical.byte_reads, 0);
            }
            IndexedCodeDecision::Search(mut probe) => {
                assert!(probe.next_byte() >= previous_search_end);
                loop {
                    let step = probe
                        .advance(frame(text, probe.next_byte(), width), 4096)
                        .unwrap();
                    match step {
                        CodeSpanStep::Candidate { work, candidate } => {
                            searched_bytes += work.byte_reads;
                            previous_search_end = candidate.closing().end;
                            previous_commit = candidate.range().end;
                            ranges.push(candidate.range());
                            break;
                        }
                        CodeSpanStep::Unmatched { .. } => {
                            panic!("完整last index已证明存在closer，不得悄悄重放失败搜索")
                        }
                        CodeSpanStep::Progress { work } | CodeSpanStep::NeedInput { work } => {
                            searched_bytes += work.byte_reads
                        }
                    }
                }
            }
        }
    }
    assert!(searched_bytes <= 2 * text.len());
    (ranges, searched_bytes, unmatched)
}

#[test]
fn actual_parser_same_closer_escaped_openers_and_failed_searches_match() {
    let mut sources = vec![
        "x `foo``bar``".to_owned(),
        "x \\``foo`bar`".to_owned(),
        "x `a\\`b` \\`literal \\``two`".to_owned(),
        "x `` a ` b `` `tail`".to_owned(),
        "x 中文😀 `a\r\nb` &grave; &#96; `last`".to_owned(),
    ];
    let alphabet = b"ab `\\&;#x09";
    let mut random = 23u64;
    for _ in 0..512 {
        let mut text = String::from("x ");
        for _ in 0..127 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(char::from(alphabet[random as usize % alphabet.len()]));
        }
        sources.push(text);
    }
    for source in sources {
        let expected: Vec<_> = Parser::new(&source)
            .into_offset_iter()
            .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
            .collect();
        for width in [1, 2, 7, 31, 4096] {
            assert_eq!(
                indexed_code(&source, width).0,
                expected,
                "frame={width}, source={source:?}"
            );
        }
    }
}

#[test]
fn giant_distinct_lengths_have_finite_metadata_and_no_unmatched_lookahead() {
    assert_eq!(maximum_distinct_runs(32 * 1024 * 1024), 8191);
    assert!(maximum_distinct_runs(usize::MAX) < usize::MAX);
    let mut source = String::from("x ");
    for count in 1..=1450 {
        source.extend(std::iter::repeat('`').take(count));
        source.push('a');
    }
    assert!(source.len() > 1024 * 1024);
    let (actual, searched, unmatched) = indexed_code(&source, 997);
    assert!(actual.is_empty());
    assert_eq!(searched, 0);
    assert_eq!(unmatched, 1450);
    let (index, work) = build(&source, 997);
    assert_eq!(index.last.len(), 1450);
    assert!(work.live_metadata_bytes_bound < 160 * 1024);
    let expected: Vec<_> = Parser::new(&source)
        .into_offset_iter()
        .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn successful_searches_are_disjoint_and_priority_counterexamples_remain_candidates() {
    let source = format!("x `{}尾😀` ``a ` b`` `unmatched", "a".repeat(1_048_577));
    let (actual, reads, unmatched) = indexed_code(&source, 1021);
    assert!(reads <= 2 * source.len());
    assert_eq!(unmatched, 1);
    let expected: Vec<_> = Parser::new(&source)
        .into_offset_iter()
        .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
        .collect();
    assert_eq!(actual, expected);
    for source in ["x <a href=\"`\">`", "x $`a`$", "x [label](<`a`>)"] {
        let (index, _) = build(source, 1);
        let opener = runs(source, 1)[0];
        assert!(matches!(
            index.lookup(opener, 4096).unwrap().decision,
            IndexedCodeDecision::Search(_)
        ));
        assert!(!Parser::new_ext(source, Options::ENABLE_MATH)
            .any(|event| matches!(event, Event::Code(_))));
        // 这些真实反例有词法closer却不能产生最终Code，证明Index完成并不授予优先序权限。
    }
}

#[test]
fn metadata_failure_cancellation_and_frame_end_never_publish_partial_index() {
    let source = "x `one` ``two``";
    let limit = metadata_bound(0).unwrap();
    let mut builder =
        CodeLastRunIndexBuilder::new(11, source.len(), 0..source.len(), limit).unwrap();
    loop {
        match builder.advance(
            frame(source, builder.next_byte(), 1),
            MIN_CODE_INDEX_STEP_WORK,
        ) {
            Ok(step) => {
                assert_ne!(step.status, CodeIndexStatus::Complete);
                assert!(builder.take_complete().unwrap().is_none());
            }
            Err(error) => {
                assert_eq!(error, CodeIndexError::MetadataLimit);
                break;
            }
        }
    }
    assert!(builder.last.is_empty());
    assert!(builder.cursor.is_none());
    assert!(matches!(
        builder.take_complete(),
        Err(CodeIndexError::MetadataLimit)
    ));
    let mut builder = CodeLastRunIndexBuilder::new(
        11,
        source.len(),
        0..source.len(),
        MAX_CODE_INDEX_METADATA_BYTES,
    )
    .unwrap();
    for _ in 0..6 {
        builder
            .advance(
                frame(source, builder.next_byte(), 1),
                MIN_CODE_INDEX_STEP_WORK,
            )
            .unwrap();
    }
    builder.cancel();
    assert!(builder.last.is_empty());
    assert!(builder.cursor.is_none());
    assert!(matches!(
        builder.take_complete(),
        Err(CodeIndexError::Cancelled)
    ));
    let mut builder = CodeLastRunIndexBuilder::new(
        11,
        source.len(),
        0..source.len(),
        MAX_CODE_INDEX_METADATA_BYTES,
    )
    .unwrap();
    let step = builder
        .advance(frame(source, 0, 3), MIN_CODE_INDEX_STEP_WORK)
        .unwrap();
    assert_eq!(step.status, CodeIndexStatus::NeedInput);
    assert!(builder.take_complete().unwrap().is_none());
    let bad = SourceFrame {
        source_revision: 11,
        byte_start: builder.next_byte(),
        bytes: &[],
        real_eof: true,
    };
    assert!(matches!(
        builder.advance(bad, MIN_CODE_INDEX_STEP_WORK),
        Err(CodeIndexError::InvalidFrame)
    ));
    assert!(matches!(
        builder.take_complete(),
        Err(CodeIndexError::InvalidFrame)
    ));
}
