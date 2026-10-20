//! 单元格显示的唯一标量转换；只省略原语义标记与既有转义，不持源字符串或积累输出。

use std::ops::Range;

use crate::MarkdownPresentationStyles;

const INPUT_SCALARS_PER_STEP: usize = 256;

/// 输入由原来源与同次语义查询共同提供；Paused 不能被当成 EOF 决定末尾 slash 的显示。
pub enum MarkdownTableCellDisplayInput {
    Scalar {
        scalar: char,
        styles: MarkdownPresentationStyles,
    },
    Paused,
    End,
}

/// 一个真实输出标量同时保留三种坐标；来源与显示都是完整 UTF-16 标量，emoji 不压成一个单元。
pub struct MarkdownTableCellDisplayScalar {
    pub scalar: char,
    pub display_bytes: Range<usize>,
    pub display_utf16: Range<usize>,
    pub raw_bytes: Range<usize>,
    pub raw_utf16: Range<usize>,
    pub canonical_utf16: Range<usize>,
    pub styles: MarkdownPresentationStyles,
}

/// Progress 只表示量子已用完；唯一的 Complete 必须来自原输入明确 End 与所有待发标量耗尽。
pub enum MarkdownTableCellDisplayAnswer {
    Scalar(MarkdownTableCellDisplayScalar),
    Progress,
    NeedInput,
    Complete,
}

pub struct MarkdownTableCellDisplayStep {
    pub input_bytes: usize,
    pub input_scalars: usize,
    pub answer: MarkdownTableCellDisplayAnswer,
}

struct RawScalar {
    scalar: char,
    bytes: Range<usize>,
    utf16: Range<usize>,
    styles: MarkdownPresentationStyles,
}

struct SlashRun {
    byte_start: usize,
    utf16_start: usize,
    count: usize,
    styles: MarkdownPresentationStyles,
}

struct SlashOutput {
    run: SlashRun,
    next: usize,
    count: usize,
    paired: bool,
}

/// 固定状态只保一段 slash 计数与一个已读后继；长 slash-run 不申请相同长度的缓存或循环输出。
pub struct MarkdownTableCellDisplayCursor {
    canonical_start: usize,
    raw_bytes: usize,
    raw_utf16: usize,
    display_bytes: usize,
    display_utf16: usize,
    slashes: Option<SlashRun>,
    output: Option<SlashOutput>,
    held: Option<RawScalar>,
    ended: bool,
}

impl MarkdownTableCellDisplayCursor {
    /// 起点由原格票给出；本游标只转换纯值，不签发源、预览或编辑权限。
    #[must_use]
    pub fn new(canonical_start: usize) -> Self {
        Self {
            canonical_start,
            raw_bytes: 0,
            raw_utf16: 0,
            display_bytes: 0,
            display_utf16: 0,
            slashes: None,
            output: None,
            held: None,
            ended: false,
        }
    }

    /// 每次最多消费 256 个原标量并交付一个可见标量；读取错误原样返回，不能用默认样式续读。
    pub fn step<E>(
        &mut self,
        read: impl FnMut(usize) -> Result<MarkdownTableCellDisplayInput, E>,
    ) -> Result<MarkdownTableCellDisplayStep, E> {
        self.step_with_input_limit(INPUT_SCALARS_PER_STEP, read)
    }

