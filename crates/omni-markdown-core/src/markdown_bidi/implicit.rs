// Copyright 2015 The Servo Project Developers.
// Algorithm adapted from unicode-bidi 0.3.18 implicit.rs. SPDX-License-Identifier: MIT OR Apache-2.0
//! Apply W/N/I rules through actual IRS cursors; pending ET/BN/NI runs retain endpoints only, and bracket matches are written at their opening records.

use super::prepare::io::CooperativeStore;
use super::*;
use sequences::{Position, first, next, previous, set_processing};

pub(super) async fn resolve(
    store: &mut CooperativeStore,
    sequence: Sequence,
) -> Result<(), BidiError> {
    // Preserve rule order and the same Store across suspension: weak resolution precedes bracket pairing and neutral resolution.
    weak(store, sequence).await?;
    brackets(store, sequence).await?;
    neutral(store, sequence).await
}

async fn after_scalar(
    store: &mut CooperativeStore,
    position: Position,
) -> Result<Option<Position>, BidiError> {
    // Advance by the original scalar's UTF16 length along the IRS, keeping a low surrogate from becoming an independent lookahead character.
    let mut at = Some(position);
    for _ in 0..Text::read_prepared(store, position.index)
        .await?
        .scalar_len()?
    {
        at = match at {
            Some(value) => next(store, value).await?,
            None => return Err(BidiError::CorruptMetadata),
        };
    }
    Ok(at)
}

async fn write_processing(
    store: &mut CooperativeStore,
    position: Position,
    class: BidiClass,
) -> Result<(), BidiError> {
    let mut text = Text::read_prepared(store, position.index).await?;
    text.processing = class;
    text.write_prepared(store, position.index).await
}

