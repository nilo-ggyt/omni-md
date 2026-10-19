//! 消费原协调器的小闭项票；端点不重新定位，实际整形仍只使用原Job装配路径。

use super::*;
use crate::markdown_line_composer::ClosedItem;

struct Payload {
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    utf16: Range<usize>,
    bytes: Range<usize>,
    options: options::ValidatedOptions,
    cancellation: MarkdownShapingCancellation,
}

/// 原Style票只移入这一个续体；queue时不创建Buffer，也不读取任何邻接Source。
pub(crate) struct PendingClosedShaping {
    payload: Option<Payload>,
    context: ContextInput,
}

pub(crate) struct ClosedShapingStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) job: Option<MarkdownShapingJob>,
}

impl PendingClosedShaping {
    /// 消费原Style scope时就转交权限；旧probe立即撤销，后续只由唯一owner续进。
    pub(crate) fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        closed: ClosedItem,
        options: MarkdownShapingOptions,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        } // 弱Source票仍须由本次同一冻结payload支持，不能凭坐标重新捕获正文。
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        } // 原Style选定的真实Font身份不由名字、face编号或相同轴值替代。
        let (utf16, bytes, cancellation) = closed.into_input(authority, font_authority)?;
        if cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        } // 已取消时不创建Buffer或借邻接上下文；源首错在此之前已经核验。
        let validated = options::ValidatedOptions::new(options, &font)?;
        let context = ContextInput::new(&bytes, source.byte_len());
        Ok(Self {
            payload: Some(Payload {
                source,
                authority: authority.clone(),
                font,
                font_authority: font_authority.clone(),
                utf16,
                bytes,
                options: validated,
                cancellation,
            }),
            context,
        }) // 两张原GCB票已在原scope存活时消费；没有新的公开坐标或可替换取消位。
    }

    fn guard(&self) -> Result<()> {
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        payload.source.health()?;
        if !payload.source.permits(&payload.authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        }
        if !payload.font.permits(&payload.font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        }
        if payload.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        } // 每个零工作或Source读取回合都核原位；不能靠已装入的坐标缓存绕过撤权。
        Ok(())
    }

    pub(crate) fn step(&mut self, budget: MarkdownShapingBudget) -> Result<ClosedShapingStep> {
        let result = self.advance(budget);
        if result.is_err() {
            self.payload.take();
        } // 失败立即归还原Source/Font；外层旧续体不能重新装配较新文档。
        result
    }

    fn advance(&mut self, budget: MarkdownShapingBudget) -> Result<ClosedShapingStep> {
        self.guard()?;
        if budget.bytes == 0 || budget.scalars == 0 {
            return Err(MarkdownShapingError::InvalidOptions);
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let (bytes, scalars) = self.context.step(&payload.source, budget)?;
        self.guard()?;
        if !self.context.complete() {
            return Ok(ClosedShapingStep {
                bytes,
                scalars,
                job: None,
            });
        } // 同一个UTF8残余在后继量子继续；未完整准备上下文前不创建Job或字形FD。
        let prepared = self.context.prepared()?;
        let payload = self.payload.take().ok_or(MarkdownShapingError::Poisoned)?;
        let mut job = MarkdownShapingJob::assemble_with_context(
            payload.source,
            &payload.authority,
            payload.font,
            &payload.font_authority,
            (payload.utf16, payload.bytes, Some(prepared)),
            payload.options,
            payload.cancellation,
        )?;
        job.guard(&payload.authority, &payload.font_authority)?;
        Ok(ClosedShapingStep {
            bytes,
            scalars,
            job: Some(job),
        })
    }
}

/// 最多两个20byte邻接窗；不另建Reader、正文镜像或按段落长度增长的残余。
struct ContextInput {
    ranges: [Range<usize>; 2],
    raw: [[u8; 20]; 2],
    used: [usize; 2],
    phase: usize,
    partial: [u8; 4],
    partial_used: usize,
    partial_needed: usize,
    total: usize,
    maximum_borrow: usize,
}