    /// 原准备作业合并多次输出时只借剩余输入额度；长隐藏区不能超出该次已扣减的量子。
    pub(crate) fn step_with_input_limit<E>(
        &mut self,
        input_limit: usize,
        mut read: impl FnMut(usize) -> Result<MarkdownTableCellDisplayInput, E>,
    ) -> Result<MarkdownTableCellDisplayStep, E> {
        let mut bytes = 0;
        let mut scalars = 0;
        let answer = loop {
            if let Some(mut output) = self.output.take() {
                let index = if output.paired {
                    output.next * 2 + 1 // 普通成对 slash 保留第二个原 slash，而非制造新的显示字符。
                } else {
                    output.next // code 与非转义目标逐个保留原位置；隐藏的末 slash 不进入输出。
                };
                let raw = RawScalar {
                    scalar: '\\',
                    bytes: output.run.byte_start + index..output.run.byte_start + index + 1,
                    utf16: output.run.utf16_start + index..output.run.utf16_start + index + 1,
                    styles: output.run.styles,
                };
                output.next += 1;
                if output.next != output.count {
                    self.output = Some(output);
                }
                break MarkdownTableCellDisplayAnswer::Scalar(self.emit(raw));
            }
            if let Some(raw) = self.held.take() {
                if !hidden(raw.styles) {
                    break MarkdownTableCellDisplayAnswer::Scalar(self.emit(raw));
                }
                continue; // 原后继的 hidden 语义仍有效，不因它用于结束 slash-run 而恢复显示。
            }
            if self.ended {
                if self.slashes.is_some() {
                    self.finish_slashes(None);
                    continue;
                }
                break MarkdownTableCellDisplayAnswer::Complete;
            }
            if scalars == input_limit.min(INPUT_SCALARS_PER_STEP) {
                break MarkdownTableCellDisplayAnswer::Progress;
            }
            let canonical = self
                .canonical_start
                .checked_add(self.raw_utf16)
                .expect("原格端点必须位于可表示的文档 UTF16 域");
            let (scalar, styles) = match read(canonical)? {
                MarkdownTableCellDisplayInput::Paused => {
                    break MarkdownTableCellDisplayAnswer::NeedInput;
                }
                MarkdownTableCellDisplayInput::End => {
                    self.ended = true;
                    continue;
                }
                MarkdownTableCellDisplayInput::Scalar { scalar, styles } => (scalar, styles),
            };
            let raw = RawScalar {
                scalar,
                bytes: self.raw_bytes..self.raw_bytes + scalar.len_utf8(),
                utf16: self.raw_utf16..self.raw_utf16 + scalar.len_utf16(),
                styles: styles.without(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER),
            };
            self.raw_bytes = raw.bytes.end;
            self.raw_utf16 = raw.utf16.end;
            bytes += scalar.len_utf8();
            scalars += 1;
            if let Some(run) = self.slashes.as_mut() {
                if scalar == '\\' {
                    run.count += 1; // 原转换器的连续 slash 词法先于中间样式；保持原首 slash 的样式。
                    continue;
                }
                self.finish_slashes(Some(&raw));
                self.held = Some(raw);
                continue;
            }
            if hidden(raw.styles) {
                continue; // 隐藏语法只前进 raw/canonical，不占任何显示字节或 GCB/UBA 输入。
            }
            if scalar == '\\' {
                self.slashes = Some(SlashRun {
                    byte_start: raw.bytes.start,
                    utf16_start: raw.utf16.start,
                    count: 1,
                    styles: raw.styles,
                });
                continue;
            }
            break MarkdownTableCellDisplayAnswer::Scalar(self.emit(raw));
        };
        Ok(MarkdownTableCellDisplayStep {
            input_bytes: bytes,
            input_scalars: scalars,
            answer,
        })
    }

    /// 只在真实后继或 EOF 已知时决定 slash 数量；Paused 不进入这个业务决定。
    fn finish_slashes(&mut self, next: Option<&RawScalar>) {
        let run = self.slashes.take().expect("只有原连续 slash 段可完成");
        let code = next.is_some_and(|next| {
            next.scalar == '|'
                && run.styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                && next
                    .styles
                    .contains(MarkdownPresentationStyles::INLINE_CODE)
        });
        let paired = !run.styles.contains(MarkdownPresentationStyles::INLINE_CODE)
            && next.is_some_and(|next| next.scalar.is_ascii_punctuation());
        let count = if code {
            run.count - 1 // GFM code pipe 只消耗最后一个表级转义 slash。
        } else if paired {
            run.count / 2
        } else {
            run.count // 非标点或真实 EOF 不消耗普通字面 slash。
        };
        if count != 0 {
            self.output = Some(SlashOutput {
                run,
                next: 0,
                count,
                paired,
            });
        }
    }

