//! One fixed 4-KiB source window and one scalar carry; every actual source lease ends before any await can be reached.
use super::*;
use cooperate::Gate;
use std::rc::Rc;

pub(in crate::markdown_bidi) struct Reader<'a> {
    snapshot: &'a MarkdownTextSnapshot,
    position: usize,
    end: usize,
    buffer: [u8; 4096],
    from: usize,
    available: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
    scalar_start: usize,
}
impl<'a> Reader<'a> {
    pub(in crate::markdown_bidi) fn new(
        snapshot: &'a MarkdownTextSnapshot,
        range: Range<usize>,
    ) -> Self {
        // Borrow the original immutable snapshot and requested byte range, retaining neither another source capture nor a whole paragraph copy.
        Self {
            snapshot,
            position: range.start,
            end: range.end,
            buffer: [0; 4096],
            from: 0,
            available: 0,
            pending: [0; 4],
            used: 0,
            needed: 0,
            scalar_start: range.start,
        }
    }
    pub(in crate::markdown_bidi) fn position(&self) -> usize {
        self.position
    }
    pub(in crate::markdown_bidi) async fn next(
        &mut self,
        gate: &Rc<Gate>,
    ) -> Result<Option<(char, usize)>, BidiError> {
        gate.health()?;
        loop {
            if self.from == self.available {
                if self.position == self.end {
                    if self.used != 0 {
                        return Err(BidiError::CorruptMetadata);
                    }
                    return Ok(None); // Only the original actual byte endpoint ends input; a quota or buffer tail is not EOF.
                }
                let count = gate.source(self.end - self.position).await?;
                // Copy only the admitted bounded window, then return all actual piece leases before suspending on further source/scalar work.
                let mut copied = 0;
                let result =
                    self.snapshot
                        .visit_byte_range(self.position..self.position + count, |piece| {
                            self.buffer[copied..copied + piece.len()].copy_from_slice(piece);
                            copied += piece.len();
                            gate.bytes_read(piece.len());
                            gate.maximum_borrow(piece.len());
                            Ok(())
                        });
                self.snapshot.original_backing_health()?;
                result?; // A failed source-lease return precedes cancellation; publish no new window and retain no borrowed reference on error.
                gate.health()?;
                if copied != count || copied == 0 {
                    return Err(BidiError::CorruptMetadata);
                }
                self.from = 0;
                self.available = copied;
            }
            let byte = self.buffer[self.from];
            if self.used == 0 {
                // Carry incomplete UTF8 bytes between windows; a new window never resets a partially consumed scalar.
                self.scalar_start = self.position;
                self.needed = match byte {
                    0..=0x7f => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(BidiError::CorruptMetadata),
                };
            }
            self.pending[self.used] = byte;
            self.used += 1;
            self.from += 1;
            self.position += 1;
            if self.used == self.needed {
                let scalar = std::str::from_utf8(&self.pending[..self.used])
                    .ok()
                    .and_then(|value| value.chars().next())
                    .ok_or(BidiError::CorruptMetadata)?;
                // The future retains this actual scalar and decoder position; resumption does not decode the window again from its beginning.
                gate.scalar().await?;
                gate.decoded_scalar();
                self.used = 0;
                return Ok(Some((scalar, self.scalar_start)));
            }
        }
    }
}