async fn weak(store: &mut CooperativeStore, sequence: Sequence) -> Result<(), BidiError> {
    use BidiClass::*;
    // Keep only the distinct pre-rule classes and unresolved endpoint ranges needed by W1-W6, without a character-length state vector.
    let mut prev_before_w4 = sequence.sos;
    let mut prev_before_w5 = sequence.sos;
    let mut prev_before_w1 = sequence.sos;
    let mut last_strong_al = false;
    let mut et_start = None;
    let mut et_end = None;
    let mut bn_start = None;
    let mut at = Some(first(store, sequence).await?);
    while let Some(position) = at {
        let following = next(store, position).await?;
        let mut text = Text::read_prepared(store, position.index).await?;
        if text.processing == BN {
            bn_start.get_or_insert(position);
            at = following;
            continue; // BN does not advance weak-class context; backfill it only when actual subsequent resolution incorporates it into an ET run.
        }
        let mut before_w2 = text.processing;
        if text.processing == NSM {
            text.processing = match prev_before_w1 {
                RLI | LRI | FSI | PDI => ON,
                class => class,
            };
            before_w2 = text.processing;
        }
        prev_before_w1 = text.processing;
        match text.processing {
            EN if last_strong_al => text.processing = AN,
            AL => text.processing = R,
            _ => {} // Preserve classes outside W2/W3 rather than prematurely merging them by direction.
        }
        match before_w2 {
            L | R => last_strong_al = false,
            AL => last_strong_al = true,
            _ => {}
        }
        let before_w456 = text.processing;
        match text.processing {
            EN => {
                if et_start.is_some() {
                    set_processing(store, et_start, et_end, EN).await?;
                }
                et_start = None;
                et_end = None;
            }
            ES | CS => {
                if text.continuation() {
                    // Reuse the scalar head's resolution for its low-surrogate record instead of running numeric-separator lookahead twice.
                    text.processing = Text::read_prepared(store, position.index - 1)
                        .await?
                        .processing;
                } else {
                    // Inspect the actual next nonremoved class, with eos only at the real IRS end; each read remains cooperatively budgeted.
                    let mut look = after_scalar(store, position).await?;
                    let mut next_class = sequence.eos;
                    while let Some(candidate) = look {
                        let class = Text::read_prepared(store, candidate.index)
                            .await?
                            .processing;
                        if !removed(class) {
                            next_class = class;
                            break;
                        }
                        look = next(store, candidate).await?;
                    }
                    if next_class == EN && last_strong_al {
                        next_class = AN;
                    }
                    text.processing = match (prev_before_w4, text.processing, next_class) {
                        (EN, ES | CS, EN) => EN,
                        (AN, CS, AN) => AN,
                        _ => ON,
                    };
                    if text.processing == ON {
                        // Apply the same resolved neutral class to adjacent removed BN records by walking the original chain, not a source envelope.
                        let mut look = previous(store, position).await?;
                        while let Some(candidate) = look {
                            if Text::read_prepared(store, candidate.index)
                                .await?
                                .processing
                                != BN
                            {
                                break;
                            }
                            write_processing(store, candidate, ON).await?;
                            look = previous(store, candidate).await?;
                        }
                        let mut look = after_scalar(store, position).await?;
                        while let Some(candidate) = look {
                            if Text::read_prepared(store, candidate.index)
                                .await?
                                .processing
                                != BN
                            {
                                break;
                            }
                            write_processing(store, candidate, ON).await?;
                            look = next(store, candidate).await?;
                        }
                    }
                }
            }
            ET => {
                if prev_before_w5 == EN {
                    text.processing = EN;
                } else {
                    if et_start.is_none() {
                        et_start = bn_start.or(Some(position));
                    }
                    et_end = following; // Trailing BN has not yet joined ET; do not include unrelated pending characters in the backfill interval.
                }
            }
            _ => {}
        }
        bn_start = None;
        prev_before_w5 = text.processing;
        if prev_before_w5 != ET {
            if et_start.is_some() {
                set_processing(store, et_start, et_end, ON).await?;
            }
            et_start = None;
            et_end = None;
        }
        prev_before_w4 = before_w456;
        text.write_prepared(store, position.index).await?;
        at = following;
    }
    if et_start.is_some() {
        set_processing(store, et_start, et_end, ON).await?;
    }
    // W7 consumes actual completed W1-W6 classes, without rereading source characters or retaining another processing table.
    let mut last_l = sequence.sos == L;
    let mut at = Some(first(store, sequence).await?);
    while let Some(position) = at {
        let mut text = Text::read_prepared(store, position.index).await?;
        match text.processing {
            EN if last_l => {
                text.processing = L;
                text.write_prepared(store, position.index).await?;
            }
            L => last_l = true,
            R | AL => last_l = false,
            _ => {}
        }
        at = next(store, position).await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Opening {
    identity: u32,
    position: Position,
}

async fn brackets(store: &mut CooperativeStore, sequence: Sequence) -> Result<(), BidiError> {
    use BidiClass::*;
    let dummy = Opening {
        identity: 0,
        position: Position { run: 0, index: 0 },
    };
    let mut stack = [dummy; 63];
    let mut depth = 0;
    let mut at = Some(first(store, sequence).await?);
    // BD16's 63 entries are the Unicode rule, not an arbitrary implementation cap; overflow ends new pair identification for this IRS.
    while let Some(position) = at {
        let text = Text::read_prepared(store, position.index).await?;
        if !text.continuation() && text.processing == ON && text.bracket != 0 {
            if text.flags & 4 != 0 {
                if depth == stack.len() {
                    break;
                }
                stack[depth] = Opening {
                    identity: text.bracket,
                    position,
                };
                depth += 1;
            } else if text.flags & 8 != 0
                && let Some(index) = (0..depth)
                    .rev()
                    .find(|&index| stack[index].identity == text.bracket)
            {
                // Persist the matching closer/run coordinates at the actual opening record before discarding nested unmatched openings.
                let opening = stack[index].position;
                let mut record = Text::read_prepared(store, opening.index).await?;
                record.pair_end = position.index;
                record.pair_start_run = opening.run;
                record.pair_end_run = position.run;
                record.write_prepared(store, opening.index).await?;
                depth = index;
            }
        }
        at = next(store, position).await?;
    }
    let beginning = first(store, sequence).await?;
    let e = direction(Text::read_prepared(store, beginning.index).await?.level);
    let not_e = if e == L { R } else { L };
    let mut at = Some(first(store, sequence).await?);
    // Visit stored matches in opening-source order, matching the upstream ordering without an O(N) pair array or sorting buffer.
    while let Some(position) = at {
        let text = Text::read_prepared(store, position.index).await?;
        if text.pair_end != NONE {
            let closing = Position {
                run: text.pair_end_run,
                index: text.pair_end,
            };
            let mut look = after_scalar(store, position).await?;
            let mut found_e = false;
            let mut found_not_e = false;
            // N0 first searches actual content inside this pair for the embedding direction, treating numbers as prescribed strong context.
            while let Some(candidate) = look {
                if candidate.index >= closing.index {
                    break;
                }
                let class = Text::read_prepared(store, candidate.index)
                    .await?
                    .processing;
                if class == e {
                    found_e = true;
                    break;
                }
                if class == not_e {
                    found_not_e = true;
                } else if matches!(class, EN | AN) {
                    if e == L {
                        found_not_e = true;
                    } else {
                        found_e = true;
                        break;
                    }
                }
                look = next(store, candidate).await?;
            }
            let replacement = if found_e {
                Some(e)
            } else if found_not_e {
                // Only when opposite-direction content exists is preceding actual strong context needed; sos remains the genuine IRS fallback.
                let mut before = previous(store, position).await?;
                let mut strong = sequence.sos;
                while let Some(candidate) = before {
                    let class = Text::read_prepared(store, candidate.index)
                        .await?
                        .processing;
                    if matches!(class, L | R | EN | AN) {
                        strong = if matches!(class, EN | AN) { R } else { class };
                        break;
                    }
                    before = previous(store, candidate).await?;
                }
                Some(strong)
            } else {
                None
            };
            if let Some(class) = replacement {
                // Rewrite both complete bracket scalars and their required adjacent NSM/BN records through the same budgeted Store.
                let after_opening = after_scalar(store, position).await?;
                let after_closing = after_scalar(store, closing).await?;
                set_processing(store, Some(position), after_opening, class).await?;
                set_processing(store, Some(closing), after_closing, class).await?;
                let mut before = previous(store, position).await?;
                while let Some(candidate) = before {
                    if Text::read_prepared(store, candidate.index)
                        .await?
                        .processing
                        != BN
                    {
                        break;
                    }
                    write_processing(store, candidate, class).await?;
                    before = previous(store, candidate).await?;
                }
                for bracket in [position, closing] {
                    let mut following = after_scalar(store, bracket).await?;
                    while let Some(candidate) = following {
                        let next_text = Text::read_prepared(store, candidate.index).await?;
                        if next_text.original != NSM && next_text.processing != BN {
                            break;
                        }
                        write_processing(store, candidate, class).await?;
                        following = next(store, candidate).await?;
                    }
                }
            }
        }
        at = next(store, position).await?;
    }
    Ok(())
}

fn ni(class: BidiClass) -> bool {
    use BidiClass::*;
    matches!(class, B | S | WS | ON | FSI | LRI | RLI | PDI)
}

async fn neutral(store: &mut CooperativeStore, sequence: Sequence) -> Result<(), BidiError> {
    use BidiClass::*;
    let beginning = first(store, sequence).await?;
    let e = direction(Text::read_prepared(store, beginning.index).await?.level);
    let mut before = sequence.sos;
    let mut at = Some(beginning);
    while let Some(position) = at {
        let class = Text::read_prepared(store, position.index).await?.processing;
        if ni(class) || class == BN {
            // Locate the actual end of one neutral/removed run, then resolve its endpoints under N1/N2 without buffering its characters.
            let mut following = next(store, position).await?;
            let mut after = sequence.eos;
            while let Some(candidate) = following {
                let class = Text::read_prepared(store, candidate.index)
                    .await?
                    .processing;
                if !ni(class) && class != BN {
                    after = class;
                    break;
                }
                following = next(store, candidate).await?;
            }
            let class = match (before, after) {
                (L, L) => L,
                (R | AN | EN, R | AN | EN) => R,
                _ => e,
            };
            set_processing(store, Some(position), following, class).await?;
            if let Some(candidate) = following {
                before = Text::read_prepared(store, candidate.index)
                    .await?
                    .processing;
                at = next(store, candidate).await?;
            } else {
                at = None;
            }
        } else {
            before = class;
            at = next(store, position).await?;
        }
    }
    Ok(())
}

pub(super) async fn finish_levels(
    store: &mut CooperativeStore,
    paragraph: Paragraph,
) -> Result<(), BidiError> {
    use BidiClass::*;
    // I1/I2 adjust the original explicit level using the already resolved class; invalid overflow is not clamped into a plausible level.
    for index in paragraph.start..paragraph.end {
        let mut text = Text::read_prepared(store, index).await?;
        let increase = match (text.level & 1 != 0, text.processing) {
            (false, AN | EN) => 2,
            (false, R) | (true, L | EN | AN) => 1,
            _ => 0,
        };
        text.level = text
            .level
            .checked_add(increase)
            .filter(|&level| level <= 126)
            .ok_or(BidiError::CorruptMetadata)?;
        text.write_prepared(store, index).await?;
    }
    let mut previous = paragraph.base;
    // Restore levels for X9-removed coordinates from their preceding actual level, preserving a complete UTF16 result for later L1/L2.
    for index in paragraph.start..paragraph.end {
        let mut text = Text::read_prepared(store, index).await?;
        if removed(text.original) {
            text.level = previous;
            text.write_prepared(store, index).await?;
        }
        previous = text.level;
    }
    Ok(())
}
