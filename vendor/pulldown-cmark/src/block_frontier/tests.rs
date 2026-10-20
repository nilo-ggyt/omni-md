//! 真实Parser种子和事件作oracle；原型结果只留当前row的纯事实。
use super::*;
use crate::{Event, Parser, Tag};

pub(crate) struct CapturedTableSeed {
    pub roles: Vec<ContainerRole>,
    pub columns: usize,
    pub header_byte_start: usize,
    pub body_start: usize,
}

fn dialect() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_GFM
}

/// 种子必须来自真实、有界prefix，省略后续body不能反向改变已消费的容器和header事实。
fn seed(prefix: &str, full: &str) -> TableContinuationState {
    assert!(full.starts_with(prefix));
    let mut seeds = crate::firstpass::capture_table_seeds(prefix, dialect());
    let seed = seeds.pop().expect("真实prefix必须建立表");
    assert_eq!(seed.body_start, prefix.len());
    let mut source_line = 0;
    let mut previous_cr = false;
    for byte in prefix.bytes() {
        if byte == b'\r' || (byte == b'\n' && !previous_cr) {
            source_line += 1;
        }
        previous_cr = byte == b'\r';
    }
    TableContinuationState::resume(TableCheckpoint {
        spine: StructuralSpineSnapshot::from_roles(&seed.roles),
        scalars: TableScalars {
            source_revision: 17,
            source_end: full.len(),
            header_byte_start: prefix.as_bytes()[..seed.header_byte_start]
                .iter()
                .rposition(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(0, |index| index + 1),
            // 旧9项只校验续行语义；本fixture的D完整物理行范围由真实prefix的最后一行取得。
            // 需要容器continuation prefix的新增证明使用下方真实global构造，不能用此test seed冒充。
            separator_byte_start: prefix
                .trim_end_matches(['\r', '\n'])
                .rfind(['\r', '\n'])
                .map_or(0, |ix| ix + 1),
            separator_prefix_bytes: 0,
            last_line_terminated: prefix.ends_with(['\r', '\n']),
            columns: seed.columns,
            next_row_ordinal: 1,
            next_source_line: source_line,
            next_byte: prefix.len(),
            next_utf16: prefix.encode_utf16().count(),
            missing_cells: 0,
        },
        options: dialect(),
        header_inline: cell::TableInlineRow {
            ordinal: 0,
            start: prefix.as_bytes()[..seed.header_byte_start]
                .iter()
                .rposition(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(0, |index| index + 1),
            content_start: seed.header_byte_start,
            content_end: prefix[seed.header_byte_start..]
                .find(['\r', '\n'])
                .map_or(prefix.len(), |index| seed.header_byte_start + index),
            end: prefix[seed.header_byte_start..]
                .find('\n')
                .map_or(prefix.len(), |index| seed.header_byte_start + index + 1),
        },
        last_inline: None,
    })
}

/// 任意frame split只控制交付边界；每次消费后推进精确offset，不重放已消费byte。
fn consume(
    state: &mut TableContinuationState,
    full: &str,
    chunks: &[usize],
    budget: usize,
) -> (Vec<TableRowFact>, usize) {
    let mut rows = Vec::new();
    for _ in 0..full.len().saturating_mul(100) + 20000 {
        let offset = state.next_byte();
        let frame_end = chunks
            .get(chunks.partition_point(|end| *end <= offset))
            .copied()
            .unwrap_or(full.len())
            .min(offset + MAX_FRONTIER_FRAME_BYTES);
        let step = state
            .advance(
                SourceFrame {
                    source_revision: 17,
                    byte_start: offset,
                    bytes: &full.as_bytes()[offset..frame_end],
                    real_eof: frame_end == full.len(),
                },
                budget,
            )
            .unwrap();
        let work = state.last_work();
        assert!(work.byte_reads <= budget && work.transitions <= MAX_FRONTIER_STEP_BYTES);
        match step {
            TableStep::Row { fact, .. } => rows.push(fact),
            TableStep::Ended { byte_offset, .. } => return (rows, byte_offset),
            TableStep::Progress { .. } => assert!(work.transitions > 0),
            TableStep::NeedInput { .. } => (),
        }
    }
    panic!("续体没有单调工作进展")
}

/// 只替换测试交付器：真实分类前瞻可以回放，所有原Row/Ended/错误断言不依赖同步finish_row时机。
fn advance_until_visible(
    state: &mut TableContinuationState,
    full: &str,
    budget: usize,
) -> Result<TableStep, FrontierError> {
    loop {
        let offset = state.next_byte();
        let end = (offset + MAX_FRONTIER_FRAME_BYTES).min(full.len());
        let step = state.advance(
            SourceFrame {
                source_revision: 17,
                byte_start: offset,
                bytes: &full.as_bytes()[offset..end],
                real_eof: end == full.len(),
            },
            budget,
        )?;
        let work = state.last_work();
        assert!(work.byte_reads <= budget && work.transitions <= MAX_FRONTIER_STEP_BYTES);
        if matches!(step, TableStep::Row { .. } | TableStep::Ended { .. }) {
            return Ok(step);
        }
        assert!(work.transitions > 0);
    }
}

fn oracle_rows(source: &str) -> Vec<(std::ops::Range<usize>, usize)> {
    let mut rows = Vec::new();
    let mut active = false;
    for (event, range) in Parser::new_ext(source, dialect()).into_offset_iter() {
        match event {
            Event::Start(Tag::TableRow) => {
                rows.push((range, 0));
                active = true;
            }
            Event::Start(Tag::TableCell) if active => rows.last_mut().unwrap().1 += 1,
            Event::End(crate::TagEnd::TableRow) => active = false,
            Event::End(crate::TagEnd::Table) => break,
            _ => {}
        }
    }
    rows
}

#[test]
fn deterministic_escape_and_interrupt_rows_match_real_parser_cells() {
    let prefix = "| A | B | C |\n| - | - | - |\n";
    let alphabet = b"a |\\`&;*_-[]<>\t\n";
    let mut random = 0xa4b9d8397e52c113_u64;
    for length in 0..4096 {
        // 合成源仅在test持有；原型每个step只接所需byte frame，不借完整source字符串。
        let mut tail = String::new();
        for _ in 0..(length % 97) {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            tail.push(char::from(alphabet[random as usize % alphabet.len()]));
        }
        let full = format!("{prefix}{tail}\n");
        let expected = oracle_rows(&full);
        let mut state = seed(prefix, &full);
        let (actual, _) = consume(&mut state, &full, &[], 11);
        assert_eq!(actual.len(), expected.len(), "tail={tail:?}");
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.byte_end, expected.0.end, "tail={tail:?}");
            assert_eq!(
                actual.actual_cells.min(3) + actual.missing_cells,
                expected.1,
                "tail={tail:?}"
            );
        }
    }
}

