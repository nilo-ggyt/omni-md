//! 同一冻结 Source 的显式折叠段落证明；只保有限端点，未读隐区不产生 Unicode 或样式权限。

use std::{ops::Range, sync::Arc};

use super::{MarkdownLayoutTextAuthority, MarkdownLayoutTextSource};
use crate::{MarkdownDocumentBufferError, MarkdownGraphemeBoundary};

/// 每个位置仍为原完整 Source 的 byte/UTF16 坐标，不是删掉隐区后的投影坐标。
#[derive(Clone, Copy, Debug)]
pub(crate) struct FoldSpan {
    pub(crate) start: MarkdownGraphemeBoundary,
    pub(crate) header_end: MarkdownGraphemeBoundary,
    pub(crate) end: MarkdownGraphemeBoundary,
    /// 末尾真实 CR/LF 的起点；无末尾分隔符的真实 EOF 则等于 end。
    pub(crate) tail_start: MarkdownGraphemeBoundary,
    pub(crate) eof: bool,
}

/// 各 Unicode job 只共享这份固定上限的证明，独立游标不复制范围目录或保活正文。
#[derive(Clone)]
pub(crate) struct FoldPlan {
    authority: MarkdownLayoutTextAuthority,
    spans: Arc<[FoldSpan]>,
}

impl FoldPlan {
    /// 原 composition root 已核锚点数量；这里排序联合并证明真实首尾 CRLF，不扫隐藏行体。
    /// 构造读取的固定端点属于显式显示配置，不把跳过的 byte 记成扫描或全源已分析。
    pub(crate) fn new(
        source: &MarkdownLayoutTextSource,
        ranges: &[Range<usize>],
    ) -> Result<Self, MarkdownDocumentBufferError> {
        source.health()?;
        let snapshot = source.snapshot_for_bidi();
        let length = source.utf16_len();
        let mut ordered = ranges.to_vec();
        ordered.sort_unstable_by_key(|range| (range.start, range.end));
        let mut kept = 0usize;
        for index in 0..ordered.len() {
            let range = ordered[index].clone();
            if range.start == 0 || range.start >= range.end || range.end > length {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            if kept != 0 && ordered[kept - 1].end >= range.start {
                ordered[kept - 1].end = ordered[kept - 1].end.max(range.end);
            } else {
                ordered[kept] = range;
                kept += 1;
            }
        }
        ordered.truncate(kept);
        let mut spans = Vec::with_capacity(kept);
        for hidden in ordered {
            let first = snapshot.utf16_code_unit_at(hidden.start)?;
            if !matches!(first, 10 | 13)
                || first == 10 && snapshot.utf16_code_unit_at(hidden.start - 1)? == 13
            {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            // GB3 与 LB5 需要完整 CRLF；不能把内部 LF 或 CR 后的半个终止符签为段边界。
            let mut header_end = hidden.start + 1;
            if first == 13 && header_end < length && snapshot.utf16_code_unit_at(header_end)? == 10
            {
                header_end += 1;
            }
            if header_end > hidden.end {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            let last = snapshot.utf16_code_unit_at(hidden.end - 1)?;
            let tail_start = if last == 10 {
                if hidden.end >= 2 && snapshot.utf16_code_unit_at(hidden.end - 2)? == 13 {
                    hidden.end - 2
                } else {
                    hidden.end - 1
                }
            } else if last == 13 {
                if hidden.end < length && snapshot.utf16_code_unit_at(hidden.end)? == 10 {
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                hidden.end - 1
            } else if hidden.end == length {
                hidden.end // 真实 EOF 的 GB2/LB3 不依赖未读取隐区的文本后缀。
            } else {
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            };
            let position =
                |utf16| -> Result<MarkdownGraphemeBoundary, MarkdownDocumentBufferError> {
                    Ok(MarkdownGraphemeBoundary {
                        byte: snapshot.byte_offset_for_utf16(utf16)?,
                        utf16,
                    })
                };
            spans.push(FoldSpan {
                start: position(hidden.start)?,
                header_end: position(header_end)?,
                end: position(hidden.end)?,
                tail_start: position(tail_start)?,
                eof: hidden.end == length,
            });
        }
        source.health()?;
        Ok(Self {
            authority: source.authority(),
            spans: spans.into(),
        })
    }

    /// 只比较原 capsule 的弱身份；实际 Source/取消/I/O 检查仍由每个调用 job 的原 guard 执行。
    pub(crate) fn authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.authority
    }

    /// 固定上限的只读段表；不同 job 仅保各自 next 下标，不保第二正文或整源目录。
    pub(crate) fn spans(&self) -> &[FoldSpan] {
        &self.spans
    }

    /// 内部位置不授可见 Unicode 权限；首尾仍必须由原规则各自证明实际 GCB/换段。
    pub(crate) fn hides(&self, position: usize, utf16: bool) -> bool {
        let index = self.spans.partition_point(|span| {
            if utf16 {
                span.start.utf16 < position
            } else {
                span.start.byte < position
            }
        });
        index != 0
            && position
                < if utf16 {
                    self.spans[index - 1].end.utf16
                } else {
                    self.spans[index - 1].end.byte
                }
    }
}
