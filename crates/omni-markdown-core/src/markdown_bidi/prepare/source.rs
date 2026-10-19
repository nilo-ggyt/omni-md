//! 一个固定4KiB源窗口与单scalar残余；真实lease已结束后才能到达任何await。
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
                    return Ok(None); // 只有原真实byte端点才能结束，不把一次预算或缓冲尾部当EOF。
                }
                let count = gate.source(self.end - self.position).await?;
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
                result?; // 源lease归还失败先于取消；这时不发布新窗口，也不保留其引用。
                gate.health()?;
                if copied != count || copied == 0 {
                    return Err(BidiError::CorruptMetadata);
                }
                self.from = 0;
                self.available = copied;
            }
            let byte = self.buffer[self.from];
            if self.used == 0 {
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
                // Future保留这个实际scalar和decoder位置；暂停后不是从窗口头再次解码。
                gate.scalar().await?;
                gate.decoded_scalar();
                self.used = 0;
                return Ok(Some((scalar, self.scalar_start)));
            }
        }
    }
}
