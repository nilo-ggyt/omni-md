//! Conservative, bounded bracket cues over canonical Markdown source.
//!
//! Matching follows the preserved editor's four caret-side candidates and equal-token-context
//! nesting for the contexts admitted below. This is not a replacement Lezer parser: mixed HTML,
//! reference definitions and incomplete context return an explicit uncertainty outcome. A large
//! note can still supply a small complete block through the existing checkpoint envelope query.
//! That acquisition has its own 128 Ki UTF-16 / 256 KiB scan limits; classification and matching
//! accept at most 10,000 UTF-16 units. All source, parser state and token classes are transient.

use std::ops::Range;

use crate::{MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownViewportSemanticStatus};

mod context;

use context::{Context, classify_contexts};

/// Bound both the admitted classification envelope and each directional matching walk.
pub const MARKDOWN_BRACKET_SCAN_UTF16_UNITS: usize = 10_000;

/// Canonical scalar-aligned handles ready for native geometry after the document borrow ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownBracketMatch {
    /// The bracket next to the caret; ranges leave room for a future proven multi-unit handle.
    pub start_utf16: Range<usize>,
    /// The first opposing bracket in the same admitted token context, when one was found.
    pub end_utf16: Option<Range<usize>>,
    /// A found partner has the same bracket kind; a complete unmatched walk is false.
    pub matched: bool,
}

/// A missing cue carries its resource/context reason instead of pretending to be an unmatched pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownBracketQuery {
    /// No supported ASCII bracket occurs at any of the four adjacent caret-side candidates.
    NoMatch,
    /// Context and the complete search through its first opposing bracket are established.
    Match(MarkdownBracketMatch),
    /// Classification or matching would exceed the independent 10,000-unit source budget.
    BudgetExhausted,
    /// The complete syntax/search context cannot be established within the admitted grammar/envelope.
    ContextUnavailable,
}

