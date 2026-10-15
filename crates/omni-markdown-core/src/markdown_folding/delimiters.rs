//! Exact delimiter metadata and finite HTML termination over canonical source chunks.
//!
//! No prefix truncation grants hiding authority: delimiter runs, info validity and closing tails
//! include the complete source line, while explicit HTML terminators use a finite overlap matcher.

use super::lines::Line;
use super::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, PREFIX_UNITS};

/// Complete delimiter metadata is streamed even when its run or info text exceeds the prefix cap.
#[derive(Clone, Copy)]
pub(super) struct FenceDelimiter {
    pub(super) marker: u8,
    pub(super) length: usize,
    pub(super) valid_opening: bool,
    pub(super) closing_tail: bool,
}

/// HTML block termination is finite state; ambiguous long starts grant no hiding authority.
#[derive(Clone, Copy)]
pub(super) enum HtmlBlock {
    UntilBlank,
    Until(&'static str),
    Uncertain,
}

/// Classify complete punctuation-only lines without granting authority to a truncated long prefix.
pub(super) fn marker_line_metadata(
    document: &MarkdownDocumentBuffer,
    start: usize,
    end: usize,
    prefix: &str,
    complete: bool,
) -> Result<(Option<usize>, bool), MarkdownDocumentBufferError> {
    if !prefix
        .trim_start_matches(' ')
        .starts_with(['=', '-', '*', '_'])
        || (!complete && end - start > PREFIX_UNITS)
    {
        // Ordinary lines need no second scan; paint-time hints never inspect a giant marker body.
        return Ok((None, false));
    }
    let mut scratch = [0_u16; PREFIX_UNITS];
    let mut offset = start;
    let mut indentation = 0;
    let mut marker = None;
    let mut count = 0;
    let mut spacing = false;
    let mut contiguous = true;
    while offset < end {
        // Candidate lines stream with finite scratch even when an underline spans the entire note.
        let length = (end - offset).min(scratch.len());
        document.copy_utf16_code_units(offset, &mut scratch[..length])?;
        for &unit in &scratch[..length] {
            // Only the initial zero-to-three spaces precede a top-level punctuation marker.
            let Some(selected) = marker else {
                if unit == 32 && indentation < 3 {
                    // Three spaces remain legal; the fourth instead classifies an indented code line.
                    indentation += 1;
                    continue;
                }
                if matches!(unit, 61 | 45 | 42 | 95) {
                    // One marker identity excludes mixed punctuation without a retained token list.
                    marker = Some(unit);
                    count = 1;
                    continue;
                }
                // Tabs, four-space indentation and other initial content cannot form these delimiters.
                return Ok((None, false));
            };
            if unit == selected {
                // Spaces between markers allow thematic breaks, while invalidating Setext contiguity.
                count += 1;
                contiguous &= !spacing;
            } else if matches!(unit, 9 | 32) {
                // Trailing ASCII spaces/tabs are legal for both forms; Unicode whitespace is content.
                spacing = true;
            } else {
                // Any different punctuation/text scalar ends delimiter authority for the complete line.
                return Ok((None, false));
            }
        }
        offset += length;
    }
    let setext = if contiguous && matches!(marker, Some(61 | 45)) {
        // Contiguous equals and hyphen runs identify level one and two respectively.
        Some(if marker == Some(61) {
            // An equals underline denotes a level-one heading.
            1
        } else {
            // The remaining eligible contiguous marker is a level-two hyphen underline.
            2
        })
    } else {
        // Spaced, empty and other marker lines cannot promote an ordinary paragraph into a heading.
        None
    };
    Ok((setext, count >= 3 && matches!(marker, Some(45 | 42 | 95))))
}

/// Inspect the complete potential fence with fixed UTF-16 scratch, including long runs and info text.
pub(super) fn scan_fence_delimiter(
    document: &MarkdownDocumentBuffer,
    start: usize,
    end: usize,
) -> Result<Option<FenceDelimiter>, MarkdownDocumentBufferError> {
    let mut scratch = [0_u16; PREFIX_UNITS];
    let mut at = start;
    let mut spaces = 0;
    let mut marker = None;
    let mut length = 0;
    let mut tail = false;
    let mut closing_tail = true;
    let mut backtick_info = false;
    while at < end {
        // Every chunk consumes source units without retaining any line-sized string or grammar tree.
        let count = (end - at).min(scratch.len());
        document.copy_utf16_code_units(at, &mut scratch[..count])?;
        for &unit in &scratch[..count] {
            // Only zero-to-three initial spaces may precede a top-level or container-relative fence.
            let Some(selected) = marker else {
                if unit == 32 && spaces < 3 {
                    // Limited indentation remains eligible for a delimiter at this source level.
                    spaces += 1;
                    continue;
                }
                if matches!(unit, 96 | 126) {
                    // The first delimiter unit fixes the marker kind for the complete run.
                    marker = Some(unit);
                    length = 1;
                    continue;
                }
                // Any other first content scalar is not a fence candidate.
                return Ok(None);
            };
            if !tail && unit == selected {
                // Count the entire run, not merely the first 1024 units visible in the prefix hint.
                length += 1;
            } else {
                // Closing fences allow only spaces/tabs; backtick opening info forbids any backtick.
                tail = true;
                closing_tail &= matches!(unit, 9 | 32);
                backtick_info |= unit == 96;
            }
        }
        at += count;
    }
    Ok(marker.filter(|_| length >= 3).map(|marker| {
        // The complete delimiter metadata is authoritative even for a document-sized marker/info line.
        FenceDelimiter {
            marker: marker as u8,
            length,
            valid_opening: marker == 126 || !backtick_info,
            closing_tail,
        }
    }))
}

/// Search a finite ASCII HTML terminator across fixed UTF-16 chunks without copying its body.
pub(super) fn html_ends(
    document: &MarkdownDocumentBuffer,
    line: &Line,
    block: HtmlBlock,
) -> Result<bool, MarkdownDocumentBufferError> {
    let pattern = match block {
        HtmlBlock::UntilBlank | HtmlBlock::Uncertain => {
            // Blank-line termination also conservatively closes an uncertain long-tag context.
            return Ok(line.blank);
        }
        HtmlBlock::Until(pattern) => {
            // Explicit terminators, including raw tags, match case-insensitively with finite state.
            pattern.as_bytes()
        }
    };
    let mut prefix = [0_usize; 16];
    let mut matched = 0;
    for index in 1..pattern.len() {
        // Build a tiny KMP table for the fixed terminator, preserving overlaps across source chunks.
        while matched > 0 && pattern[index] != pattern[matched] {
            // Reuse the longest proper prefix after a mismatch instead of rescanning source units.
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            // Extend the finite overlap prefix only when its next ASCII unit matches.
            matched += 1;
        }
        prefix[index] = matched;
    }
    matched = 0;
    let mut scratch = [0_u16; PREFIX_UNITS];
    let mut at = line.start;
    while at < line.end {
        // The scan touches each unit once and retains no text after its current bounded chunk.
        let count = (line.end - at).min(scratch.len());
        document.copy_utf16_code_units(at, &mut scratch[..count])?;
        for &unit in &scratch[..count] {
            let unit = if (65..=90).contains(&unit) {
                // ASCII case folding preserves native unit width and does not reinterpret Unicode text.
                unit + 32
            } else {
                // Non-ASCII units cannot equal an ASCII terminator but remain safe scan input.
                unit
            };
            while matched > 0 && unit != u16::from(pattern[matched]) {
                // Fall back through the finite terminator state, preserving cross-chunk overlaps.
                matched = prefix[matched - 1];
            }
            if unit == u16::from(pattern[matched]) {
                // This source unit extends the currently recognized terminator prefix.
                matched += 1;
            }
            if matched == pattern.len() {
                // An actual explicit close ends opacity on this source line, including a same-line close.
                return Ok(true);
            }
        }
        at += count;
    }
    Ok(false)
}
