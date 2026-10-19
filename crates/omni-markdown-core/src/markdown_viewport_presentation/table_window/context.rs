//! 当前表格窗口的有限underline上下文；未知closing与真正没有匹配严格分开。

use std::ops::Range;

use super::{
    MAX_MARKDOWN_PRESENTATION_DECORATIONS, MarkdownTableWindowError, TABLE_WINDOW_MAX_BYTES,
};
use crate::markdown_viewport_presentation::underline::SanitizedUnderlineScanner;

const MAX_CONTEXT_STEP_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_PAIRS: usize = MAX_MARKDOWN_PRESENTATION_DECORATIONS / 3;

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
/// 真实UTF8 byte坐标；必须由document source adapter映射成UTF16后才能传给表格投影/原生属性。
pub(crate) struct MarkdownTableUnderlineBytePair {
    pub open: Range<usize>,
    pub content: Range<usize>,
    pub close: Range<usize>,
}

pub(crate) struct MarkdownTableContextFrame<'a> {
    pub generation: u64,
    pub byte_start: usize,
    pub bytes: &'a [u8],
    pub real_eof: bool,
}

impl std::fmt::Debug for MarkdownTableContextFrame<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 诊断只报告源坐标与计数，不输出借用的用户正文。
        formatter
            .debug_struct("MarkdownTableContextFrame")
            .field("generation", &self.generation)
            .field("byte_start", &self.byte_start)
            .field("byte_count", &self.bytes.len())
            .field("real_eof", &self.real_eof)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MarkdownTableContextStep {
    pub consumed: usize,
    pub utf8_validation_bytes: usize,
    pub ready: bool,
}

#[derive(Debug)]
/// 一个请求最多保header与连续body两段coverage；不创建全源tag目录或保存源正文。
pub(crate) struct MarkdownTableUnderlineContext {
    generation: u64,
    source_end: usize,
    coverage: [Range<usize>; 2],
    coverage_len: usize,
    scanner: SanitizedUnderlineScanner,
    #[cfg(test)]
    pairs: Vec<MarkdownTableUnderlineBytePair>,
    utf16_pairs: Vec<super::MarkdownTableUnderline>,
    utf16_next: usize,
    open_utf16: Option<usize>,
    unicode: [u8; 4],
    unicode_len: usize,
    ready: bool,
    failed: bool,
}

