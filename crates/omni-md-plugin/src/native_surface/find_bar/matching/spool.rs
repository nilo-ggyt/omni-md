//! Anonymous replacement records store ranges only. The outer action owns the
//! literal arena, independently of readers that borrow it.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::rc::Rc;

use super::{Found, ReplacementTemplate, TemplateToken};
use omni_markdown_core::{
    MarkdownDocumentBufferError, MarkdownReplacement, MarkdownReplacementPart,
    MarkdownTextSelection,
};

const MAX_SPOOL_BYTES: u64 = 256 * 1_024 * 1_024;
const IO_BYTES: usize = 32 * 1_024;

/// Each match writes its main range and only the deduplicated captures referenced
/// by the template, never duplicate replacement bytes.
pub(super) struct ReplacementSpool<'a> {
    file: BufWriter<File>,
    template: &'a ReplacementTemplate,
    bytes: u64,
    count: usize,
    inserted: usize,
}

impl<'a> ReplacementSpool<'a> {
    /// Create private unlinked storage before any edit. The File owns its descriptor
    /// and backing storage on every return, including metadata-verification failure.
    pub(super) fn new(template: &'a ReplacementTemplate) -> Result<Self, String> {
        let file = tempfile::tempfile().map_err(|_| "无法创建匿名替换事务")?;
        let metadata = file.metadata().map_err(|_| "无法验证替换事务")?;
        if metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 0 {
            // Shared access or a named record is unacceptable; reject before canonical editing.
            return Err("替换事务未满足私有匿名存储要求".into());
        }
        Ok(Self {
            file: BufWriter::with_capacity(IO_BYTES, file),
            template,
            bytes: 0,
            count: 0,
            inserted: 0,
        })
    }

    /// Enforce the on-disk record budget before writing. Any partial I/O failure
    /// rejects the enclosing action; this spool is never resumed as a valid prefix.
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_SPOOL_BYTES)
            .ok_or("替换事务超过 256 MiB 临时记录上限")?;
        self.file.write_all(bytes).map_err(|_| "无法写入替换事务")?;
        self.bytes = total;
        Ok(())
    }

    /// Any failure makes the caller discard this entire spool. A partial header
    /// cannot be reused or authorize partial replacement.
    pub(super) fn push(&mut self, found: &Found) -> Result<(), String> {
        self.write(&(found.range.start as u64).to_le_bytes())?;
        self.write(&(found.range.end as u64).to_le_bytes())?;
        for &capture in &self.template.captures {
            // Repeated references share one capture slot. A nonparticipating
            // branch is represented by one tag byte rather than a source range.
            match found.captures.get(capture).ok_or("替换捕获记录不完整")? {
                Some(range) => {
                    // Record only original-snapshot coordinates; Core validates ranges/scalar boundaries at commit.
                    self.write(&[1])?;
                    self.write(&(range.start as u64).to_le_bytes())?;
                    self.write(&(range.end as u64).to_le_bytes())?;
                }
                None => self.write(&[0])?, // Absent differs from empty; preserve baseline "undefined" expansion.
            }
        }
        let mut inserted = 0usize;
        for token in &self.template.tokens {
            let units = match token {
                TemplateToken::Literal { utf16, .. } => *utf16, // Compute literal width once while parsing.
                TemplateToken::Capture(slot) => {
                    // The fixed template maps slots to captures without allocating a per-match vector.
                    let capture = self.template.captures[*slot];
                    found.captures[capture].as_ref().map_or(9, Range::len)
                }
            };
            inserted = inserted
                .checked_add(units)
                .ok_or("替换内容超过 UTF-16 长度范围")?;
        }
        self.inserted = self
            .inserted
            .checked_add(inserted)
            .ok_or("替换内容超过 UTF-16 长度范围")?;
        self.count = self.count.checked_add(1).ok_or("替换次数超过有效范围")?;
        Ok(())
    }

    pub(super) fn inserted_utf16(&self) -> usize {
        self.inserted
    }
    pub(super) fn count(&self) -> usize {
        self.count
    }

    /// Reuse one capture scratch for this template. Mapping retains no complete
    /// match list or per-match delta array.
    pub(super) fn map_selection(
        &mut self,
        before: MarkdownTextSelection,
    ) -> Result<MarkdownTextSelection, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法重置替换事务的读取位置")?;
        // Seek flushed the writer. This reader borrows the underlying File only
        // while it is read-only, never bypassing pending buffered writes.
        let mut reader = BufReader::with_capacity(IO_BYTES, self.file.get_mut());
        let mut captures = vec![None; self.template.captures.len()];
        let mut after = before;
        let mut delta = 0isize;
        for _ in 0..self.count {
            let range =
                read_record(&mut reader, &mut captures).map_err(|_| "无法读取替换后的选区映射")?;
            let inserted = self.template.inserted_utf16(&captures)?;
            let removed = range
                .end
                .checked_sub(range.start)
                .ok_or("替换后的选区超过有效范围")?;
            let change = isize::try_from(inserted)
                .ok()
                .and_then(|inserted| {
                    isize::try_from(removed)
                        .ok()
                        .and_then(|removed| inserted.checked_sub(removed))
                })
                .ok_or("替换后的选区超过有效范围")?;
            for (original, mapped) in [
                (before.anchor_utf16, &mut after.anchor_utf16),
                (before.head_utf16, &mut after.head_utf16),
            ] {
                if original >= range.start {
                    // A position inside replacement follows the inserted fragment's
                    // end; later positions follow cumulative length changes.
                    let base = if original <= range.end {
                        range.start.checked_add(inserted)
                    } else {
                        original.checked_add_signed(change)
                    }
                    .ok_or("替换后的选区超过有效范围")?;
                    *mapped = base
                        .checked_add_signed(delta)
                        .ok_or("替换后的选区超过有效范围")?;
                }
            }
            delta = delta
                .checked_add(change)
                .ok_or("替换后的选区超过有效范围")?;
        }
        Ok(after)
    }

    /// The independent template owner outlives the returned reader. This needs
    /// neither a self-referential reader nor an unsafe lifetime extension.
    pub(super) fn into_replacements(mut self) -> Result<SpoolReplacements<'a>, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法重置替换事务的读取位置")?;
        let file = self.file.into_inner().map_err(|_| "无法完成替换事务")?;
        Ok(SpoolReplacements {
            shared: Rc::new(SpoolReader {
                template: self.template,
                state: RefCell::new(ReaderState {
                    file: BufReader::with_capacity(IO_BYTES, file),
                    captures: vec![None; self.template.captures.len()],
                    pending_parts: 0,
                    failed: false,
                }),
            }),
            remaining: self.count,
        })
    }
}