/// Query supported cues without mutating canonical text or retaining a parse/cache after this call.
///
/// Complete documents up to 10,000 UTF-16 units support searches across separate equal-kind blocks.
/// Larger documents use a complete checkpoint-backed block around the caret. Reaching that block's
/// unknown outside edge returns `ContextUnavailable`, whereas reaching a proven document edge can
/// report an unmatched bracket. Existing envelope acquisition may inspect up to 128 Ki UTF-16 / 256
/// KiB, separately from the 10,000-unit classification/matching cap; it never rescans an unbounded
/// prefix. The admitted offset-context mapping is regression-tested against frozen CodeMirror output
/// and deliberately excludes mixed HTML and reference definitions, rather than claiming full Lezer
/// equivalence. Any raw '<' in a large note conservatively blocks local-context proof; an escaped
/// backtick also blocks an acquired envelope because the parsers disagree at adjacent backtick runs.
/// No native state, persistent token tree, or full-document mutable mirror is created.
pub fn markdown_bracket_match(
    document: &MarkdownDocumentBuffer,
    caret_utf16: usize,
) -> Result<MarkdownBracketQuery, MarkdownDocumentBufferError> {
    // Reject out-of-range and split-surrogate carets before acquiring source or parser scratch.
    document.byte_offset_for_utf16(caret_utf16)?;
    if !adjacent_ascii_bracket(document, caret_utf16)? {
        // HTML tag-name handles are outside the admitted ASCII-cue API, even away from a bracket.
        return Ok(MarkdownBracketQuery::NoMatch);
    }

    // A short complete document establishes global node context without any checkpoint acquisition.
    let (range, source) = if document.utf16_len() <= MARKDOWN_BRACKET_SCAN_UTF16_UNITS {
        let slice = document.materialize_viewport(0..document.utf16_len(), 0)?;
        (slice.utf16_range, slice.text)
    } else {
        if document.contains_ascii_less_than() {
            // Existing block checkpoints do not track HTML across blank lines. A far earlier '<'
            // can therefore invalidate this block's apparent Paragraph identity; omit the cue in O(1).
            return Ok(MarkdownBracketQuery::ContextUnavailable);
        }
        // Reuse bounded block proof for ordinary short paragraphs deep inside a large note.
        let mut start = caret_utf16.saturating_sub(1);
        let mut end = caret_utf16.saturating_add(1).min(document.utf16_len());
        if document.byte_offset_for_utf16(start)
            == Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        {
            // A supplementary scalar before the caret needs its leading surrogate inside the request.
            start -= 1;
        }
        if document.byte_offset_for_utf16(end)
            == Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
        {
            // Include the whole following scalar while keeping the requested extent at most four units.
            end += 1;
        }
        let slice = document.materialize_markdown_viewport(start..end, 0)?;
        if slice.semantic_status != MarkdownViewportSemanticStatus::Complete {
            // A source fallback is not independently parseable Markdown and cannot authorize a cue.
            return Ok(MarkdownBracketQuery::ContextUnavailable);
        }
        if slice.utf16_range.len() > MARKDOWN_BRACKET_SCAN_UTF16_UNITS {
            // Drop the bounded acquisition before constructing any classifier/parser over a giant block.
            return Ok(MarkdownBracketQuery::BudgetExhausted);
        }
        (slice.utf16_range, slice.text)
    };

    // Offset events only classify this fixed-size complete source envelope, never a retained AST.
    let Some(contexts) = classify_contexts(&source) else {
        // Unsupported grammar is an omitted cue, not a fabricated match from plain-text nesting.
        return Ok(MarkdownBracketQuery::ContextUnavailable);
    };
    let local_caret = caret_utf16 - range.start;
    let caret_byte = byte_for_utf16(&source, local_caret);
    let before = caret_byte.checked_sub(1);
    let candidates = [
        (before, false),
        (before, true),
        (Some(caret_byte), true),
        (Some(caret_byte), false),
    ];
    for (candidate, forward) in candidates {
        // Preserve closing-before, opening-before, opening-after, closing-after precedence exactly.
        let Some(start) = candidate.filter(|offset| *offset < source.len()) else {
            // A document/envelope edge cannot supply a bracket on that side of the caret.
            continue;
        };
        let Some((kind, opening)) = bracket(source.as_bytes()[start]) else {
            // A UTF-8 continuation byte cannot be an ASCII bracket and is harmless to inspect here.
            continue;
        };
        if opening != forward {
            // Only the direction entering this bracket's interior is an applicable candidate.
            continue;
        }
        // An earlier uncertain candidate suppresses later ones; otherwise a bounded miss could change precedence.
        return Ok(scan_partner(
            &source,
            &contexts,
            &range,
            document.utf16_len(),
            start,
            kind,
            forward,
        ));
    }
    // A complete envelope that excluded an adjacent outside candidate has no safe local interpretation.
    Ok(MarkdownBracketQuery::ContextUnavailable)
}

/// Inspect only the two neighboring code units before paying for any Markdown source acquisition.
fn adjacent_ascii_bracket(
    document: &MarkdownDocumentBuffer,
    caret: usize,
) -> Result<bool, MarkdownDocumentBufferError> {
    let start = caret.saturating_sub(1);
    let end = caret.saturating_add(1).min(document.utf16_len());
    let mut units = [0_u16; 2];
    document.copy_utf16_code_units(start, &mut units[..end - start])?;
    Ok(units[..end - start].iter().any(|unit| {
        // Only ASCII punctuation participates; surrogate halves never become candidate delimiters.
        u8::try_from(*unit).ok().and_then(bracket).is_some()
    }))
}

/// Convert a validated local scalar-aligned UTF-16 boundary inside a bounded envelope.
fn byte_for_utf16(source: &str, position: usize) -> usize {
    let mut utf16 = 0;
    for (byte, scalar) in source.char_indices() {
        // Stop before consuming the scalar starting exactly at the requested coordinate.
        if utf16 == position {
            return byte;
        }
        utf16 += scalar.len_utf16();
    }
    source.len()
}

