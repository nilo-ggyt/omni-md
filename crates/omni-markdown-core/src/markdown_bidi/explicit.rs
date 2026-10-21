// Copyright 2015 The Servo Project Developers.
// Algorithm adapted from unicode-bidi 0.3.18 explicit.rs. SPDX-License-Identifier: MIT OR Apache-2.0
//! X1-X8/X9 use a fixed 126-entry valid embedding stack; overflow counters still preserve the semantics of all input controls.

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
    // Start each actual paragraph with its own base and bounded legal stack, without inheriting controls from a preceding paragraph.
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
                // Preserve the initiator's current level and override behavior before deciding whether its new embedding/isolate is legal.
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
                    // Only valid levels enter the fixed stack; overflow controls are counted separately, never represented by unbounded heap growth.
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
                } // X9 removes formatting influence only; the original UTF16 coordinate remains represented.
            }
            PDI => {
                // An overflowing isolate consumes only its overflow count. A matching valid isolate also discards nested embeddings back to its original stack entry.
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
                if overflow_isolate > 0 { /* PDF inside an overflowing isolate must not pop the valid stack. */
                } else if overflow_embedding > 0 {
                    overflow_embedding -= 1;
                } else if last.override_status != Override::Isolate && depth >= 2 {
                    depth -= 1;
                }
                text.level = stack[depth - 1].level;
                text.processing = BN;
            }
            B => {} // B retains this paragraph's base; the next actual paragraph rebuilds the entire explicit stack.
            _ => {
                text.level = last.level;
                if text.original != BN {
                    text.processing = apply_override(last.override_status, text.processing);
                }
            }
        }
        text.write_prepared(store, at).await?;
        // A low-surrogate record shares the head's processing class/level and must not be treated as another independent character.
        if length == 2 {
            let mut continuation = Text::read_prepared(store, at + 1).await?;
            continuation.level = text.level;
            continuation.processing = text.processing;
            continuation.write_prepared(store, at + 1).await?;
        }
        if at == paragraph.start {
            run_level = text.level;
        } else if !removed(text.original) && text.level != run_level {
            // Publish completed logical level runs in the same Store; X9-removed controls do not create artificial run changes.
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
        // Retain the last actual run too; this logical run inventory is not yet a visually reordered soft line.
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
    // Only directional overrides replace a character's processing class; neutral/isolate stack entries retain the actual class.
    match status {
        Override::Rtl => BidiClass::R,
        Override::Ltr => BidiClass::L,
        _ => class,
    }
}
