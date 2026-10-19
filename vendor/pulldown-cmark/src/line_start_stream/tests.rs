// Copyright 2015 Google Inc. All rights reserved.
// 测试固定0.13.4原实现与分段共享kernel的返回值、标量和工作预算，MIT条款见LICENSE。

use super::oracle::OracleLineStart;
use super::*;

/// 期望值只来自独立保留的原实现；四个标量一并比较，不能仅比返回值。
fn oracle(
    bytes: &[u8],
    state: LineStartState,
    operation: LineStartOperation,
) -> (LineStartState, LineStartValue) {
    if operation == LineStartOperation::BlankLine {
        // 独立保留原 scan_whitespace_no_nl + scan_eol 定义，不能调用已接新 kernel 的适配器自证。
        let tail = &bytes[state.ix..];
        let leading = tail
            .iter()
            .take_while(|&&byte| matches!(byte, b' ' | b'\t' | 0x0b | 0x0c))
            .count();
        let eol = match &tail[leading..] {
            [] => Some(0),
            [b'\r', b'\n', ..] => Some(2),
            [b'\r' | b'\n', ..] => Some(1),
            _ => None,
        };
        let count = eol.map(|eol| leading + eol);
        let mut expected = state;
        if let Some(count) = count {
            expected.ix += count;
        }
        return (expected, LineStartValue::BlankLine(count));
    }
    let mut old = OracleLineStart::from_state(bytes, state);
    let value = match operation {
        LineStartOperation::Space(count) => {
            LineStartValue::SpaceRemaining(count - old.scan_space_upto(count))
        }
        LineStartOperation::AllSpace => {
            old.scan_all_space();
            LineStartValue::Unit
        }
        LineStartOperation::AtEol => LineStartValue::Bool(old.is_at_eol()),
        LineStartOperation::BlankLine => unreachable!("blank 原函数期望值在上方独立计算"),
        LineStartOperation::Quote => LineStartValue::Bool(old.scan_blockquote_marker()),
        LineStartOperation::Definition(indent) => LineStartValue::Definition(
            old.scan_definition_list_definition_marker_with_indent(indent),
        ),
        LineStartOperation::List(indent) => {
            LineStartValue::List(old.scan_list_marker_with_indent(indent))
        }
    };
    // bytes_scanned/remaining_space仍是原公共内部方法；同时验证新导出的state含义。
    assert_eq!(old.bytes_scanned(), old.state().ix);
    assert_eq!(old.remaining_space(), old.state().spaces_remaining);
    (old.state(), value)
}

/// 每次只借一个固定宽frame；NeedInput可要求回到早先anchor，但没有正文拼接或行buffer。
fn segmented(
    bytes: &[u8],
    state: LineStartState,
    operation: LineStartOperation,
    width: usize,
    budget: usize,
    origin: usize,
) -> (LineStartState, LineStartValue, LineStartWork) {
    let mut stream =
        LineStartStream::new(17, origin, origin + bytes.len(), state, operation).unwrap();
    let mut next = stream.next_byte();
    let mut old_total = LineStartWork::default();
    for _ in 0..(bytes.len() * 8 + 512) {
        // 全部请求都必须属于原source区间；source_end只允许作为真实EOF锚点。
        assert!((origin..=origin + bytes.len()).contains(&next));
        let local = next - origin;
        let start = if local == bytes.len() {
            local
        } else {
            local / width * width
        };
        let end = (start + width).min(bytes.len());
        let step = stream
            .advance(
                LineStartFrame {
                    source_revision: 17,
                    byte_start: origin + start,
                    bytes: &bytes[start..end],
                    real_eof: end == bytes.len(),
                },
                budget,
                budget,
            )
            .unwrap();
        assert!(step.work.byte_reads <= budget);
        assert!(step.work.transitions <= budget);
        let total = stream.total_work();
        assert_eq!(
            total.byte_reads,
            old_total.byte_reads + step.work.byte_reads
        );
        assert_eq!(
            total.transitions,
            old_total.transitions + step.work.transitions
        );
        old_total = total;
        match step.progress {
            LineStartProgress::Complete { state, value } => return (state, value, total),
            LineStartProgress::Progress { next_byte }
            | LineStartProgress::NeedInput { next_byte } => next = next_byte,
        }
    }
    panic!("有限源扫描没有按预算取得单调推进");
}

/// frame可在任意单一位置断开，包括UTF8/CRLF；oracle始终拿真实完整源作独立对照。
fn one_cut(bytes: &[u8], operation: LineStartOperation, cut: usize) {
    let mut stream =
        LineStartStream::new(3, 0, bytes.len(), LineStartState::default(), operation).unwrap();
    let mut next = 0;
    for _ in 0..(bytes.len() * 8 + 512) {
        let (start, end) = if next < cut {
            (0, cut)
        } else if next < bytes.len() {
            (cut, bytes.len())
        } else {
            (bytes.len(), bytes.len())
        };
        let step = stream
            .advance(
                LineStartFrame {
                    source_revision: 3,
                    byte_start: start,
                    bytes: &bytes[start..end],
                    real_eof: end == bytes.len(),
                },
                1,
                1,
            )
            .unwrap();
        match step.progress {
            LineStartProgress::Complete { state, value } => {
                assert_eq!(
                    (state, value),
                    oracle(bytes, LineStartState::default(), operation),
                    "cut={cut}, operation={operation:?}, bytes={bytes:?}"
                );
                return;
            }
            LineStartProgress::Progress { next_byte }
            | LineStartProgress::NeedInput { next_byte } => next = next_byte,
        }
    }
    panic!("单切分没有完成");
}

