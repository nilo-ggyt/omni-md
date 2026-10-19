// Copyright 2015 The Servo Project Developers.
// Algorithm adapted from unicode-bidi 0.3.18 explicit.rs. SPDX-License-Identifier: MIT OR Apache-2.0
//! X1–X8/X9：合法显式level栈固定126项，overflow计数仍保留全部输入语义。

use super::prepare::io::CooperativeStore;
use super::*;
use unicode_bidi::Level;

#[derive(Clone, Copy, PartialEq)]
enum Override {
    Neutral,
    Rtl,
    Ltr,
    Isolate,
}
#[derive(Clone, Copy)]
struct Status {
    level: u8,
    override_status: Override,
}

pub(super) async fn compute(
    store: &mut CooperativeStore,
    paragraph: Paragraph,
) -> Result<(u32, u32), BidiError> {
    use BidiClass::*;
    let initial = Status {
        level: paragraph.base,
        override_status: Override::Neutral,
    };
    let mut stack = [initial; 126];
    let mut depth = 1;
    let mut overflow_isolate = 0_u32;
    let mut overflow_embedding = 0_u32;
    let mut valid_isolate = 0_u32;
    let mut at = paragraph.start;
    let mut run_start = at;
    let mut run_level = paragraph.base;
    let mut runs = 0;
    while at < paragraph.end {
        let mut text = Text::read_prepared(store, at).await?;
        let length = text.scalar_len()?;
        text.level = paragraph.base;
        let last = stack[depth - 1];
        match text.original {
            RLE | LRE | RLO | LRO | RLI | LRI | FSI => {
                text.level = last.level;
                let isolate = matches!(text.original, RLI | LRI | FSI);
                if isolate {
                    text.processing = apply_override(last.override_status, text.processing);
                }
                let current = Level::new(last.level).map_err(|_| BidiError::CorruptMetadata)?;
                let next = if matches!(text.original, R | AL | RLE | RLO | RLI) {
                    current.new_explicit_next_rtl()
                } else {
                    current.new_explicit_next_ltr()
                };
                if let Ok(next) = next
                    && overflow_isolate == 0
                    && overflow_embedding == 0
                {
                    if depth >= stack.len() {
                        return Err(BidiError::CorruptMetadata);
                    }
                    stack[depth] = Status {
                        level: next.number(),
                        override_status: match text.original {
                            RLO => Override::Rtl,
                            LRO => Override::Ltr,
                            RLI | LRI | FSI => Override::Isolate,
                            _ => Override::Neutral,
                        },
                    };
                    depth += 1;
                    if isolate {
                        valid_isolate += 1;
                    } else {
                        text.level = next.number();
                    }
                } else if isolate {
                    overflow_isolate = overflow_isolate
                        .checked_add(1)
                        .ok_or(BidiError::CoordinateOverflow)?;
                } else if overflow_isolate == 0 {
                    overflow_embedding = overflow_embedding
                        .checked_add(1)
                        .ok_or(BidiError::CoordinateOverflow)?;
                }
                if !isolate {
                    text.processing = BN;
                } // X9只移除格式作用；原UTF16位置仍保留。
            }
            PDI => {
                if overflow_isolate > 0 {
                    overflow_isolate -= 1;
                } else if valid_isolate > 0 {
                    overflow_embedding = 0;
                    loop {
                        if depth <= 1 {
                            return Err(BidiError::CorruptMetadata);
                        }
                        depth -= 1;
                        if stack[depth].override_status == Override::Isolate {
                            break;
                        }
                    }
                    valid_isolate -= 1;
                }
                text.level = stack[depth - 1].level;
                text.processing = apply_override(stack[depth - 1].override_status, text.processing);
            }
            PDF => {
                if overflow_isolate > 0 { /* isolate溢出内部的PDF不得弹合法栈。 */
                } else if overflow_embedding > 0 {
                    overflow_embedding -= 1;
                } else if last.override_status != Override::Isolate && depth >= 2 {
                    depth -= 1;
                }
                text.level = stack[depth - 1].level;
                text.processing = BN;
            }
            B => {} // B仍保持paragraph base，随后独立段会重建整个显式栈。
            _ => {
                text.level = last.level;
                if text.original != BN {
                    text.processing = apply_override(last.override_status, text.processing);
                }
            }
        }
        text.write_prepared(store, at).await?;
        // UTF16低项共享head的处理类别与level，不能被误当成另一个独立字符。
        if length == 2 {
            let mut continuation = Text::read_prepared(store, at + 1).await?;
            continuation.level = text.level;
            continuation.processing = text.processing;
            continuation.write_prepared(store, at + 1).await?;
        }
        if at == paragraph.start {
            run_level = text.level;
        } else if !removed(text.original) && text.level != run_level {
            Run {
                start: run_start,
                end: at,
                previous: NONE,
                next: NONE,
            }
            .write_prepared(store, runs)
            .await?;
            runs += 1;
            run_start = at;
            run_level = text.level;
        }
        at += length;
    }
    if run_start < paragraph.end {
        Run {
            start: run_start,
            end: paragraph.end,
            previous: NONE,
            next: NONE,
        }
        .write_prepared(store, runs)
        .await?;
        runs += 1;
    }
    Ok((0, runs))
}

fn apply_override(status: Override, class: BidiClass) -> BidiClass {
    match status {
        Override::Rtl => BidiClass::R,
        Override::Ltr => BidiClass::L,
        _ => class,
    }
}