#[test]
fn arbitrary_byte_frames_preserve_unicode_crlf_cells_and_no_eol_tail() {
    let prefix = "| H | B | C |\r\n| - | - | - |\r\n";
    let full = format!("{prefix}| 中😀 | `a\\|b` | |\r\nplain\r\n| a\\\\|x | **e\u{301}** | fin |");
    let oracle = oracle_rows(&full);
    for split in prefix.len()..=full.len() {
        // 包括UTF8 continuation和CRLF中间的每个byte位置，不只测试Rust合法str slice边界。
        let mut state = seed(prefix, &full);
        let (rows, end) = consume(&mut state, &full, &[split], 3);
        assert_eq!(rows.len(), oracle.len(), "split={split}");
        for (index, (actual, expected)) in rows.iter().zip(&oracle).enumerate() {
            assert_eq!(actual.row_ordinal, index + 1);
            assert_eq!(actual.byte_end, expected.0.end);
            assert_eq!(
                actual.actual_cells.min(3) + actual.missing_cells,
                expected.1
            );
            assert_eq!(
                actual.utf16_start,
                full[..actual.byte_start].encode_utf16().count()
            );
            assert_eq!(
                actual.utf16_end,
                full[..actual.byte_end].encode_utf16().count()
            );
        }
        assert_eq!(end, full.len());
        assert_eq!(state.buffered_source_capacity(), 0);
    }
}

#[test]
fn actual_container_spine_and_interrupts_match_full_source() {
    for (prefix, tails) in [
        (
            "| H | B |\n| - | - |\n",
            vec![
                "plain\n",
                "# stop\n",
                "---\n",
                "```rust\n",
                "- item\n",
                "<div>\n",
                "[^n]: stop\n",
                "| # cell | x |\n",
            ],
        ),
        (
            "> | H |\n> | - |\n",
            vec!["> | yes |\n", "| outside |\n", "> plain\n", ">\n"],
        ),
        (
            "-   item\n\n    | H |\n    | - |\n",
            vec!["    | yes |\n", "| outside |\n", "    plain\n"],
        ),
        (
            "[^n]: note\n\n    | H |\n    | - |\n",
            vec!["    | yes |\n", "| outside |\n"],
        ),
    ] {
        for tail in tails {
            let full = format!("{prefix}{tail}");
            let oracle = oracle_rows(&full);
            let mut state = seed(prefix, &full);
            let chunks = (prefix.len()..full.len()).collect::<Vec<_>>();
            let (rows, _) = consume(&mut state, &full, &chunks, 2);
            assert_eq!(rows.len(), oracle.len(), "{full:?}");
            for (actual, expected) in rows.iter().zip(oracle) {
                assert_eq!(actual.byte_end, expected.0.end);
            }
        }
    }
}