    /// 一个输出和原 scalar 一一对应；省略造成的 gap 保留在来源坐标中，不能被显示偏移抹平。
    fn emit(&mut self, raw: RawScalar) -> MarkdownTableCellDisplayScalar {
        let display_bytes = self.display_bytes..self.display_bytes + raw.scalar.len_utf8();
        let display_utf16 = self.display_utf16..self.display_utf16 + raw.scalar.len_utf16();
        self.display_bytes = display_bytes.end;
        self.display_utf16 = display_utf16.end;
        MarkdownTableCellDisplayScalar {
            scalar: raw.scalar,
            display_bytes,
            display_utf16,
            raw_bytes: raw.bytes,
            canonical_utf16: self.canonical_start + raw.utf16.start
                ..self.canonical_start + raw.utf16.end,
            raw_utf16: raw.utf16,
            styles: raw.styles,
        }
    }
}

/// 原表格合同中两个 marker 位都删除；没有任何字体大小或 paint 透明度替代这个转换。
fn hidden(styles: MarkdownPresentationStyles) -> bool {
    styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        || styles.contains(MarkdownPresentationStyles::MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 小样本以真实标量、输出和来源逐项比较；完整输出只保留在这个固定长度夹具中。
    fn converted(
        text: &str,
        canonical: usize,
        styles: impl Fn(usize) -> MarkdownPresentationStyles,
    ) -> Vec<MarkdownTableCellDisplayScalar> {
        let mut input = text.chars();
        let mut cursor = MarkdownTableCellDisplayCursor::new(canonical);
        let mut output = Vec::new();
        loop {
            let step = cursor
                .step(|at| {
                    Ok::<_, std::convert::Infallible>(match input.next() {
                        Some(scalar) => MarkdownTableCellDisplayInput::Scalar {
                            scalar,
                            styles: styles(at),
                        },
                        None => MarkdownTableCellDisplayInput::End,
                    })
                })
                .unwrap();
            match step.answer {
                MarkdownTableCellDisplayAnswer::Scalar(scalar) => output.push(scalar),
                MarkdownTableCellDisplayAnswer::Complete => return output,
                MarkdownTableCellDisplayAnswer::Progress => {}
                MarkdownTableCellDisplayAnswer::NeedInput => panic!("固定夹具没有暂停输入"),
            }
        }
    }

    #[test]
    /// 普通转义与 code pipe 保留不同 slash 来源；emoji 的两单元映射不能被压缩。
    fn commonmark_code_and_unicode_keep_the_original_scalar_provenance() {
        let empty = MarkdownPresentationStyles::default();
        for (input, expected) in [
            (r"a\|b", "a|b"),
            (r"a\\|b", r"a\|b"),
            (r"a\\\|b", r"a\|b"),
            (r"a\中😀", r"a\中😀"),
            (r"end\\", r"end\\"),
        ] {
            let output = converted(input, 30, |_| empty);
            let text: String = output.iter().map(|value| value.scalar).collect();
            assert_eq!(text, expected);
            for scalar in &output {
                assert_eq!(scalar.raw_utf16.len(), scalar.display_utf16.len());
                assert_eq!(scalar.canonical_utf16.start, 30 + scalar.raw_utf16.start);
                assert_eq!(scalar.raw_bytes.len(), scalar.display_bytes.len());
            }
        }
        let code = MarkdownPresentationStyles::INLINE_CODE;
        let normal = converted(r"\\\|", 10, |_| empty);
        let inline = converted(r"\\\|", 10, |_| code);
        assert_eq!(
            normal.iter().map(|value| value.scalar).collect::<String>(),
            r"\|"
        );
        assert_eq!(
            inline.iter().map(|value| value.scalar).collect::<String>(),
            r"\\|"
        );
        assert_eq!(normal[0].canonical_utf16, 11..12);
        assert_eq!(inline[0].canonical_utf16, 10..11);
        assert_eq!(inline[1].canonical_utf16, 11..12);
        assert_eq!(normal[1].canonical_utf16, 13..14);
    }

    #[test]
    /// 省略标记不留下字号宽度；结合标记与 RTL 真正相邻的显示字节由后继 Unicode 算法消费。
    fn hidden_markers_produce_no_display_positions_and_keep_source_gaps() {
        let output = converted("e**\u{301}**אב😀", 7, |at| {
            if matches!(at - 7, 1 | 2 | 4 | 5) {
                MarkdownPresentationStyles::MARKER
            } else {
                MarkdownPresentationStyles::default()
            }
        });
        assert_eq!(
            output.iter().map(|value| value.scalar).collect::<String>(),
            "e\u{301}אב😀"
        );
        assert_eq!(output[1].display_utf16, 1..2);
        assert_eq!(output[1].canonical_utf16, 10..11);
        assert_eq!(output.last().unwrap().display_utf16, 4..6);
        assert_eq!(output.last().unwrap().canonical_utf16, 15..17);
    }

    #[test]
    /// Paused 表示后继尚未知；不能在窗口边界提前发出最后 slash，再在下一窗口追加 pipe。
    fn paused_input_does_not_finalize_a_slash_run() {
        let mut cursor = MarkdownTableCellDisplayCursor::new(0);
        let mut sent = false;
        let step = cursor
            .step(|_| {
                Ok::<_, std::convert::Infallible>(if !sent {
                    sent = true;
                    MarkdownTableCellDisplayInput::Scalar {
                        scalar: '\\',
                        styles: MarkdownPresentationStyles::INLINE_CODE,
                    }
                } else {
                    MarkdownTableCellDisplayInput::Paused
                })
            })
            .unwrap();
        assert!(matches!(
            step.answer,
            MarkdownTableCellDisplayAnswer::NeedInput
        ));
        let step = cursor
            .step(|_| {
                Ok::<_, std::convert::Infallible>(MarkdownTableCellDisplayInput::Scalar {
                    scalar: '|',
                    styles: MarkdownPresentationStyles::INLINE_CODE,
                })
            })
            .unwrap();
        let MarkdownTableCellDisplayAnswer::Scalar(scalar) = step.answer else {
            panic!("原 code 转义完成后必须仅交付真实 pipe");
        };
        assert_eq!(scalar.scalar, '|');
        assert_eq!(scalar.raw_utf16, 1..2);
        assert_eq!(scalar.display_utf16, 0..1);
    }

    #[test]
    /// 六万余个连续 slash 只保固定计数；每步读取和输出受真实量子约束，不构造同长缓冲。
    fn long_slash_runs_keep_bounded_turns_and_exact_original_positions() {
        for code in [false, true] {
            let styles = if code {
                MarkdownPresentationStyles::INLINE_CODE
            } else {
                MarkdownPresentationStyles::default()
            };
            let mut remaining = 65537;
            let mut pipe_sent = false;
            let mut cursor = MarkdownTableCellDisplayCursor::new(100);
            let mut slashes = 0;
            let mut pipes = 0;
            loop {
                let step = cursor
                    .step(|_| {
                        Ok::<_, std::convert::Infallible>(if remaining != 0 {
                            remaining -= 1;
                            MarkdownTableCellDisplayInput::Scalar {
                                scalar: '\\',
                                styles,
                            }
                        } else if !pipe_sent {
                            pipe_sent = true;
                            MarkdownTableCellDisplayInput::Scalar {
                                scalar: '|',
                                styles,
                            }
                        } else {
                            MarkdownTableCellDisplayInput::End
                        })
                    })
                    .unwrap();
                assert!(step.input_scalars <= INPUT_SCALARS_PER_STEP);
                assert!(step.input_bytes <= INPUT_SCALARS_PER_STEP);
                match step.answer {
                    MarkdownTableCellDisplayAnswer::Scalar(scalar) if scalar.scalar == '\\' => {
                        let offset = if code { slashes } else { slashes * 2 + 1 };
                        assert_eq!(scalar.canonical_utf16.start, 100 + offset);
                        slashes += 1;
                    }
                    MarkdownTableCellDisplayAnswer::Scalar(scalar) => {
                        assert_eq!(scalar.scalar, '|');
                        assert_eq!(scalar.canonical_utf16.start, 100 + 65537);
                        pipes += 1;
                    }
                    MarkdownTableCellDisplayAnswer::Complete => break,
                    MarkdownTableCellDisplayAnswer::Progress => {}
                    MarkdownTableCellDisplayAnswer::NeedInput => panic!("同步固定生成器没有暂停"),
                }
            }
            assert_eq!(slashes, if code { 65536 } else { 32768 });
            assert_eq!(pipes, 1);
        }
    }
}
