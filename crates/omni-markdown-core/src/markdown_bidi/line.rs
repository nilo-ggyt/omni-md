// Copyright 2015 The Servo Project Developers.
// L1 adapted from unicode-bidi 0.3.18 lib.rs. SPDX-License-Identifier: MIT OR Apache-2.0
//! L1 for an actual soft line writes reusable disk scratch; L2 emits runs through a bounded level tree, without a whole-line index map.

use super::*;

pub(super) fn visit(
    store: &mut Store,
    paragraph_count: u32,
    source_start: usize,
    line: Range<u32>,
    mut visit: impl FnMut(MarkdownBidiRun),
) -> Result<(), BidiError> {
    if line.is_empty() {
        return Ok(());
    }
    let mut low = 0;
    let mut high = paragraph_count;
    // Locate the actual containing paragraph in the original metadata; a single L1 operation cannot cross another paragraph's base.
    let paragraph = loop {
        if low >= high {
            return Err(BidiError::InvalidRange);
        }
        let mid = low + (high - low) / 2;
        let paragraph = Paragraph::read(store, mid)?;
        if line.start < paragraph.start {
            high = mid;
        } else if line.start >= paragraph.end {
            low = mid + 1;
        } else {
            break paragraph;
        }
    };
    if line.end > paragraph.end {
        return Err(BidiError::InvalidRange);
    } // One L1 operation must not cross a different UBA paragraph's base.
    let mut at = line.start;
    // Copy resolved levels into reusable line scratch, leaving the immutable paragraph analysis available to other actual lines.
    while at < line.end {
        let text = Text::read(store, at)?;
        set(store, at, text.level)?;
        at += 1;
    }
    use BidiClass::*;
    let mut reset_from = Some(line.start);
    let mut previous = paragraph.base;
    let mut at = line.start;
    while at < line.end {
        let text = Text::read(store, at)?;
        let end = at + text.scalar_len()?;
        match text.original {
            B | S => {
                // Reset the separator and its preceding resettable sequence to the actual paragraph base.
                let from = *reset_from.get_or_insert(at);
                for unit in from..end {
                    set(store, unit, paragraph.base)?;
                }
                reset_from = None;
            }
            WS | FSI | LRI | RLI | PDI => {
                reset_from.get_or_insert(at);
            }
            RLE | LRE | RLO | LRO | PDF | BN => {
                // X9-removed controls inherit the preceding resolved level while remaining part of the trailing resettable sequence.
                reset_from.get_or_insert(at);
                for unit in at..end {
                    set(store, unit, previous)?;
                }
            }
            _ => reset_from = None,
        }
        previous = get(store, at)?;
        at = end;
    }
    if let Some(from) = reset_from {
        // A real line end applies L1 to its trailing whitespace/formatting sequence, not just a source-buffer or item endpoint.
        for unit in from..line.end {
            set(store, unit, paragraph.base)?;
        }
    }
    // Recurse only into higher levels; the level hierarchy bounds depth to 127 independently of the line's length.
    emit(store, line, source_start, &mut visit, 0)
}

fn get(store: &mut Store, index: u32) -> Result<u8, BidiError> {
    let level = store.read(Table::Run, index)?[0];
    if level > 126 {
        Err(BidiError::CorruptMetadata)
    } else {
        Ok(level)
    }
}

fn set(store: &mut Store, index: u32, level: u8) -> Result<(), BidiError> {
    let mut bytes = [0; 32];
    bytes[0] = level;
    store.write(Table::Run, index, bytes)
}

fn scalar_before(store: &mut Store, end: u32) -> Result<u32, BidiError> {
    // Walk backward to the actual scalar head so a visual reversal never separates a surrogate pair.
    let last = end.checked_sub(1).ok_or(BidiError::CorruptMetadata)?;
    if Text::read(store, last)?.continuation() {
        last.checked_sub(1).ok_or(BidiError::CorruptMetadata)
    } else {
        Ok(last)
    }
}

fn emit(
    store: &mut Store,
    range: Range<u32>,
    global: usize,
    visit: &mut impl FnMut(MarkdownBidiRun),
    depth: u8,
) -> Result<(), BidiError> {
    // The recursive state is a range and level depth only; glyph order is streamed to the caller rather than retained as an index inventory.
    if depth > 126 {
        return Err(BidiError::CorruptMetadata);
    }
    let mut minimum = 126;
    let mut at = range.start;
    while at < range.end {
        minimum = minimum.min(get(store, at)?);
        at += Text::read(store, at)?.scalar_len()?;
    }
    let reverse = minimum & 1 != 0;
    // Reverse traversal at odd levels and recurse into contiguous higher-level groups, retaining actual global UTF16 source ranges.
    let mut cursor = if reverse { range.end } else { range.start };
    while if reverse {
        cursor > range.start
    } else {
        cursor < range.end
    } {
        let position = if reverse {
            scalar_before(store, cursor)?
        } else {
            cursor
        };
        let higher = get(store, position)? > minimum;
        let edge = cursor;
        loop {
            cursor = if reverse {
                scalar_before(store, cursor)?
            } else {
                cursor + Text::read(store, cursor)?.scalar_len()?
            };
            if cursor == if reverse { range.start } else { range.end } {
                break;
            }
            let probe = if reverse {
                scalar_before(store, cursor)?
            } else {
                cursor
            };
            if (get(store, probe)? > minimum) != higher {
                break;
            }
        }
        let part = if reverse { cursor..edge } else { edge..cursor };
        if higher {
            emit(store, part, global, visit, depth + 1)?;
        } else {
            visit(MarkdownBidiRun {
                source_utf16_start: global + part.start as usize,
                source_utf16_end: global + part.end as usize,
                level: minimum,
            });
        }
    }
    Ok(())
}