/// One actual File cursor and capture scratch serve the nested iterator. Outstanding
/// parts must finish before another record can overwrite their capture coordinates.
struct ReaderState {
    file: BufReader<File>,
    captures: Vec<Option<Range<usize>>>,
    pending_parts: usize,
    failed: bool,
}
struct SpoolReader<'a> {
    template: &'a ReplacementTemplate,
    state: RefCell<ReaderState>,
}
pub(super) struct SpoolReplacements<'a> {
    shared: Rc<SpoolReader<'a>>,
    remaining: usize,
}
pub(super) struct SpoolParts<'a> {
    shared: Rc<SpoolReader<'a>>,
    next: usize,
}

impl<'a> Iterator for SpoolReplacements<'a> {
    type Item = Result<MarkdownReplacement<SpoolParts<'a>>, MarkdownDocumentBufferError>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut state = self.shared.state.borrow_mut();
        if self.remaining == 0 || state.failed {
            // Completion or prior failure permanently ends this stream; it cannot
            // restart or skip a failed record.
            return None;
        }
        if state.pending_parts != 0 {
            // Unconsumed parts still use this capture scratch; reading another
            // header would overwrite their source coordinates.
            state.failed = true;
            return Some(Err(
                MarkdownDocumentBufferError::ReplacementInputUnavailable,
            ));
        }
        let ReaderState { file, captures, .. } = &mut *state;
        match read_record(file, captures) {
            Ok(range_utf16) => {
                // Publish only after the entire header/capture record is read;
                // release the state borrow before Core consumes the replacement.
                self.remaining -= 1;
                state.pending_parts = self.shared.template.tokens.len();
                Some(Ok(MarkdownReplacement {
                    range_utf16,
                    parts: SpoolParts {
                        shared: Rc::clone(&self.shared),
                        next: 0,
                    },
                }))
            }
            Err(error) => {
                // Short/corrupt input fails the whole canonical transaction;
                // the consumed prefix cannot become a successful partial edit.
                state.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl<'a> Iterator for SpoolParts<'a> {
    type Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>;
    fn next(&mut self) -> Option<Self::Item> {
        let template: &'a ReplacementTemplate = self.shared.template;
        let token = template.tokens.get(self.next)?;
        let mut state = self.shared.state.borrow_mut();
        if state.failed {
            return None; // Outer advancement before part completion revoked this old part stream.
        }
        self.next += 1;
        state.pending_parts -= 1;
        Some(Ok(match token {
            TemplateToken::Literal { bytes, .. } => {
                // Borrow the outer template, not RefCell-owned storage, without
                // allocating a String or copying the shared literal.
                MarkdownReplacementPart::Literal(Cow::Borrowed(&template.arena[bytes.clone()]))
            }
            TemplateToken::Capture(slot) => match &state.captures[*slot] {
                Some(range) => MarkdownReplacementPart::SourceRange(range.clone()), // Core reads original text from its old root.
                None => MarkdownReplacementPart::Literal(Cow::Borrowed("undefined")), // Preserve existing absent-capture semantics.
            },
        }))
    }
}

impl Drop for SpoolParts<'_> {
    fn drop(&mut self) {
        if self.next != self.shared.template.tokens.len() {
            // Dropping parts early permanently seals the reader; remaining capture
            // data cannot be reinterpreted as the next record.
            self.shared.state.borrow_mut().failed = true;
        }
    }
}

/// Decode one fixed-width wire coordinate, rejecting truncated input or a value
/// the target platform cannot represent before it reaches Core.
fn read_usize(reader: &mut impl Read) -> Result<usize, MarkdownDocumentBufferError> {
    let mut bytes = [0; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)?;
    usize::try_from(u64::from_le_bytes(bytes))
        .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)
}

/// The fixed template determines scratch length. Records cannot request a giant
/// allocation or change the following record's layout.
fn read_record(
    reader: &mut impl Read,
    captures: &mut [Option<Range<usize>>],
) -> Result<Range<usize>, MarkdownDocumentBufferError> {
    let range = read_usize(reader)?..read_usize(reader)?;
    for capture in captures {
        let mut tag = [0];
        reader
            .read_exact(&mut tag)
            .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)?;
        *capture = match tag[0] {
            0 => None, // A valid nonparticipating branch has no source coordinates.
            1 => Some(read_usize(reader)?..read_usize(reader)?), // One Core transaction still validates ranges/scalars.
            _ => return Err(MarkdownDocumentBufferError::ReplacementInputUnavailable), // Corruption is neither ignored nor resynchronized.
        };
    }
    Ok(range)
}

#[cfg(test)]
mod tests;
