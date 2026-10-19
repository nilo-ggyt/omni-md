//! 有限原piece借用和至多一个UTF8标量残余；cluster永远使用原全局UTF16位置。
use std::ops::Range;

use harfrust::Buffer;

use super::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutTextSource,
    MarkdownShapingBudget, MarkdownShapingError as Error, MarkdownShapingJob,
    MarkdownShapingStatus as Status, MarkdownShapingStep, Result,
};

pub(super) struct Input {
    pub byte: usize,
    pub utf16: usize,
    pub scalars: usize,
    pending: [u8; 4],
    used: usize,
    needed: usize,
}

impl Input {
    pub(super) fn new(byte: usize, utf16: usize) -> Self {
        Self {
            byte,
            utf16,
            scalars: 0,
            pending: [0; 4],
            used: 0,
            needed: 0,
        }
    }

    fn push(&mut self, byte: u8, buffer: &mut Buffer) -> Result<bool> {
        if self.used == 0 {
            self.needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(Error::InvalidUtf8),
            }; // 只记真实单标量长度；不把半个UTF8片丢掉后重新解释下一页。
        }
        self.pending[self.used] = byte;
        self.used += 1;
        self.byte = self.byte.checked_add(1).ok_or(Error::CoordinateOverflow)?;
        if self.used < self.needed {
            return Ok(false); // 真实残余仍占当前step的byte预算，不提前添加半个字符。
        }
        let scalar = std::str::from_utf8(&self.pending[..self.used])
            .ok()
            .and_then(|value| value.chars().next())
            .ok_or(Error::InvalidUtf8)?;
        let cluster = u32::try_from(self.utf16).map_err(|_| Error::CoordinateOverflow)?;
        buffer.push(u32::from(scalar), cluster);
        if !buffer.allocation_successful() {
            return Err(Error::ShaperStorage); // 分页写失败后不能继续输入或声称前缀完整。
        }
        self.utf16 = self
            .utf16
            .checked_add(scalar.len_utf16())
            .ok_or(Error::CoordinateOverflow)?;
        self.scalars = self
            .scalars
            .checked_add(1)
            .ok_or(Error::CoordinateOverflow)?;
        self.used = 0;
        Ok(true)
    }
}

impl MarkdownShapingJob {
    pub(super) fn step_input(
        &mut self,
        budget: MarkdownShapingBudget,
    ) -> Result<MarkdownShapingStep> {
        let cancellation = self.cancellation.clone();
        let payload = self.payload.as_mut().ok_or(Error::Poisoned)?;
        // 每byte最多完成一个scalar；同时取两个余额的最小值，硬保证三个范围上限。
        let count = MARKDOWN_LAYOUT_SOURCE_BYTES
            .min(budget.bytes)
            .min(budget.scalars)
            .min(self.source_bytes.end - self.input.byte);
        let mut failure = None;
        let mut step = MarkdownShapingStep {
            status: Status::Ingesting,
            bytes: 0,
            scalars: 0,
        };
        let visit =
            payload
                .source
                .visit_byte_range(self.input.byte..self.input.byte + count, |piece| {
                    self.source_borrow_max = self.source_borrow_max.max(piece.len());
                    for (index, &byte) in piece.iter().enumerate() {
                        if index.is_multiple_of(128) && cancellation.requested() {
                            failure = Some(Error::Cancelled);
                            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                        } // 实际输入循环有界观察；lease仍先真实回收，回收失败不能被取消遮掉。
                        // 计入实际解引用字节；即使本次随后失败也不能把已做工作抹成0。
                        step.bytes += 1;
                        self.source_bytes_read += 1;
                        match self.input.push(byte, &mut payload.buffer) {
                            Ok(true) => step.scalars += 1,
                            Ok(false) => {} // 一个标量仍跨边界等待，保持同一个decoder。
                            Err(error) => {
                                failure = Some(error);
                                // visit的错误类型属于源；原始字形错误保存在本地并在返回后复原。
                                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                            }
                        }
                    }
                    Ok(())
                });
        let visit = match visit {
            Err(error) if !matches!(error, MarkdownDocumentBufferError::RangeOutsideDocument) => {
                return Err(error.into()); // 原lease真实回收失败优先于callback内部的取消占位错误。
            }
            result => result,
        };
        if let Some(error) = failure {
            return Err(error);
        }
        visit?;
        if self.input.byte == self.source_bytes.end {
            if self.input.used != 0 || self.input.utf16 != self.source_utf16.end {
                return Err(Error::InvalidUtf8); // 只有真实run尾且编码和坐标都完整才能shape。
            }
            self.status = Status::AwaitingShape;
            step.status = self.status;
        }
        if !payload.buffer.paged_checkpoint() || !payload.buffer.allocation_successful() {
            return Err(Error::ShaperStorage); // 同一step真实回写/撤驻留失败永久撤权。
        }
        Ok(step)
    }
}