#[test]
fn frame_exhaustion_does_not_close_or_commit_a_partial_row() {
    let prefix = "| H | B |\n| - | - |\n";
    let full = format!("{prefix}| 中😀 | final |\r\n");
    let mut state = seed(prefix, &full);
    let middle = prefix.len() + 4;
    loop {
        let start = state.next_byte();
        let step = state
            .advance(
                SourceFrame {
                    source_revision: 17,
                    byte_start: start,
                    bytes: &full.as_bytes()[start..middle],
                    real_eof: false,
                },
                4,
            )
            .unwrap();
        assert!(!matches!(
            step,
            TableStep::Row { .. } | TableStep::Ended { .. }
        ));
        if state.next_byte() == middle {
            break;
        }
    }
    assert!(state.checkpoint().is_none());
    assert_eq!(state.scalars.next_row_ordinal, 1);
    assert_eq!(state.scalars.missing_cells, 0);
    assert_eq!(
        state
            .advance(
                SourceFrame {
                    source_revision: 17,
                    byte_start: middle,
                    bytes: &[],
                    real_eof: true,
                },
                1
            )
            .unwrap_err(),
        FrontierError::InvalidFrame
    );
    let (rows, _) = consume(&mut state, &full, &[], 5);
    assert_eq!(rows.len(), 1);
}

#[test]
fn checkpoints_share_spine_and_preserve_cumulative_missing_cell_fuel() {
    let prefix = "| A | B | C |\n| - | - | - |\n";
    let full = format!("{prefix}first\nsecond\n");
    let mut state = seed(prefix, &full);
    let a = state.checkpoint().unwrap();
    let b = state.checkpoint().unwrap();
    assert!(a.spine.shares_tail(&b.spine));
    state.scalars.missing_cells = MAX_TABLE_AUTOCOMPLETED_CELLS - 2;
    let step = advance_until_visible(&mut state, &full, 20).unwrap();
    assert!(matches!(
        step,
        TableStep::Row {
            fact: TableRowFact {
                missing_cells: 2,
                ..
            },
            ..
        }
    ));
    let checkpoint = state.checkpoint().unwrap();
    assert_eq!(
        checkpoint.scalars.missing_cells,
        MAX_TABLE_AUTOCOMPLETED_CELLS
    );
    let mut resumed = TableContinuationState::resume(checkpoint);
    assert_eq!(resumed.buffered_source_capacity(), 0);
    assert_eq!(
        advance_until_visible(&mut resumed, &full, 20).unwrap_err(),
        FrontierError::MissingCellFuel
    );
}

#[test]
fn long_table_admits_one_row_at_a_time_without_total_row_storage() {
    let prefix = "| A | B |\n| - | - |\n";
    let row = format!("| {} | b |\n", "x".repeat(240));
    let full = format!("{prefix}{}", row.repeat(5000));
    let mut state = seed(prefix, &full);
    let mut count = 0;
    loop {
        match advance_until_visible(&mut state, &full, 7).unwrap() {
            TableStep::Row { fact, .. } => {
                count += 1;
                assert_eq!(fact.row_ordinal, count);
                assert!(state.row.is_none());
                assert_eq!(state.buffered_source_capacity(), 0);
            }
            TableStep::Ended { .. } => break,
            _ => unreachable!(),
        }
    }
    assert_eq!(count, 5000);
    assert_eq!(state.scalars.missing_cells, 0);
}

#[test]
fn giant_row_uses_no_source_buffer_and_resumes_classification_and_utf16_work() {
    let prefix = "| H |\n| - |\n";
    let full = format!("{prefix}{}\n", "中😀".repeat(200_000));
    let mut state = seed(prefix, &full);
    let step = advance_until_visible(&mut state, &full, MAX_FRONTIER_STEP_BYTES).unwrap();
    let TableStep::Row { fact, .. } = step else {
        panic!("巨行仍是原Parser认可的一个真实成员");
    };
    assert_eq!(fact.actual_cells, 1);
    assert_eq!(fact.byte_end, full.len());
    assert_eq!(fact.utf16_end, full.encode_utf16().count());
    assert_eq!(state.buffered_source_capacity(), 0);
    assert!(state.row.is_none());
}

