// Copyright 2015 Google Inc. All rights reserved.
// 固定上游实现与分段候选的纯差分；MIT许可见LICENSE。

use super::*;
use crate::{
    block_frontier::SourceFrame, firstpass::run_first_pass, parse::ItemBody, Event, Options, Parser,
};
use std::ops::Range;

fn frame(text: &str, at: usize, width: usize) -> SourceFrame<'_> {
    let end = (at + width).min(text.len());
    SourceFrame {
        source_revision: 7,
        byte_start: at,
        bytes: &text.as_bytes()[at..end],
        real_eof: end == text.len(),
    }
}

fn candidates(text: &str, width: usize, budget: usize) -> Vec<InlineCandidate> {
    let mut scanner = InlineLexicalCursor::new(7, text.len(), 0..text.len()).unwrap();
    let mut output = Vec::new();
    loop {
        let step = scanner
            .advance(frame(text, scanner.next_byte(), width), budget)
            .unwrap();
        assert!(step.work.total() <= budget);
        match step.status {
            InlineStepStatus::Candidate(value) => output.push(value),
            InlineStepStatus::Complete => break,
            InlineStepStatus::Progress | InlineStepStatus::NeedInput => {}
        }
    }
    output
}

fn firstpass_code_runs(text: &str) -> Vec<(Range<usize>, usize, bool)> {
    let (mut tree, _) = run_first_pass(text, Options::empty());
    tree.reset();
    let mut pending: Vec<_> = tree.cur().into_iter().collect();
    let mut result = Vec::new();
    while let Some(index) = pending.pop() {
        let node = tree[index];
        if let Some(next) = node.next {
            pending.push(next);
        }
        if let Some(child) = node.child {
            pending.push(child);
        }
        if let ItemBody::MaybeCode(count, escaped) = node.item.body {
            result.push((node.item.start..node.item.end, count, escaped));
        }
    }
    result
}

#[test]
fn real_firstpass_delimiters_are_identical_at_every_small_frame_boundary() {
    let mut corpus = vec![
        "x `foo`".to_owned(),
        "x \\`foo``bar``".to_owned(),
        "x \\``foo`bar`".to_owned(),
        "x \\\\`foo` &grave; &#96; &bogus`x`".to_owned(),
        "x 中文😀 `a\r\nb` &CounterClockwiseContourIntegral;".to_owned(),
    ];
    let alphabet = *b"`\\&;#x0a ";
    let mut state = 19u64;
    for _ in 0..128 {
        let mut text = String::from("x ");
        for _ in 0..97 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(char::from(alphabet[(state as usize) % alphabet.len()]));
        }
        corpus.push(text);
    }
    for text in corpus {
        let expected = firstpass_code_runs(&text);
        for width in 1..=17 {
            let actual: Vec<_> = candidates(&text, width, 1024)
                .into_iter()
                .filter_map(|candidate| match candidate {
                    InlineCandidate::CodeDelimiter(run) => {
                        Some((run.range(), run.raw_count(), run.preceded_by_backslash()))
                    }
                    InlineCandidate::Entity(_) => None,
                })
                .collect();
            assert_eq!(actual, expected, "frame={width}, source={text:?}");
        }
    }
}

// 原0.13.4 scan_entity的独立oracle，保留旧whole-slice规则以防共享提取后自比自。
fn original_entity(bytes: &[u8]) -> (usize, Option<crate::CowStr<'static>>) {
    let mut end = 1;
    if bytes.get(end) == Some(&b'#') {
        end += 1;
        let (radix, maximum) = if bytes
            .get(end)
            .is_some_and(|b| b.eq_ignore_ascii_case(&b'x'))
        {
            end += 1;
            (16, 6)
        } else {
            (10, 7)
        };
        let mut number = 0usize;
        let mut count = 0;
        for &byte in bytes[end..].iter().take(maximum) {
            let Some(digit) = char::from(byte).to_digit(radix) else {
                break;
            };
            let Some(next) = number
                .checked_mul(radix as usize)
                .and_then(|v| v.checked_add(digit as usize))
            else {
                break;
            };
            number = next;
            count += 1;
        }
        end += count;
        if count == 0 || bytes.get(end) != Some(&b';') {
            return (0, None);
        }
        let value = u32::try_from(number)
            .ok()
            .filter(|&n| n != 0)
            .and_then(char::from_u32)
            .unwrap_or('\u{fffd}');
        return (end + 1, Some(value.into()));
    }
    while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
        end += 1;
    }
    if bytes.get(end) == Some(&b';') {
        if let Some(value) = crate::entities::get_entity(&bytes[1..end]) {
            return (end + 1, Some(value.into()));
        }
    }
    (0, None)
}

