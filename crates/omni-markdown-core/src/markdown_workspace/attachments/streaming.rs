//! Rewrite moved attachment links with a finite source cursor and anonymous
//! output. The contiguous grammar remains only an independent test oracle.
use std::{
    io::{self, BufWriter, Seek, SeekFrom, Write},
    ops::Range,
};

use super::*;
use crate::original_backing::ORIGINAL_READ_BYTES;

type Result<T> = std::result::Result<T, MarkdownWorkspaceError>;

/// Own complete flushed output rewound for copying. Outer cancellation/errors
/// drop the anonymous file without publishing a filesystem name or a text mirror.
pub(in super::super) struct RewrittenMarkdown {
    pub(in super::super) file: File,
    pub(in super::super) byte_len: u64,
    pub(in super::super) revision: String,
}

/// Copy only one fixed lease into scratch. Lookahead retains no arbitrary token,
/// and changing windows never recaptures a different document version.
struct ByteCursor<'a> {
    source: &'a BoundedMarkdownText,
    bytes: [u8; ORIGINAL_READ_BYTES],
    start: usize,
    end: usize,
}
impl<'a> ByteCursor<'a> {
    /// Borrow the already-frozen source and begin with no cached window.
    fn new(source: &'a BoundedMarkdownText) -> Self {
        Self {
            source,
            bytes: [0; ORIGINAL_READ_BYTES],
            start: 0,
            end: 0,
        }
    }
    /// Fetch one original byte from the same finite scratch, preserving actual EOF.
    fn byte(&mut self, offset: usize) -> Result<Option<u8>> {
        if offset >= self.source.len() {
            // No invented sentinel may alter the grammar's original byte coordinates.
            return Ok(None);
        }
        if offset < self.start || offset >= self.end {
            // Refill just the containing 4 KiB window and reclaim its source lease
            // before subsequent grammar work uses the copied finite scratch.
            self.start = offset / ORIGINAL_READ_BYTES * ORIGINAL_READ_BYTES;
            self.end = self
                .start
                .saturating_add(ORIGINAL_READ_BYTES)
                .min(self.source.len());
            self.source
                .with_bytes(self.start..self.end, &|| true, |bytes| {
                    self.bytes[..bytes.len()].copy_from_slice(bytes);
                })?;
        }
        Ok(Some(self.bytes[offset - self.start]))
    }
    /// Locate an arbitrary-length line through windows without materializing the line.
    fn line_end(&mut self, mut start: usize) -> Result<usize> {
        while start < self.source.len() {
            // Load only the containing fixed window; scan its existing scratch below.
            self.byte(start)?;
            let end = self.end;
            if let Some(local) = self.bytes[start - self.start..end - self.start]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                // Translate the window-local newline into the same source byte coordinate.
                return Ok(start + local);
            }
            start = end;
        }
        Ok(self.source.len())
    }
    /// Count a delimiter run across leases while retaining only its scalar length.
    fn repeated(&mut self, mut start: usize, end: usize, expected: u8) -> Result<usize> {
        let origin = start;
        while start < end && self.byte(start)? == Some(expected) {
            // Even an arbitrarily long delimiter owns no token copy outside scratch.
            start += 1;
        }
        Ok(start - origin)
    }
    /// Compare the finite generated prefix against exact source bytes, including EOF.
    fn starts_with(&mut self, start: usize, prefix: &[u8]) -> Result<bool> {
        let Some(end) = start
            .checked_add(prefix.len())
            .filter(|end| *end <= self.source.len())
        else {
            // Overflow or an extent beyond real EOF cannot match a complete prefix.
            return Ok(false);
        };
        for (offset, expected) in (start..end).zip(prefix) {
            if self.byte(offset)? != Some(*expected) {
                // Stop at the first mismatch while preserving complete-prefix semantics.
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Recognize a possible fence with the existing three-space indentation rule.
    fn fence(&mut self, start: usize, end: usize) -> Result<Option<(u8, usize, usize)>> {
        let mut marker_start = start;
        while marker_start < end
            && marker_start - start < 4
            && self.byte(marker_start)? == Some(b' ')
        {
            marker_start += 1;
        }
        if marker_start - start > 3 {
            // Four leading spaces are ordinary indented content, not a fence.
            return Ok(None);
        }
        let Some(marker @ (b'`' | b'~')) = self.byte(marker_start)? else {
            // Only the two actual fence markers can open or close opaque content.
            return Ok(None);
        };
        let count = self.repeated(marker_start, end, marker)?;
        Ok((count >= 3).then_some((marker, count, marker_start + count)))
    }
    /// Accept only the grammar's original ASCII whitespace after a closing fence.
    fn whitespace(&mut self, range: Range<usize>) -> Result<bool> {
        for offset in range {
            if !self
                .byte(offset)?
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                // Non-ASCII whitespace does not satisfy this established byte grammar.
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Validate a nonempty generated filename segment followed by its real ')'.
    fn encoded_name(&mut self, start: usize, end: usize) -> Result<bool> {
        let mut offset = start;
        while offset < end
            && self.byte(offset)?.is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'%' | b'-' | b'.' | b'_' | b'~')
            })
        {
            // Retain no URL-length cap or token copy; actual grammar determines its end.
            offset += 1;
        }
        Ok(offset > start && self.byte(offset)? == Some(b')'))
    }
}

/// Count/hash bytes only after the bounded writer accepts them. Its 4 KiB buffer
/// is independent of source length; finish must flush it before output is usable.
struct Output {
    file: BufWriter<File>,
    digest: Sha256,
    bytes: u64,
}
impl Output {
    /// Acquire one anonymous spool lazily, owned solely by this rewrite attempt.
    fn new() -> Result<Self> {
        let file = tempfile::tempfile().map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        Ok(Self {
            file: BufWriter::with_capacity(ORIGINAL_READ_BYTES, file),
            digest: Sha256::new(),
            bytes: 0,
        })
    }
    /// Flush every accepted byte and rewind before handing the actual file to its consumer.
    fn finish(mut self) -> Result<RewrittenMarkdown> {
        self.file
            .flush()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let mut file = self
            .file
            .into_inner()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let digest = self.digest.finalize();
        let mut revision = String::with_capacity(digest.len() * 2);
        for byte in digest {
            // Publish a fixed-size exact-byte revision, not a retained copy of the output.
            use std::fmt::Write as _;
            let _ = write!(revision, "{byte:02x}");
        }
        Ok(RewrittenMarkdown {
            file,
            byte_len: self.bytes,
            revision,
        })
    }
}
impl Write for Output {
    /// Keep output extent and digest synchronized with successful bounded writes.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        let total = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| io::Error::other("附件改写输出长度溢出"))?;
        // Failed writes leave an unusable anonymous spool owned by this attempt;
        // do not advance its successful-byte accounting or publish a revision.
        self.file.write_all(bytes)?;
        self.digest.update(bytes);
        self.bytes = total;
        Ok(bytes.len())
    }
    /// Forward explicit flushing to the same bounded writer and its original file.
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Allocate output only when a real generated attachment destination needs a
/// different relative-depth prefix; unchanged source spans remain streamed verbatim.
pub(in super::super) fn rewrite_moved_note_attachment_links_streamed(
    content: &BoundedMarkdownText,
    source_relative_path: &str,
    destination_relative_path: &str,
) -> Result<Option<RewrittenMarkdown>> {
    let source_prefix = markdown_attachment_relative_prefix(source_relative_path)?;
    let destination_prefix = markdown_attachment_relative_prefix(destination_relative_path)?;
    // Independent callers still owe complete candidate UTF-8 validity, not only
    // validation near an otherwise recognizable attachment destination.
    content.ensure_validated(&|| true)?;
    if source_prefix == destination_prefix {
        // The move changes no relative depth, so retain the original complete byte sequence.
        return Ok(None);
    }
    let mut cursor = ByteCursor::new(content);
    let mut output = None::<Output>;
    let mut copied_until = 0;
    let mut line_start = 0;
    let mut inline_ticks = None;
    let mut fence = None;
    while line_start < content.len() {
        // Keep only source offsets and delimiter counts across arbitrarily long lines.
        let line_end = cursor.line_end(line_start)?;
        if inline_ticks.is_none()
            && let Some((marker, count, marker_end)) = cursor.fence(line_start, line_end)?
        {
            if let Some((opening, opening_count)) = fence {
                // An existing fence closes only with its own marker, sufficient
                // run length, and the original whitespace-only suffix rule.
                if marker == opening
                    && count >= opening_count
                    && cursor.whitespace(marker_end..line_end)?
                {
                    fence = None;
                }
            } else {
                // Preserve the existing grammar's unrestricted opening suffix.
                fence = Some((marker, count));
            }
            line_start = line_end.saturating_add(1);
            continue;
        }
        if fence.is_some() {
            // Leave every fenced byte in the later unchanged source span; neither
            // links nor inline-code state are parsed inside opaque fenced content.
            line_start = line_end.saturating_add(1);
            continue;
        }
        let mut index = line_start;
        while index < line_end {
            match cursor.byte(index)? {
                Some(b'\\') if inline_ticks.is_none() => {
                    // Escape one original byte without swallowing next-line syntax.
                    index = (index + 2).min(line_end);
                }
                Some(b'`') => {
                    // Only an equal-length run closes inline code. Carry this
                    // state across real lines while differing runs remain literal.
                    let count = cursor.repeated(index, line_end, b'`')?;
                    match inline_ticks {
                        Some(opening) if opening == count => inline_ticks = None,
                        None => inline_ticks = Some(count),
                        Some(_) => {}
                    }
                    index += count;
                }
                Some(b']')
                    if inline_ticks.is_none()
                        && cursor.byte(index + 1)? == Some(b'(')
                        && cursor.starts_with(index + 2, source_prefix.as_bytes())? =>
                {
                    let destination_start = index + 2;
                    let name_start = destination_start + source_prefix.len();
                    if cursor.encoded_name(name_start, line_end)? {
                        // Rewrite only a complete generated destination. Allocate
                        // the anonymous spool at its first actual replacement.
                        if output.is_none() {
                            output = Some(Output::new()?);
                        }
                        let writer = output.as_mut().expect("上支已经取得唯一output");
                        content.copy_range_to(copied_until..destination_start, writer)?;
                        writer
                            .write_all(destination_prefix.as_bytes())
                            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                        copied_until = name_start;
                        // Continue scanning the original filename and ')' rather
                        // than skipping any later syntax within the same source line.
                        index = name_start;
                    } else {
                        // Preserve empty, unclosed, or invalid destinations verbatim.
                        index += 1;
                    }
                }
                // Ordinary bytes, including UTF-8 continuations, advance only;
                // unchanged source spans preserve their exact output representation.
                _ => index += 1,
            }
        }
        line_start = line_end.saturating_add(1);
    }
    if let Some(mut writer) = output {
        // Complete the untouched suffix and expose output only after actual flush/rewind.
        content.copy_range_to(copied_until..content.len(), &mut writer)?;
        Ok(Some(writer.finish()?))
    } else {
        // With no real replacement, neither a spool nor a whole-text copy was created.
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