impl ContextInput {
    fn new(bytes: &Range<usize>, length: usize) -> Self {
        Self {
            ranges: [
                bytes.start.saturating_sub(20)..bytes.start,
                bytes.end..bytes.end.saturating_add(20).min(length),
            ],
            raw: [[0; 20]; 2],
            used: [0; 2],
            phase: 0,
            partial: [0; 4],
            partial_used: 0,
            partial_needed: 0,
            total: 0,
            maximum_borrow: 0,
        }
    }

    fn finish_ranges(&mut self) {
        while self.phase < 2 && self.used[self.phase] == self.ranges[self.phase].len() {
            self.phase += 1;
            self.partial_used = 0;
            self.partial_needed = 0;
        } // 两个固定窗最多转换两次；后窗UTF8尾残余沿原decode_context合同舍弃，非文档EOF。
    }

    fn scalar(&mut self, byte: u8) -> Result<usize> {
        if self.partial_used == 0 {
            if self.phase == 0 && self.used[0] < 3 && byte & 0xc0 == 0x80 {
                return Ok(0); // 原前窗允许落在UTF8中部，只跳过真实开头至多三个continuation。
            }
            self.partial_needed = match byte {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(MarkdownShapingError::InvalidUtf8),
            };
        }
        self.partial[self.partial_used] = byte;
        self.partial_used += 1;
        if self.partial_used != self.partial_needed {
            return Ok(0); // 没有完整解码的标量不能提前计数；残余仍是同一固定数组。
        }
        std::str::from_utf8(&self.partial[..self.partial_used])
            .map_err(|_| MarkdownShapingError::InvalidUtf8)?;
        self.partial_used = 0;
        Ok(1)
    }

    fn step(
        &mut self,
        source: &MarkdownLayoutTextSource,
        budget: MarkdownShapingBudget,
    ) -> Result<(usize, usize)> {
        self.finish_ranges();
        if self.complete() {
            return Ok((0, 0)); // 实际文档两端无邻接字符，零读即可装配，不能读不存在的EOF。
        }
        let phase = self.phase;
        let at = self.ranges[phase].start + self.used[phase];
        let count = budget
            .bytes
            .min(budget.scalars)
            .min(self.ranges[phase].end - at);
        let mut scalars = 0;
        let mut failure = None;
        source
            .visit_byte_range(at..at + count, |piece| {
                self.maximum_borrow = self.maximum_borrow.max(piece.len());
                for &byte in piece {
                    match self.scalar(byte) {
                        Ok(count) => scalars += count,
                        Err(error) => {
                            failure = Some(error);
                            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                        }
                    }
                    self.raw[phase][self.used[phase]] = byte;
                    self.used[phase] += 1;
                    self.total += 1;
                }
                Ok(())
            })
            .or_else(|error| {
                if matches!(error, MarkdownDocumentBufferError::RangeOutsideDocument)
                    && failure.is_some()
                {
                    Ok(())
                } else {
                    Err(error)
                }
            })?; // 原lease回收失败仍以真实Source错误返回，不被本地解码失败的占位错误遮盖。
        if let Some(error) = failure {
            return Err(error);
        }
        self.finish_ranges();
        Ok((count, scalars)) // byte和完整scalar均在读取的同一回合收费，未同时输入中心文字。
    }

    fn complete(&self) -> bool {
        self.phase == 2
    }

    fn prepared(&self) -> Result<input::PreparedContext> {
        if !self.complete() {
            return Err(MarkdownShapingError::Pending);
        }
        Ok(input::PreparedContext {
            before: input::decode_context(&self.raw[0][..self.used[0]], true)?,
            after: input::decode_context(&self.raw[1][..self.used[1]], false)?,
            bytes: self.total,
            maximum_borrow: self.maximum_borrow,
        }) // 纯固定缓冲解码共用原算法；装配不再解引用原Source，也不重复收取字节。
    }
}