impl MarkdownTableUnderlineContext {
    /// 只接同一个当前窗口的真实源coverage；document/root身份由拥有本请求的producer另行约束。
    pub(crate) fn new(
        generation: u64,
        source_end: usize,
        coverage: &[Range<usize>],
    ) -> Result<Self, MarkdownTableWindowError> {
        if coverage.len() > 2 {
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        let mut total = 0_usize;
        let mut previous_end = 0;
        for range in coverage {
            if range.start >= range.end || range.end > source_end || range.start < previous_end {
                // 不接受反序、重叠或越界coverage，避免同一源段重复计数掩盖预算。
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            total = total.saturating_add(range.end - range.start);
            if total > TABLE_WINDOW_MAX_BYTES {
                return Err(MarkdownTableWindowError::SourceLimit);
            }
            previous_end = range.end;
        }
        let mut ranges = [0..0, 0..0];
        ranges[..coverage.len()].clone_from_slice(coverage);
        Ok(Self {
            generation,
            source_end,
            coverage: ranges,
            coverage_len: coverage.len(),
            scanner: SanitizedUnderlineScanner::default(),
            #[cfg(test)]
            pairs: Vec::new(),
            utf16_pairs: Vec::new(),
            utf16_next: 0,
            open_utf16: None,
            unicode: [0; 4],
            unicode_len: 0,
            ready: coverage.is_empty(),
            failed: false,
        })
    }

    pub(crate) fn next_byte(&self) -> usize {
        self.scanner.next_byte()
    }

    /// None表示真实配对尚未决定；只有Ready才可以把空slice解释为本窗口无underline。
    #[cfg(test)]
    pub(crate) fn ready_byte_pairs(&self) -> Option<&[MarkdownTableUnderlineBytePair]> {
        (self.ready && !self.failed).then_some(self.pairs.as_slice())
    }

    #[cfg(feature = "s2-table-window-prototype")]
    /// 只有Ready可移交有限UTF16值；byte测试目录不进入产品，整批无需再查询piece leaf前缀。
    pub(crate) fn take_ready_utf16_pairs(&mut self) -> Option<Vec<super::MarkdownTableUnderline>> {
        (self.ready && !self.failed).then(|| std::mem::take(&mut self.utf16_pairs))
    }

    pub(crate) fn advance(
        &mut self,
        frame: MarkdownTableContextFrame<'_>,
        budget: usize,
    ) -> Result<MarkdownTableContextStep, MarkdownTableWindowError> {
        let frame_end = frame.byte_start.checked_add(frame.bytes.len());
        if self.failed {
            return Err(MarkdownTableWindowError::SemanticLimit);
        }
        if budget == 0
            || budget > MAX_CONTEXT_STEP_BYTES
            || frame.bytes.len() > MAX_CONTEXT_STEP_BYTES
        {
            return Err(MarkdownTableWindowError::SourceLimit);
        }
        if frame.generation != self.generation
            || frame.byte_start != self.next_byte()
            || !frame_end.is_some_and(|end| end <= self.source_end)
            || (frame.real_eof && frame_end != Some(self.source_end))
        {
            // 所有来源/EOF检查先于状态写入，旧frame不能推进新代次的请求。
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if self.ready {
            return Ok(MarkdownTableContextStep {
                consumed: 0,
                utf8_validation_bytes: 0,
                ready: true,
            });
        }
        let coverage_end = self.coverage[self.coverage_len - 1].end;
        let mut consumed = 0;
        let mut utf8_validation_bytes = 0;
        for byte in frame.bytes.iter().take(budget) {
            // UTF16位置和原ASCII配对同步推进；每个scalar最多4B固定scratch，不再逐pair重扫leaf。
            let mut units = 0;
            if self.unicode_len == 0 && byte.is_ascii() {
                units = 1;
                utf8_validation_bytes += 1;
            } else {
                if self.unicode_len == 4 {
                    self.failed = true;
                    return Err(MarkdownTableWindowError::InvalidSource);
                }
                self.unicode[self.unicode_len] = *byte;
                self.unicode_len += 1;
                utf8_validation_bytes += self.unicode_len;
                match std::str::from_utf8(&self.unicode[..self.unicode_len]) {
                    Ok(text) => {
                        units = text.encode_utf16().count();
                        self.unicode_len = 0;
                    }
                    Err(error) if error.error_len().is_none() && self.unicode_len < 4 => {}
                    Err(_) => {
                        self.failed = true;
                        return Err(MarkdownTableWindowError::InvalidSource);
                    }
                }
            }
            self.utf16_next += units;
            let was_open = self.scanner.has_open();
            let pair = self.scanner.push(*byte);
            if !was_open && self.scanner.has_open() {
                // ASCII opening完成时UTF16位置已确定，后面任意长Unicode内容不会改变此锚点。
                self.open_utf16 = Some(self.utf16_next - 3);
            }
            consumed += 1;
            if let Some(pair) = pair {
                let intersects = self.coverage[..self.coverage_len]
                    .iter()
                    .any(|range| pair.open_start < range.end && pair.end > range.start);
                if intersects {
                    if self.utf16_pairs.len() == MAX_CONTEXT_PAIRS {
                        // 所有push共用同一分配前限额；拒绝整批，不留下可发布的部分underline。
                        self.failed = true;
                        return Err(MarkdownTableWindowError::SemanticLimit);
                    }
                    let open = self
                        .open_utf16
                        .ok_or(MarkdownTableWindowError::InvalidSource)?;
                    let close = self.utf16_next - 4;
                    self.utf16_pairs.push(super::MarkdownTableUnderline {
                        open: open..open + 3,
                        content: open + 3..close,
                        close: close..self.utf16_next,
                    });
                    #[cfg(test)]
                    self.pairs.push(MarkdownTableUnderlineBytePair {
                        open: pair.open_start..pair.content_start,
                        content: pair.content_start..pair.close_start,
                        close: pair.close_start..pair.end,
                    });
                }
                self.open_utf16 = None;
            }
            if self.unicode_len == 0 && self.scanner.settled_after(coverage_end) {
                // 后续opening已不可能覆盖请求；不为了全局目录继续扫描无关源尾。
                self.ready = true;
                return Ok(MarkdownTableContextStep {
                    consumed,
                    utf8_validation_bytes,
                    ready: true,
                });
            }
        }
        if consumed == frame.bytes.len() && frame.real_eof {
            // 未闭合opening在真实EOF保持literal；短frame不能走这个“无匹配”完成出口。
            if self.unicode_len != 0 {
                self.failed = true;
                return Err(MarkdownTableWindowError::InvalidSource);
            }
            self.ready = true;
        }
        Ok(MarkdownTableContextStep {
            consumed,
            utf8_validation_bytes,
            ready: self.ready,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original(source: &str, coverage: &[Range<usize>]) -> Vec<MarkdownTableUnderlineBytePair> {
        let mut result = Vec::new();
        let mut search = 0;
        while let Some(relative) = source[search..].find("<u>") {
            let open = search + relative;
            let content = open + 3;
            let Some(relative) = source[content..].find("</u>") else {
                break;
            };
            let close = content + relative;
            let end = close + 4;
            if coverage
                .iter()
                .any(|range| open < range.end && end > range.start)
            {
                result.push(MarkdownTableUnderlineBytePair {
                    open: open..content,
                    content: content..close,
                    close: close..end,
                });
            }
            search = end;
        }
        result
    }

    fn finish(context: &mut MarkdownTableUnderlineContext, source: &str, chunk: usize) {
        while context.ready_byte_pairs().is_none() {
            let start = context.next_byte();
            let end = (start + chunk).min(source.len());
            let step = context
                .advance(
                    MarkdownTableContextFrame {
                        generation: 7,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    chunk,
                )
                .unwrap();
            assert!(step.ready || step.consumed > 0);
        }
    }

    #[test]
    fn real_source_pairing_and_coverage_match_original_despite_omitted_gap() {
        for source in [
            "<u>header\nomitted <u>row\nvisible</u>tail",
            "中😀<u></u><u>two</u>after",
            "<u>first</u>gap<u>second</u>",
            "plain<u>unmatched",
        ] {
            let coverage = [0..5, source.len() - 5..source.len()];
            let expected = original(source, &coverage);
            for chunk in 1..=source.len() {
                let mut context =
                    MarkdownTableUnderlineContext::new(7, source.len(), &coverage).unwrap();
                finish(&mut context, source, chunk);
                assert_eq!(
                    context.ready_byte_pairs(),
                    Some(expected.as_slice()),
                    "chunk={chunk}"
                );
            }
        }
    }

    #[test]
    fn unknown_close_needs_true_eof_and_invalid_frame_has_no_authority() {
        let source = "<u>selected and unclosed";
        let coverage = 3..5;
        let mut context =
            MarkdownTableUnderlineContext::new(7, source.len(), std::slice::from_ref(&coverage))
                .unwrap();
        let step = context
            .advance(
                MarkdownTableContextFrame {
                    generation: 7,
                    byte_start: 0,
                    bytes: source.as_bytes(),
                    real_eof: false,
                },
                source.len(),
            )
            .unwrap();
        assert!(!step.ready);
        assert!(context.ready_byte_pairs().is_none());
        assert_eq!(
            context.advance(
                MarkdownTableContextFrame {
                    generation: 8,
                    byte_start: source.len(),
                    bytes: &[],
                    real_eof: true,
                },
                1
            ),
            Err(MarkdownTableWindowError::InvalidSource)
        );
        let step = context
            .advance(
                MarkdownTableContextFrame {
                    generation: 7,
                    byte_start: source.len(),
                    bytes: &[],
                    real_eof: true,
                },
                1,
            )
            .unwrap();
        assert!(step.ready);
        assert_eq!(context.ready_byte_pairs(), Some([].as_slice()));
    }

    #[test]
    fn long_crossing_pair_keeps_only_current_pair_metadata_and_releases_it_with_request() {
        let mut source = String::from("<u>");
        source.extend(std::iter::repeat_n('x', 4 * 1024 * 1024));
        source.push_str("</u>tail");
        let coverage = 3..7;
        let mut context =
            MarkdownTableUnderlineContext::new(7, source.len(), std::slice::from_ref(&coverage))
                .unwrap();
        finish(&mut context, &source, 4096);
        assert_eq!(context.pairs.len(), 1);
        assert_eq!(context.pairs[0].content, 3..source.len() - 8);
        assert!(context.next_byte() < source.len());
        assert!(context.pairs.capacity() < 16);
    }

    #[test]
    fn metadata_limit_is_checked_before_next_pair_push() {
        let source = "<u></u>".repeat(MAX_CONTEXT_PAIRS + 1);
        let coverage = 0..source.len();
        let mut context =
            MarkdownTableUnderlineContext::new(7, source.len(), std::slice::from_ref(&coverage))
                .unwrap();
        assert_eq!(
            context.advance(
                MarkdownTableContextFrame {
                    generation: 7,
                    byte_start: 0,
                    bytes: source.as_bytes(),
                    real_eof: true,
                },
                MAX_CONTEXT_STEP_BYTES
            ),
            Err(MarkdownTableWindowError::SemanticLimit)
        );
        assert_eq!(context.pairs.len(), MAX_CONTEXT_PAIRS);
        assert!(context.ready_byte_pairs().is_none());
    }
}
