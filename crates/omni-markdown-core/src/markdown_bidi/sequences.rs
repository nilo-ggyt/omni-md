// Copyright 2015 The Servo Project Developers.
// Algorithm adapted from unicode-bidi 0.3.18 prepare.rs. SPDX-License-Identifier: MIT OR Apache-2.0
//! BD13/X10 represent isolating run sequences as disk-backed bidirectional run chains, without paragraph-length Vecs for suspended or completed sequences.

use super::prepare::io::CooperativeStore;
use super::*;
use store::DiskStack;

#[derive(Clone, Copy)]
pub(super) struct Position {
    pub(super) run: u32,
    pub(super) index: u32,
}

pub(super) async fn next(
    store: &mut CooperativeStore,
    position: Position,
) -> Result<Option<Position>, BidiError> {
    // Advance one original UTF16 record within its run, then follow the actual linked successor; source envelopes do not describe an IRS path.
    let run = Run::read_prepared(store, position.run).await?;
    if position.index + 1 < run.end {
        return Ok(Some(Position {
            run: position.run,
            index: position.index + 1,
        }));
    }
    if run.next == NONE {
        return Ok(None);
    }
    let following = Run::read_prepared(store, run.next).await?;
    Ok(Some(Position {
        run: run.next,
        index: following.start,
    }))
}

pub(super) async fn previous(
    store: &mut CooperativeStore,
    position: Position,
) -> Result<Option<Position>, BidiError> {
    // Traverse the same actual run chain backward rather than treating intervening isolated source text as part of this sequence.
    let run = Run::read_prepared(store, position.run).await?;
    if position.index > run.start {
        return Ok(Some(Position {
            run: position.run,
            index: position.index - 1,
        }));
    }
    if run.previous == NONE {
        return Ok(None);
    }
    let preceding = Run::read_prepared(store, run.previous).await?;
    Ok(Some(Position {
        run: run.previous,
        index: preceding.end - 1,
    }))
}

pub(super) async fn first(
    store: &mut CooperativeStore,
    sequence: Sequence,
) -> Result<Position, BidiError> {
    Ok(Position {
        run: sequence.first,
        index: Run::read_prepared(store, sequence.first).await?.start,
    })
}

pub(super) async fn last(
    store: &mut CooperativeStore,
    sequence: Sequence,
) -> Result<Position, BidiError> {
    Ok(Position {
        run: sequence.last,
        index: Run::read_prepared(store, sequence.last).await?.end - 1,
    })
}

pub(super) async fn compute(
    store: &mut CooperativeStore,
    paragraph: Paragraph,
    runs: Range<u32>,
) -> Result<u32, BidiError> {
    use BidiClass::*;
    let mut stack = DiskStack::new();
    store.stack_push(&mut stack, NONE, NONE).await?; // The algorithm's empty bottom sequence is a sentinel, not an actual enumerable run.
    let mut count = 0;
    for id in runs {
        let run = Run::read_prepared(store, id).await?;
        if run.start >= run.end {
            return Err(BidiError::CorruptMetadata);
        }
        let start_class = Text::read_prepared(store, run.start).await?.original;
        let mut end_class = start_class;
        // X9-removed trailing records do not determine the run's isolate-ending behavior.
        for index in (run.start..run.end).rev() {
            let class = Text::read_prepared(store, index).await?.original;
            if !removed(class) {
                end_class = class;
                break;
            }
        }
        let (head, tail) = if start_class == PDI && stack.len() > 1 {
            // Resume the original suspended chain only when an actual matching PDI starts this run.
            store
                .stack_pop(&mut stack)
                .await?
                .ok_or(BidiError::CorruptMetadata)?
        } else {
            (NONE, NONE)
        };
        let head = if head == NONE {
            id
        } else {
            // Connect real run records in place; a resumed chain does not copy a list accumulated while its isolate was suspended.
            let mut previous = Run::read_prepared(store, tail).await?;
            previous.next = id;
            previous.write_prepared(store, tail).await?;
            let mut current = run;
            current.previous = tail;
            current.write_prepared(store, id).await?;
            head
        };
        if matches!(end_class, RLI | LRI | FSI) {
            store.stack_push(&mut stack, head, id).await?; // A matching PDI restores this actual chain without copying accumulated runs.
        } else {
            emit(store, paragraph, head, id, count).await?;
            count += 1;
        }
    }
    while let Some((head, tail)) = store.stack_pop(&mut stack).await? {
        // Unmatched suspended chains still become actual sequences at paragraph end; the empty bottom sentinel emits nothing.
        if head != NONE {
            emit(store, paragraph, head, tail, count).await?;
            count += 1;
        }
    }
    Ok(count)
}

async fn emit(
    store: &mut CooperativeStore,
    paragraph: Paragraph,
    head: u32,
    tail: u32,
    id: u32,
) -> Result<(), BidiError> {
    let mut sequence = Sequence {
        first: head,
        last: tail,
        sos: BidiClass::L,
        eos: BidiClass::L,
    };
    let first = first(store, sequence).await?;
    let last = last(store, sequence).await?;
    // Determine effective endpoint levels along this IRS, skipping X9-removed records without discarding their original coordinates.
    let mut start_level = Text::read_prepared(store, first.index).await?.level;
    let mut cursor = Some(first);
    while let Some(position) = cursor {
        let text = Text::read_prepared(store, position.index).await?;
        if !removed(text.original) {
            start_level = text.level;
            break;
        }
        cursor = next(store, position).await?;
    }
    let mut end_level = Text::read_prepared(store, last.index).await?.level;
    let mut cursor = Some(last);
    while let Some(position) = cursor {
        let text = Text::read_prepared(store, position.index).await?;
        if !removed(text.original) {
            end_level = text.level;
            break;
        }
        cursor = previous(store, position).await?;
    }
    let mut predecessor = paragraph.base;
    // X10 uses actual surrounding paragraph levels for sos/eos; an ending isolate does not acquire an unrelated following strong context.
    for index in (paragraph.start..first.index).rev() {
        let text = Text::read_prepared(store, index).await?;
        if !removed(text.original) {
            predecessor = text.level;
            break;
        }
    }
    let mut last_non_removed = BidiClass::BN;
    for index in (paragraph.start..=last.index).rev() {
        let text = Text::read_prepared(store, index).await?;
        if !removed(text.original) {
            last_non_removed = text.original;
            break;
        }
    }
    let mut successor = paragraph.base;
    if !matches!(
        last_non_removed,
        BidiClass::RLI | BidiClass::LRI | BidiClass::FSI
    ) {
        for index in last.index + 1..paragraph.end {
            let text = Text::read_prepared(store, index).await?;
            if !removed(text.original) {
                successor = text.level;
                break;
            }
        }
    }
    sequence.sos = direction(start_level.max(predecessor));
    sequence.eos = direction(end_level.max(successor));
    sequence.write_prepared(store, id).await
}

/// Update the actual cursor interval within one IRS; its source envelope must not include intervening isolated text as though it belonged to this sequence.
pub(super) async fn set_processing(
    store: &mut CooperativeStore,
    start: Option<Position>,
    end: Option<Position>,
    class: BidiClass,
) -> Result<(), BidiError> {
    let mut at = start;
    while let Some(position) = at {
        if end.is_some_and(|end| end.run == position.run && end.index == position.index) {
            break;
        }
        let mut text = Text::read_prepared(store, position.index).await?;
        text.processing = class;
        text.write_prepared(store, position.index).await?;
        at = next(store, position).await?;
    }
    Ok(())
}
