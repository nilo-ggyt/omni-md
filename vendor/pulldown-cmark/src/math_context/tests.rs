use super::*;

#[derive(Default)]
// 原firstpass Vec状态仅保留为纯测oracle；生产只有上方固定容量共享kernel。
struct Original {
    stack: Vec<u8>,
    next: usize,
}

impl Original {
    fn apply(&mut self, token: u8) -> Option<u8> {
        match token {
            b'$' => Some(if self.stack.len() > MAX_NESTING {
                self.next as u8
            } else {
                self.stack.last().copied().unwrap_or_else(|| {
                    self.stack.push(!0);
                    !0
                })
            }),
            b'{' => {
                if self.stack.len() == MAX_NESTING {
                    self.stack.push(self.next as u8);
                    self.next = MAX_NESTING;
                } else if self.stack.len() > MAX_NESTING {
                    self.next += 1;
                } else if !self.stack.is_empty() {
                    self.stack.push(self.next as u8);
                    self.next += 1;
                }
                None
            }
            b'}' => {
                if let &mut [ref mut top] = &mut self.stack[..] {
                    *top = top.wrapping_sub(1);
                } else if self.stack.len() > MAX_NESTING {
                    if self.next <= MAX_NESTING {
                        self.stack.pop();
                    } else {
                        self.next -= 1;
                    }
                } else {
                    self.stack.pop();
                }
                None
            }
            _ => None,
        }
    }
}

#[test]
fn original_math_state_transitions_match_fixed_kernel_including_wrap_and_saturation() {
    // 固定序列覆盖首次math之前brace、标签255回绕、超25层计数及降回真实栈。
    let source = format!(
        "{{{{}}}}$}}{}{}{}",
        "{}".repeat(600),
        "{".repeat(80),
        "}".repeat(90)
    );
    let mut expected = Original::default();
    let mut actual = MathContext::default();
    for byte in source.bytes() {
        let tag = match byte {
            b'$' => Some(actual.dollar()),
            b'{' => {
                actual.open();
                None
            }
            b'}' => {
                actual.close();
                None
            }
            _ => None,
        };
        assert_eq!(tag, expected.apply(byte));
        assert_eq!(&actual.stack[..actual.len], expected.stack.as_slice());
        assert_eq!(actual.next, expected.next);
    }
    // 确定性随机token序列验证所有可达深度切换；不引入外部随机依赖或不可复现seed。
    let mut seed = 0x8b2a_1f93_u64;
    for _ in 0..100_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let token = [b'$', b'{', b'}', b'x'][(seed >> 32) as usize % 4];
        let tag = match token {
            b'$' => Some(actual.dollar()),
            b'{' => {
                actual.open();
                None
            }
            b'}' => {
                actual.close();
                None
            }
            _ => None,
        };
        assert_eq!(tag, expected.apply(token));
        assert_eq!(&actual.stack[..actual.len], expected.stack.as_slice());
        assert_eq!(actual.next, expected.next);
    }
    actual.reset();
    assert_eq!(actual, MathContext::default());
    assert!(!std::mem::needs_drop::<MathContext>());
    assert!(std::mem::size_of::<MathContext>() <= 48);
}

#[cfg(feature = "omni-block-frontier")]
mod segmented {
    use super::*;
    use crate::{block_frontier::SourceFrame, Event, Options, Parser};

    fn scan(
        source: &str,
        range: std::ops::Range<usize>,
        context: MathContext,
        width: usize,
    ) -> MathContext {
        let mut scan = MathContextScan::new(7, source.len(), range, context).unwrap();
        loop {
            let at = scan.next_byte();
            let end = (at + width).min(source.len());
            let step = scan
                .advance(
                    SourceFrame {
                        source_revision: 7,
                        byte_start: at,
                        bytes: &source.as_bytes()[at..end],
                        real_eof: end == source.len(),
                    },
                    MathSourceAuthority {
                        current_revision: 7,
                        cancelled: false,
                    },
                    17,
                )
                .unwrap();
            let work = match step {
                MathContextStep::Complete { context, work } => {
                    assert!(work.byte_reads <= 17 && work.transitions <= 17);
                    return context;
                }
                MathContextStep::Progress { work, .. }
                | MathContextStep::NeedInput { work, .. } => work,
            };
            assert!(work.byte_reads <= 17 && work.transitions <= 17);
        }
    }