#[test]
fn exact_return_and_mutation_at_every_frame_boundary() {
    let cases: &[&[u8]] = &[
        b"",
        b"\r",
        b"\r\nx",
        b"\t >\t- item\r\n",
        b"- \r\nrest",
        b"* * * \r\n",
        b"* * x\n",
        b"-\t\t\t\t tail",
        b"+          \x0b\x0c\r\n",
        b":     x",
        b":\t x",
        b"123456789. x",
        b"1234567890. x",
        b"99)x",
        "> 中文🙂\r\n".as_bytes(),
    ];
    let operations = [
        LineStartOperation::Space(0),
        LineStartOperation::Space(1),
        LineStartOperation::Space(3),
        LineStartOperation::Space(8),
        LineStartOperation::AllSpace,
        LineStartOperation::AtEol,
        LineStartOperation::BlankLine,
        LineStartOperation::Quote,
        LineStartOperation::Definition(2),
        LineStartOperation::List(3),
    ];
    for bytes in cases {
        // 原构造器也在oracle中实际执行，确保默认scalar不是测试自行猜测。
        assert_eq!(
            OracleLineStart::new(bytes).state(),
            LineStartState::default()
        );
        for operation in operations {
            for cut in 0..=bytes.len() {
                one_cut(bytes, operation, cut);
            }
        }
    }
}

#[test]
fn state_carries_tab_columns_and_min_hrule_through_operation_sequences() {
    let sources: &[&[u8]] = &[
        b"\t> \t-    item",
        b" \t:   x",
        b"- \r\n-   x",
        b" >\t+     item",
        b" \t123.\titem",
        "\t> 中文🙂".as_bytes(),
    ];
    let sequences = [
        LineStartOperation::Space(3),
        LineStartOperation::Quote,
        LineStartOperation::Space(1),
        LineStartOperation::List(2),
        LineStartOperation::Definition(1),
        LineStartOperation::Space(2),
        LineStartOperation::List(0),
        LineStartOperation::AtEol,
        LineStartOperation::AllSpace,
    ];
    for bytes in sources {
        for width in 1..=bytes.len().max(1) {
            let mut state = LineStartState::default();
            for operation in sequences {
                let expected = oracle(bytes, state, operation);
                let actual = segmented(bytes, state, operation, width, 1, 913);
                assert_eq!(
                    (actual.0, actual.1),
                    expected,
                    "width={width}, operation={operation:?}, state={state:?}"
                );
                state = actual.0;
            }
        }
    }
}

#[test]
fn deterministic_operation_differential_uses_real_upstream_oracle() {
    let alphabet = b" \t-*+>:10.)\r\nx\x0b\x0c";
    let mut seed = 0x7d3a_947b_24f0_1683_u64;
    for case in 0..2048 {
        // 固定伪随机输入不依赖外部库；只是测试fixture，不进入运行时语法。
        let len = case % 83;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            bytes.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
        }
        let operations = [
            LineStartOperation::Space(case % 13),
            LineStartOperation::AllSpace,
            LineStartOperation::AtEol,
            LineStartOperation::Quote,
            LineStartOperation::Definition(case % 4),
            LineStartOperation::List(case % 4),
        ];
        for operation in operations {
            let expected = oracle(&bytes, LineStartState::default(), operation);
            for width in [1, 2, 3, 7, 64] {
                let actual = segmented(
                    &bytes,
                    LineStartState::default(),
                    operation,
                    width,
                    1 + (case % 7),
                    0,
                );
                assert_eq!(
                    (actual.0, actual.1),
                    expected,
                    "case={case}, width={width}, op={operation:?}, bytes={bytes:?}"
                );
            }
        }
    }
}

#[test]
fn giant_hrule_and_blank_lookahead_have_constant_state_and_charged_linear_work() {
    for &marker in b"-+*" {
        let mut bytes = vec![b' '; 2 * 1024 * 1024];
        bytes[0] = marker;
        bytes.extend_from_slice(b"x\r\n| h |\n| - |\n| v |\n");
        let operation = LineStartOperation::List(0);
        let expected = oracle(&bytes, LineStartState::default(), operation);
        let actual = segmented(
            &bytes,
            LineStartState::default(),
            operation,
            MAX_LINE_START_STEP,
            1024,
            100,
        );
        assert_eq!((actual.0, actual.1), expected);
        // 一次hrule加一次blank probe及有限前后阶段；工作量按读取计费，不按commit ix伪装常数。
        assert!(actual.2.byte_reads <= bytes.len() * 2 + 64);
        assert!(actual.2.transitions <= actual.2.byte_reads + 32);
        assert!(actual.0.ix < 16, "提交位置不能等于lookahead尾端");
    }
    assert!(!std::mem::needs_drop::<LineStartStream>());
    assert!(std::mem::size_of::<LineStartStream>() <= 320);
}