#[test]
fn entities_share_original_numeric_name_escape_and_frame_semantics() {
    let mut corpus = vec![
        "&amp;",
        "&NotEqualTilde;",
        "&CounterClockwiseContourIntegral;",
        "&;",
        "&#;",
        "&#x;",
        "&#0;",
        "&#xD800;",
        "&#1114112;",
        "&#xFFFFFF;",
        "&#00000000;",
        "&#x0000000;",
        "&#128;",
        "&a`",
        "&AMP;",
        "&unknown;",
        "&amp",
        "&中文;",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut random = 31u64;
    let alphabet = b"&#xX0123456789abcdefABCDEF;ampZ` ";
    for _ in 0..10_000 {
        let mut raw = String::from("&");
        for _ in 0..12 {
            random = random
                .wrapping_mul(2862933555777941757)
                .wrapping_add(3037000493);
            raw.push(char::from(alphabet[random as usize % alphabet.len()]));
        }
        corpus.push(raw);
    }
    for raw in &corpus {
        assert_eq!(
            crate::scanners::scan_entity(raw.as_bytes()),
            original_entity(raw.as_bytes())
        );
    }
    for raw in corpus.iter().take(19) {
        let expected = original_entity(raw.as_bytes());
        for width in 1..=raw.len() {
            let values = candidates(raw, width, 1024);
            let entity = values.iter().find_map(|value| match value {
                InlineCandidate::Entity(entity) => Some(*entity),
                _ => None,
            });
            assert_eq!(
                entity.map(|e| (e.range().end, Some(e.decoded().into_cow()))),
                (expected.0 != 0).then_some(expected.clone())
            );
        }
    }
    assert!(candidates("\\&amp;", 1, 1024).is_empty());
    assert_eq!(
        crate::scanners::scan_entity(b"&#128;").1.unwrap().as_ref(),
        "\u{80}"
    );
}

fn probe_span(
    text: &str,
    opener: CodeDelimiterCandidate,
    width: usize,
) -> Option<CodeSpanCandidate> {
    let mut probe = CodeSpanProbe::from_opener(opener, text.len(), text.len()).unwrap()?;
    loop {
        let step = probe
            .advance(frame(text, probe.next_byte(), width), 1024)
            .unwrap();
        match step {
            CodeSpanStep::Candidate { work, candidate } => {
                assert!(work.total() <= 1024);
                return Some(candidate);
            }
            CodeSpanStep::Unmatched { work, replay_from } => {
                assert!(work.total() <= 1024);
                assert_eq!(replay_from, opener.range().end);
                return None;
            }
            CodeSpanStep::Progress { work } | CodeSpanStep::NeedInput { work } => {
                assert!(work.total() <= 1024)
            }
        }
    }
}

#[test]
fn original_code_events_same_closer_and_unmatched_replay_remain_exact() {
    // 固定spec327–349的实际语法族；只比range，不把规范化code正文复制到产品状态。
    for raw in [
        "`hi`lo`",
        "`` foo ` bar ``",
        "`foo\nbar`",
        "`foo\\`bar`",
        "``foo`bar``",
        "*foo`*`",
        "[not a `link](/foo`)",
        "`<a href=\"`\">`",
        "```foo``",
        "`foo",
        "`foo``bar``",
        "\\``foo`",
        "`a\\`b`",
    ] {
        let text = format!("x {raw}");
        let expected: Vec<_> = Parser::new(&text)
            .into_offset_iter()
            .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
            .collect();
        for width in 1..=13 {
            let runs: Vec<_> = candidates(&text, width, 1024)
                .into_iter()
                .filter_map(|value| match value {
                    InlineCandidate::CodeDelimiter(run) => Some(run),
                    _ => None,
                })
                .collect();
            let mut actual = Vec::new();
            let mut committed = 0;
            for run in runs {
                if run.range().start < committed {
                    continue;
                }
                if let Some(span) = probe_span(&text, run, width) {
                    committed = span.range().end;
                    actual.push(span.range());
                }
            }
            assert_eq!(actual, expected, "frame={width}, source={text:?}");
        }
    }
    // 相反证据：HTML先被消费时原Parser没有code，但词法仍有ticks；候选绝不是Ready样式。
    let html_first = "x <a href=\"`\">`";
    assert!(!candidates(html_first, 1, 1024).is_empty());
    assert!(!Parser::new(html_first).any(|event| matches!(event, Event::Code(_))));
}

#[test]
fn giant_spans_unclosed_entities_and_cancellation_keep_constant_state() {
    assert!(!std::mem::needs_drop::<InlineLexicalCursor>());
    assert!(!std::mem::needs_drop::<CodeSpanProbe>());
    assert!(std::mem::size_of::<InlineLexicalCursor>() <= 192);
    assert!(std::mem::size_of::<CodeSpanProbe>() <= 256);
    let text = format!("x `{}尾😀`", "a".repeat(1_048_577));
    let runs = candidates(&text, 997, 1024);
    let InlineCandidate::CodeDelimiter(open) = runs[0] else {
        panic!("真实opener必须保留");
    };
    let span = probe_span(&text, open, 1999).unwrap();
    let expected = Parser::new(&text)
        .into_offset_iter()
        .find_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
        .unwrap();
    assert_eq!(span.range(), expected);
    let long_run = format!("x {} tail", "`".repeat(1_048_577));
    let runs = candidates(&long_run, 997, 1024);
    let InlineCandidate::CodeDelimiter(run) = runs[0] else {
        panic!("巨长run也只产生一个真实候选");
    };
    assert_eq!(run.raw_count(), 1_048_577);
    // 没有closing的一字节改动必须等真实paragraph-end；不能把中间frame末判作unmatched。
    assert!(probe_span(&text[..text.len() - 1], open, 511).is_none());
    let long_entity = format!("&{} `later`", "a".repeat(1_048_577));
    let output = candidates(&long_entity, 1021, 2048);
    assert_eq!(output.len(), 2);
    let mut cursor = InlineLexicalCursor::new(7, text.len(), 0..text.len()).unwrap();
    cursor.advance(frame(&text, 0, 1024), 1024).unwrap();
    cursor.cancel();
    assert_eq!(
        cursor.advance(frame(&text, cursor.next_byte(), 1024), 1024),
        Err(InlineError::Cancelled)
    );
}

#[test]
fn explicit_logical_end_invalid_eof_and_zero_byte_transitions_are_distinct() {
    let text = "x `open suffix";
    let mut cursor = InlineLexicalCursor::new(7, text.len(), 0..3).unwrap();
    let first = cursor.advance(frame(text, 0, 3), 1024).unwrap();
    assert!(matches!(first.status, InlineStepStatus::Candidate(_)));
    assert!(matches!(
        cursor.advance(frame(text, 3, 1), 1024).unwrap().status,
        InlineStepStatus::Complete
    ));
    let mut cursor = InlineLexicalCursor::new(7, text.len(), 0..text.len()).unwrap();
    let step = cursor.advance(frame(text, 0, 3), 1024).unwrap();
    assert_eq!(step.status, InlineStepStatus::NeedInput);
    let invalid = SourceFrame {
        source_revision: 7,
        byte_start: 3,
        bytes: &[],
        real_eof: true,
    };
    assert_eq!(
        cursor.advance(invalid, 1024),
        Err(InlineError::InvalidFrame)
    );
    assert_eq!(
        cursor.advance(frame(text, 3, 1), 1024),
        Err(InlineError::InvalidFrame)
    );
    let InlineCandidate::CodeDelimiter(open) = candidates(text, 1, 1024)[0] else {
        panic!("fixture必须保留真实opener");
    };
    let mut probe = CodeSpanProbe::from_opener(open, text.len(), text.len())
        .unwrap()
        .unwrap();
    let mut invalid = frame(text, probe.next_byte(), 1);
    invalid.source_revision = 8;
    assert_eq!(probe.advance(invalid, 1024), Err(InlineError::InvalidFrame));
    assert_eq!(
        probe.advance(frame(text, probe.next_byte(), 1), 1024),
        Err(InlineError::InvalidFrame)
    );
}