/// shaper按上游合同最多消费邻接5个scalar；只借用20字节固定栈缓冲，不创建段落String。
pub(super) fn install_context(
    source: &MarkdownLayoutTextSource,
    range: Range<usize>,
    buffer: &mut Buffer,
) -> Result<(usize, usize)> {
    let before = range.start.saturating_sub(20)..range.start;
    let after = range.end..range.end.saturating_add(20).min(source.byte_len());
    let mut read = 0;
    let mut maximum = 0;
    for (range, preceding) in [(before, true), (after, false)] {
        let mut bytes = [0u8; 20];
        let mut used = 0;
        source.visit_byte_range(range, |piece| {
            // 源回调累计长度已由20byte请求固定，真实copy不越过栈数组。
            bytes[used..used + piece.len()].copy_from_slice(piece);
            used += piece.len();
            maximum = maximum.max(piece.len());
            Ok(())
        })?;
        read += used;
        let (context, count) = decode_context(&bytes[..used], preceding)?;
        if preceding {
            buffer.set_pre_context_codepoints(&context[..count]);
        } else {
            buffer.set_post_context_codepoints(&context[..count]);
        } // 原同步入口和已收费上下文共用同一个纯解码/逆序合同。
    }
    Ok((read, maximum))
}

/// 只解释已经取得的至多20byte；原邻接范围、UTF8边缘与五标量次序不因读取方式改变。
pub(super) fn decode_context(bytes: &[u8], preceding: bool) -> Result<([u32; 5], usize)> {
    let mut start = 0;
    if preceding {
        // 固定前窗可从UTF8中部开始，只跳过开头的continuation；真实run首端仍是标量边界。
        while start < bytes.len() && bytes[start] & 0xc0 == 0x80 {
            start += 1;
        }
    }
    let text = match std::str::from_utf8(&bytes[start..]) {
        Ok(text) => text,
        Err(error) if !preceding && error.error_len().is_none() => {
            // 后窗末端允许不完整UTF8，仅交付之前真实完整的邻接标量。
            std::str::from_utf8(&bytes[start..start + error.valid_up_to()])
                .map_err(|_| Error::InvalidUtf8)?
        }
        Err(_) => return Err(Error::InvalidUtf8),
    };
    let mut context = [0u32; 5];
    let mut count = 0;
    if preceding {
        // shaper前上下文以最近前驱在前；不能把源的逻辑顺序直接传入。
        for scalar in text.chars().rev().take(5) {
            context[count] = u32::from(scalar);
            count += 1;
        }
    } else {
        for scalar in text.chars().take(5) {
            context[count] = u32::from(scalar);
            count += 1;
        }
    }
    Ok((context, count))
}

pub(super) struct PreparedContext {
    pub(super) before: ([u32; 5], usize),
    pub(super) after: ([u32; 5], usize),
    pub(super) bytes: usize,
    pub(super) maximum_borrow: usize,
}

impl PreparedContext {
    pub(super) fn install(&self, buffer: &mut Buffer) -> (usize, usize) {
        buffer.set_pre_context_codepoints(&self.before.0[..self.before.1]);
        buffer.set_post_context_codepoints(&self.after.0[..self.after.1]);
        (self.bytes, self.maximum_borrow) // 只装配已按原配额读取的值，不在构造中重借Source。
    }
}