#[test]
fn deep_spine_checkpoints_share_storage_and_resume_real_parser_rows() {
    // 深容器只在真实小prefix seed中解析一次；1024个checkpoint不复制depth×role数组或行scratch。
    let quote = "> ".repeat(2048);
    let prefix = format!("{quote}| H |\n{quote}| - |\n");
    let full = format!("{prefix}{quote}| body |\n");
    let state = seed(&prefix, &full);
    assert_eq!(state.spine.len(), 2049);
    let first = state.checkpoint().unwrap();
    let checkpoints = (0..1024)
        .map(|_| state.checkpoint().unwrap())
        .collect::<Vec<_>>();
    assert!(checkpoints
        .iter()
        .all(|point| point.spine.shares_tail(&first.spine)));
    assert!(state.spine.len() * std::mem::size_of::<ContainerRole>() <= 64 * 1024);
    assert_eq!(state.buffered_source_capacity(), 0);
    // 真正合同是同spine检查点能继续同源表成员，不是某版结构体布局恰好小于128B。
    // 从其中一个真实检查点恢复，逐帧结果仍须与完整Parser行/cell事实一致。
    let mut resumed = TableContinuationState::resume(first);
    let (rows, end) = consume(&mut resumed, &full, &[], 4096);
    let expected = oracle_rows(&full);
    assert_eq!(rows.len(), expected.len());
    for (actual, expected) in rows.iter().zip(expected) {
        assert_eq!(actual.byte_end, expected.0.end);
        assert_eq!(actual.actual_cells, expected.1);
    }
    assert_eq!(end, full.len());
    assert_eq!(resumed.buffered_source_capacity(), 0);
}

#[test]
fn invalid_frame_and_zero_cell_line_never_gain_membership_authority() {
    let prefix = "| H |\n| - |\n";
    let full = format!("{prefix}|\n");
    let mut state = seed(prefix, &full);
    assert_eq!(
        state
            .advance(
                SourceFrame {
                    source_revision: 18,
                    byte_start: prefix.len(),
                    bytes: &full.as_bytes()[prefix.len()..],
                    real_eof: true,
                },
                2
            )
            .unwrap_err(),
        FrontierError::InvalidFrame
    );
    assert_eq!(state.scalars.next_byte, prefix.len());
    assert!(state.row.is_none());
    // 仅一个pipe不是“补成一格”的body；它结束当前table，且未消耗补格fuel。
    assert!(
        matches!(advance_until_visible(&mut state,&full,2).unwrap(),TableStep::Ended {byte_offset,..} if byte_offset==prefix.len())
    );
    assert_eq!(state.scalars.missing_cells, 0);
    assert_eq!(oracle_rows(&full).len(), 0);
    // 完整source借用即使只计划读1byte，也不能绕过frame准入预算。
    let full = format!("{prefix}{}", "x".repeat(MAX_FRONTIER_FRAME_BYTES + 1));
    let mut state = seed(prefix, &full);
    assert_eq!(
        state
            .advance(
                SourceFrame {
                    source_revision: 17,
                    byte_start: prefix.len(),
                    bytes: &full.as_bytes()[prefix.len()..],
                    real_eof: true,
                },
                1
            )
            .unwrap_err(),
        FrontierError::InvalidFrame
    );
    assert!(state.row.is_none());
}

#[test]
fn incomplete_frames_and_invalid_protocol_do_not_create_opaque_table_end() {
    let prefix = "| H |\n| - |\n";
    let mut state = seed(prefix, prefix);
    assert!(state.verified_end().is_none());
    let frame = SourceFrame {
        source_revision: 17,
        byte_start: prefix.len(),
        bytes: &[],
        real_eof: false,
    };
    assert!(matches!(
        state.advance(frame, 1).unwrap(),
        TableStep::NeedInput { .. }
    ));
    assert!(state.verified_end().is_none());
    let wrong = SourceFrame {
        source_revision: 18,
        byte_start: prefix.len(),
        bytes: &[],
        real_eof: true,
    };
    assert_eq!(
        state.advance(wrong, 1).unwrap_err(),
        FrontierError::InvalidFrame
    );
    assert!(state.verified_end().is_none());
    let eof = SourceFrame {
        source_revision: 17,
        byte_start: prefix.len(),
        bytes: &[],
        real_eof: true,
    };
    assert!(matches!(
        state.advance(eof, 1).unwrap(),
        TableStep::Ended { .. }
    ));
    assert!(state
        .verified_end()
        .unwrap()
        .last_body_row_ordinal()
        .is_none());
}