    #[test]
    fn actual_firstpass_row_context_matches_all_small_frame_widths_and_markup_skips() {
        // 真实完整firstpass仅用作有限oracle；尤其保留header先Scan再Active两遍与excess cells。
        for source in [
            "| $H$ {} | a |\n| - | - |\n| \\$x{} \\\\$y | `$}x{y$` | extra$} |\n| <u a='$}'>a</u> | &dollar; $a{b}c$ |\n",
            "> | $h$ |\r\n> | - |\r\n> | $}x{y$ |\r\n> | \\{${x}$ |\r\n",
            "- | $h$ |\n  | - |\n  | $}x{y$ |\n  | \\`\\`$$ |\n",
        ] {
            let options = Options::ENABLE_TABLES | Options::ENABLE_MATH;
            let rows = crate::firstpass::capture_math_rows(source, options);
            assert!(rows.len() >= 3);
            let header_end = source[rows[0].0..].find('\n').map_or(source.len(), |n| rows[0].0 + n + 1);
            for width in 1..=19 {
                let mut current = scan(source, rows[0].0..header_end, MathContext::default(), width);
                for &(start, expected) in &rows {
                    // capture与scan都只用真实行内容起点，容器prefix不能污染数学状态。
                    assert_eq!(current, expected, "width={width}, row={start}, source={source:?}");
                    let end = source[start..].find('\n').map_or(source.len(), |n| start + n + 1);
                    current = scan(source, start..end, current, width);
                }
            }
        }
    }

    #[test]
    fn finite_row_seeds_preserve_math_events_across_omitted_rows() {
        // 旧负反例在共享桥中必须恢复原事件，而不是改掉原Parser的u8回绕语义。
        let hidden = format!("$x${}", "{}".repeat(255));
        let source = format!("| H |\n| - |\n| {hidden} |\n| $}}x{{y$ |\n");
        let options = Options::ENABLE_TABLES | Options::ENABLE_MATH;
        let rows = crate::firstpass::capture_math_rows(&source, options);
        let target = source.rfind("| $}x{y$ |").unwrap();
        let projected = "| H |\n| - |\n| $}x{y$ |\n";
        let projected_start = projected.rfind("| $}x{y$ |").unwrap();
        let actual_context = rows.iter().find(|row| row.0 == target).unwrap().1;
        let seeds = [
            TableMathSeed::new(0, rows[0].1),
            TableMathSeed::new(projected_start, actual_context),
        ];
        let expected: Vec<_> = Parser::new_ext(&source, options)
            .into_offset_iter()
            .filter_map(|(event, range)| match event {
                Event::InlineMath(text) if range.start >= target => {
                    Some((text.into_string(), range.start - target..range.end - target))
                }
                _ => None,
            })
            .collect();
        let actual: Vec<_> = Parser::<crate::DefaultBrokenLinkCallback>::new_with_table_math_seeds(
            projected, options, None, &seeds,
        )
        .unwrap()
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::InlineMath(text) if range.start >= projected_start => Some((
                text.into_string(),
                range.start - projected_start..range.end - projected_start,
            )),
            _ => None,
        })
        .collect();
        assert_eq!(actual, expected);
        assert!(!actual.is_empty());
        // 容器内物理行起点不是row content anchor；入口不得接受后静默忽略seed。
        let nested = "> | H |\n> | - |\n> | $x$ |\n";
        assert!(
            Parser::<crate::DefaultBrokenLinkCallback>::new_with_table_math_seeds(
                nested,
                options,
                None,
                &[TableMathSeed::new(0, actual_context)]
            )
            .is_err()
        );
        // 即使是root投影，D行并非inline row入口；unused seed同样不能产生Parser。
        assert!(
            Parser::<crate::DefaultBrokenLinkCallback>::new_with_table_math_seeds(
                projected,
                options,
                None,
                &[TableMathSeed::new(6, actual_context)]
            )
            .is_err()
        );
    }

    #[test]
    fn math_scanner_retains_escape_at_frame_edges_and_rejects_revoked_or_invalid_authority() {
        // 多MiB不含数学的段也只保定长续体；最后escape与UTF8都可跨frame边界。
        let source = format!("{}\\$中${{a}}$", "x".repeat(1024 * 1024));
        let whole = scan(&source, 0..source.len(), MathContext::default(), 65536);
        assert_eq!(
            whole,
            scan(&source, 0..source.len(), MathContext::default(), 113)
        );
        let mut state = MathContextScan::new(7, 2, 0..2, MathContext::default()).unwrap();
        let frame = || SourceFrame {
            source_revision: 7,
            byte_start: 0,
            bytes: b"$x",
            real_eof: true,
        };
        assert_eq!(
            state.advance(
                frame(),
                MathSourceAuthority {
                    current_revision: 7,
                    cancelled: true
                },
                4
            ),
            Err(MathContextError::Cancelled)
        );
        // 首错sticky，不能通过恢复权限或改frame绕过取消。
        assert_eq!(
            state.advance(
                frame(),
                MathSourceAuthority {
                    current_revision: 7,
                    cancelled: false
                },
                4
            ),
            Err(MathContextError::Closed)
        );
        assert!(validate_seeds("中\r\nx", &[TableMathSeed::new(1, whole)]).is_err());
        assert!(validate_seeds("a\r\nb", &[TableMathSeed::new(2, whole)]).is_err());
        assert!(validate_seeds(
            "a\nb",
            &[TableMathSeed::new(2, whole), TableMathSeed::new(2, whole)]
        )
        .is_err());
        assert!(!std::mem::needs_drop::<MathContextScan>());
        assert!(std::mem::size_of::<MathContextScan>() <= 112);
    }
}