#[test]
fn long_true_hrule_rejects_marker_without_retaining_the_line() {
    let mut bytes = vec![b'-'; 1024 * 1024];
    bytes.extend_from_slice(b"\r\n| h |\n| - |\n");
    let actual = segmented(
        &bytes,
        LineStartState::default(),
        LineStartOperation::List(2),
        17,
        7,
        0,
    );
    assert_eq!(
        (actual.0, actual.1),
        oracle(
            &bytes,
            LineStartState::default(),
            LineStartOperation::List(2)
        )
    );
    assert_eq!(actual.0, LineStartState::default());
    assert_eq!(actual.1, LineStartValue::List(None));
    assert!(actual.2.byte_reads <= bytes.len() + 8);
}

#[test]
fn absent_frame_is_not_end_of_file_and_crlf_waits_for_the_next_byte() {
    for operation in [
        LineStartOperation::AtEol,
        LineStartOperation::Quote,
        LineStartOperation::List(0),
    ] {
        let mut stream =
            LineStartStream::new(9, 44, 44, LineStartState::default(), operation).unwrap();
        let pending = stream
            .advance(
                LineStartFrame {
                    source_revision: 9,
                    byte_start: 44,
                    bytes: b"",
                    real_eof: false,
                },
                1,
                1,
            )
            .unwrap();
        assert_eq!(
            pending.progress,
            LineStartProgress::NeedInput { next_byte: 44 }
        );
        let done = stream
            .advance(
                LineStartFrame {
                    source_revision: 9,
                    byte_start: 44,
                    bytes: b"",
                    real_eof: true,
                },
                8,
                8,
            )
            .unwrap();
        assert!(matches!(done.progress, LineStartProgress::Complete { .. }));
    }
    let bytes = b"- \r\nrest";
    let mut stream = LineStartStream::new(
        9,
        0,
        bytes.len(),
        LineStartState::default(),
        LineStartOperation::List(0),
    )
    .unwrap();
    let first = stream
        .advance(
            LineStartFrame {
                source_revision: 9,
                byte_start: 0,
                bytes: &bytes[..3],
                real_eof: false,
            },
            32,
            32,
        )
        .unwrap();
    assert_eq!(
        first.progress,
        LineStartProgress::NeedInput { next_byte: 3 }
    );
    assert!(stream.total_work().byte_reads <= 4);
    // CR后的未知byte决定原Err offset是3还是4，不能在上一frame末尾提前提交。
    let actual = segmented(
        bytes,
        LineStartState::default(),
        LineStartOperation::List(0),
        3,
        2,
        0,
    );
    assert_eq!(
        (actual.0, actual.1),
        oracle(
            bytes,
            LineStartState::default(),
            LineStartOperation::List(0)
        )
    );
}

#[test]
fn invalid_source_identity_range_and_budget_do_not_advance() {
    let mut stream = LineStartStream::new(
        5,
        10,
        13,
        LineStartState::default(),
        LineStartOperation::Quote,
    )
    .unwrap();
    let initial = stream.total_work();
    for frame in [
        LineStartFrame {
            source_revision: 6,
            byte_start: 10,
            bytes: b">",
            real_eof: false,
        },
        LineStartFrame {
            source_revision: 5,
            byte_start: 9,
            bytes: b">",
            real_eof: false,
        },
        LineStartFrame {
            source_revision: 5,
            byte_start: 10,
            bytes: b">",
            real_eof: true,
        },
        LineStartFrame {
            source_revision: 5,
            byte_start: 13,
            bytes: b">",
            real_eof: true,
        },
    ] {
        assert_eq!(
            stream.advance(frame, 1, 1),
            Err(LineStartError::InvalidFrame)
        );
        assert_eq!(stream.total_work(), initial);
    }
    assert_eq!(
        stream.advance(
            LineStartFrame {
                source_revision: 5,
                byte_start: 10,
                bytes: b">",
                real_eof: false
            },
            0,
            1
        ),
        Err(LineStartError::InvalidBudget)
    );
    assert_eq!(stream.total_work(), initial);
    assert_eq!(stream.next_byte(), 10);
}

#[test]
fn ordered_marker_digit_limit_does_not_request_an_unneeded_eleventh_byte() {
    let bytes = b"9999999999.anything";
    let mut stream = LineStartStream::new(
        1,
        0,
        bytes.len(),
        LineStartState::default(),
        LineStartOperation::List(0),
    )
    .unwrap();
    let step = stream
        .advance(
            LineStartFrame {
                source_revision: 1,
                byte_start: 0,
                bytes: &bytes[..10],
                real_eof: false,
            },
            32,
            32,
        )
        .unwrap();
    assert_eq!(
        step.progress,
        LineStartProgress::Complete {
            state: LineStartState::default(),
            value: LineStartValue::List(None)
        }
    );
    assert_eq!(step.work.byte_reads, 10);
}