/// Group the default editor delimiters into kind and direction without allocating a stack.
fn bracket(byte: u8) -> Option<(u8, bool)> {
    // Pair index retains mismatch identity; even positions are opening and odd positions are closing.
    b"()[]{}"
        .iter()
        .position(|candidate| {
            // Other punctuation, including HTML angle brackets, has no default plain matching authority.
            *candidate == byte
        })
        .map(|index| {
            // This fixed six-byte table makes the narrowing exact and independent of source size.
            ((index / 2) as u8, index % 2 == 0)
        })
}

/// Walk one admitted context with a fixed unit budget and no nesting stack or retained source copy.
fn scan_partner(
    source: &str,
    contexts: &[Context],
    envelope: &Range<usize>,
    document_len: usize,
    start: usize,
    kind: u8,
    forward: bool,
) -> MarkdownBracketQuery {
    let start_utf16 = envelope.start + source[..start].encode_utf16().count();
    let handle = start_utf16..start_utf16 + 1;
    let context = contexts[start];
    let mut depth = 1_usize;
    let mut distance = 1_usize;
    let mut cursor = if forward {
        // Opening candidates begin scanning immediately after their own ASCII code unit.
        start + 1
    } else {
        // Closing candidates retreat before their own code unit on the first iteration.
        start
    };
    loop {
        // Reaching an envelope edge proves EOF only when that edge is also the actual document boundary.
        if (forward && cursor == source.len()) || (!forward && cursor == 0) {
            return if (forward && envelope.end == document_len) || (!forward && envelope.start == 0)
            {
                // Complete exhaustion of the real search direction gives a truthful unmatched cue.
                MarkdownBracketQuery::Match(MarkdownBracketMatch {
                    start_utf16: handle,
                    end_utf16: None,
                    matched: false,
                })
            } else {
                // Equal token kinds can occur in later/earlier blocks; a local edge cannot prove unmatched.
                MarkdownBracketQuery::ContextUnavailable
            };
        }
        let byte = if forward {
            // Advance one complete scalar; only ASCII brackets require subsequent kind inspection.
            cursor
        } else {
            // Retreat to a scalar boundary without exposing surrogate halves or invalid UTF-8 slices.
            let mut previous = cursor - 1;
            while !source.is_char_boundary(previous) {
                // A valid UTF-8 scalar has at most three continuation bytes to retreat across.
                previous -= 1;
            }
            previous
        };
        let scalar = source[byte..]
            .chars()
            .next()
            .expect("source scalar at validated cursor");
        distance += scalar.len_utf16();
        if distance > MARKDOWN_BRACKET_SCAN_UTF16_UNITS {
            // A resource stop is never an EOF/unmatched finding, even on one giant logical line.
            return MarkdownBracketQuery::BudgetExhausted;
        }
        if scalar.is_ascii()
            && contexts[byte] == context
            && let Some((other_kind, opening)) = bracket(scalar as u8)
        {
            // Only the start token's identity contributes to nesting; other Markdown leaves are ignored.
            if opening == forward {
                // Equal-context brackets in the entering direction increase depth, regardless of kind.
                depth += 1;
            } else if depth == 1 {
                // The first opposing kind completes the walk, including a deliberate mismatch result.
                let end = envelope.start + source[..byte].encode_utf16().count();
                return MarkdownBracketQuery::Match(MarkdownBracketMatch {
                    start_utf16: handle,
                    end_utf16: Some(end..end + 1),
                    matched: kind == other_kind,
                });
            } else {
                // An inner pair closes before the candidate's own opposing delimiter is considered.
                depth -= 1;
            }
        }
        cursor = if forward {
            // Continue at the next complete scalar without materializing UTF-16 code-unit arrays.
            byte + scalar.len_utf8()
        } else {
            // The next reverse iteration retreats from this scalar's starting boundary.
            byte
        };
    }
}

#[cfg(test)]
mod tests;
