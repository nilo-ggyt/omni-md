//! One outer action owns the template arena. Each hit records capture coordinates
//! without duplicating shared replacement literals.

use std::collections::HashMap;

use super::*;

/// Literal ranges belong to the arena, not the document. Capture tokens point to
/// deduplicated slots in each spool record.
#[derive(Clone, Debug)]
pub(super) enum TemplateToken {
    Literal { bytes: Range<usize>, utf16: usize },
    Capture(usize),
}

/// The owner is independent of the reader, so Cow values passed to Core can safely
/// borrow the arena until the synchronous transaction ends.
pub(super) struct ReplacementTemplate {
    pub(super) arena: String,
    pub(super) tokens: Vec<TemplateToken>,
    pub(super) captures: Vec<usize>,
}

impl ReplacementTemplate {
    /// Parse the bounded replacement language once. Invalid numeric references
    /// remain literal rather than inventing absent capture groups.
    pub(super) fn parse(text: &str, regex: bool, capture_count: usize) -> Self {
        let mut result = Self {
            arena: String::new(),
            tokens: Vec::new(),
            captures: Vec::new(),
        };
        if !regex {
            // Literal mode never expands dollars; copy the bounded template only once.
            result.literal(text);
            return result;
        }
        let mut captures = HashMap::new();
        let mut literal = String::new();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            // Parse forward through bounded template text. Numeric-prefix attempts
            // depend on token length only and never scan document contents.
            if bytes[index] == b'$' && index + 1 < bytes.len() {
                if bytes[index + 1] == b'$' {
                    // Normalize doubled dollars once in the arena, not separately for every match.
                    literal.push('$');
                    index += 2;
                    continue;
                }
                let mut capture = None;
                let mut consumed = 1;
                if bytes[index + 1] == b'&' {
                    capture = Some(0); // The whole match is deduplicated capture zero.
                } else if bytes[index + 1].is_ascii_digit() {
                    let mut end = index + 1;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1; // Bound the complete digit run before trying its longest valid prefix.
                    }
                    for candidate_end in (index + 2..=end).rev() {
                        if let Ok(candidate) = text[index + 1..candidate_end].parse::<usize>()
                            && candidate > 0
                            && candidate < capture_count
                        {
                            // Leave surplus digits for the next literal; invalid
                            // $0/out-of-range references retain their original spelling.
                            capture = Some(candidate);
                            consumed = candidate_end - index - 1;
                            break;
                        }
                    }
                }
                if let Some(capture) = capture {
                    // Finalize the literal before adding a capture token; its
                    // arena byte offsets remain stable as later text is appended.
                    result.literal(&literal);
                    literal.clear();
                    let slot = *captures.entry(capture).or_insert_with(|| {
                        let slot = result.captures.len();
                        result.captures.push(capture);
                        slot
                    });
                    result.tokens.push(TemplateToken::Capture(slot));
                    index += consumed + 1;
                    continue;
                }
            }
            let character = text[index..]
                .chars()
                .next()
                .expect("the template index is scalar aligned");
            literal.push(character);
            index += character.len_utf8();
        }
        result.literal(&literal);
        result
    }

    fn literal(&mut self, text: &str) {
        if text.is_empty() {
            // Empty fragments need no token; an empty template still expresses deletion.
            return;
        }
        let start = self.arena.len();
        self.arena.push_str(text);
        self.tokens.push(TemplateToken::Literal {
            bytes: start..self.arena.len(),
            utf16: text.encode_utf16().count(),
        });
    }

    /// Derive inserted length from fixed literal widths and this record's capture
    /// ranges, rejecting arithmetic overflow before Core applies any replacement.
    pub(super) fn inserted_utf16(
        &self,
        captures: &[Option<Range<usize>>],
    ) -> Result<usize, String> {
        self.tokens.iter().try_fold(0usize, |total, token| {
            let units = match token {
                TemplateToken::Literal { utf16, .. } => *utf16, // Literal width is computed once while parsing.
                TemplateToken::Capture(slot) => captures
                    .get(*slot)
                    .ok_or("替换捕获记录不完整")?
                    .as_ref()
                    .map_or(9, Range::len), // The baseline expands a nonparticipating branch to "undefined".
            };
            total
                .checked_add(units)
                .ok_or_else(|| "替换内容超过 UTF-16 长度范围".to_owned())
        })
    }
}
