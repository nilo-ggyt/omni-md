// Copyright 2015 Google Inc. All rights reserved.
// 固定0.13.4 metadata opening/closing规则的分段适配；完整MIT许可见LICENSE。

//! YAML/pluses的真实closure前瞻；只保计数/源锚点，不缓存待定整块正文。
use crate::scanners::is_ascii_whitespace_no_nl;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataClosing {
    NeedInput,
    Closed(usize),
    NotClosed,
}

#[derive(Clone, Copy, Debug)]
/// 原closing scanner仅接受恰好3个marker及ASCII space；YAML也允许3个dot。
pub(crate) struct MetadataCloser {
    opening: u8,
    marker: Option<u8>,
    count: usize,
    offset: usize,
    spaces: bool,
    result: Option<MetadataClosing>,
}

impl MetadataCloser {
    pub(crate) fn new(opening: u8) -> Self {
        Self {
            opening,
            marker: None,
            count: 0,
            offset: 0,
            spaces: false,
            result: None,
        }
    }
    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> MetadataClosing {
        if let Some(result) = self.result {
            return result;
        }
        for byte in bytes {
            if self.marker.is_none() {
                if *byte == self.opening || (self.opening == b'-' && *byte == b'.') {
                    self.marker = Some(*byte);
                } else {
                    // 第一个raw byte不匹配；前导indent不能在lookahead中擅自去掉。
                    self.result = Some(MetadataClosing::NotClosed);
                    return MetadataClosing::NotClosed;
                }
            }
            if !self.spaces && Some(*byte) == self.marker {
                self.count += 1;
                self.offset += 1;
                if self.count > 3 {
                    self.result = Some(MetadataClosing::NotClosed);
                    return MetadataClosing::NotClosed;
                }
                continue;
            }
            self.spaces = true;
            let result = if self.count != 3 {
                Some(MetadataClosing::NotClosed)
            } else if *byte == b' ' {
                self.offset += 1;
                None
            } else if matches!(*byte, b'\r' | b'\n') {
                Some(MetadataClosing::Closed(self.offset))
            } else {
                Some(MetadataClosing::NotClosed)
            };
            if let Some(result) = result {
                self.result = Some(result);
                return result;
            }
        }
        if !real_eof {
            return MetadataClosing::NeedInput;
        }
        let result = if self.count == 3 {
            MetadataClosing::Closed(self.offset)
        } else {
            MetadataClosing::NotClosed
        };
        self.result = Some(result);
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataDecision {
    NeedInput,
    /// 失败只撤销opening候选；完整producer须从opening anchor重分类，不发布已经跳过的伪块。
    Rejected,
    Accepted {
        closing_start: usize,
        closing_marker_end: usize,
    },
}

#[derive(Clone, Copy, Debug)]
/// 按原scan_nextline的LF边界前瞻；CR仍可在closing/blank判定中起作用，不能替换其实际规则。
pub(crate) struct MetadataLookahead {
    opening: u8,
    next_byte: usize,
    line_start: usize,
    first_body: bool,
    possible_blank: bool,
    closing: MetadataCloser,
    decision: MetadataDecision,
}

impl MetadataLookahead {
    pub(crate) fn new(opening: u8, body_start: usize) -> Self {
        Self {
            opening,
            next_byte: body_start,
            line_start: body_start,
            first_body: true,
            possible_blank: true,
            closing: MetadataCloser::new(opening),
            decision: MetadataDecision::NeedInput,
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> MetadataDecision {
        if self.decision != MetadataDecision::NeedInput {
            return self.decision;
        }
        for byte in bytes {
            self.next_byte += 1;
            if self.first_body && self.possible_blank {
                if is_ascii_whitespace_no_nl(*byte) {
                    // 首body的blank判断允许tab/VT/FF；closing判断仍只允许ASCII space，两者不能混用。
                } else if matches!(*byte, b'\r' | b'\n') {
                    self.decision = MetadataDecision::Rejected;
                    return self.decision;
                } else {
                    self.possible_blank = false;
                }
            }
            if let MetadataClosing::Closed(end) =
                self.closing.feed(std::slice::from_ref(byte), false)
            {
                self.decision = if self.first_body {
                    MetadataDecision::Rejected
                } else {
                    MetadataDecision::Accepted {
                        closing_start: self.line_start,
                        closing_marker_end: self.line_start + end,
                    }
                };
                return self.decision;
            }
            if *byte == b'\n' {
                // 对不匹配的opaque行只流过字节至LF；无论该行多长，状态大小不变。
                self.first_body = false;
                self.possible_blank = false;
                self.line_start = self.next_byte;
                self.closing = MetadataCloser::new(self.opening);
            }
        }
        if !real_eof {
            return MetadataDecision::NeedInput;
        }
        self.decision = match self.closing.feed(&[], true) {
            MetadataClosing::Closed(end) if !self.first_body => MetadataDecision::Accepted {
                closing_start: self.line_start,
                closing_marker_end: self.line_start + end,
            },
            // 未闭合的真实EOF与首行直接闭合都否定metadata，而不是制造一个EOF结束的metadata块。
            _ => MetadataDecision::Rejected,
        };
        self.decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::{scan_blank_line, scan_ch_repeat, scan_eol, scan_nextline};
    fn original_closing(bytes: &[u8], marker: u8) -> Option<usize> {
        let mut n = scan_ch_repeat(bytes, marker);
        if n != 3 {
            if marker != b'-' {
                return None;
            }
            n = scan_ch_repeat(bytes, b'.');
            if n != 3 {
                return None;
            }
        }
        n += scan_ch_repeat(&bytes[n..], b' ');
        scan_eol(&bytes[n..]).map(|_| n)
    }
    fn original_lookup(bytes: &[u8], marker: u8) -> bool {
        // 同原函数：第一次从opening行末推进到首body；此辅助输入已经位于body起点。
        if original_closing(bytes, marker).is_some() || scan_blank_line(bytes).is_some() {
            return false;
        }
        let mut i = scan_nextline(bytes);
        while i <= bytes.len() {
            if original_closing(&bytes[i..], marker).is_some() {
                return true;
            }
            if i == bytes.len() {
                break;
            }
            i += scan_nextline(&bytes[i..]);
        }
        false
    }
    #[test]
    fn metadata_pending_all_splits_match_original_closing_and_lookup() {
        for body in [
            "",
            "\n---\n",
            "---\n",
            "x\n---\n",
            "x\n...\n",
            "x\n ---\n",
            "x\n----\n",
            "x\n---\t\n",
            "x\n---",
            "x\r---\n",
            "x\n+++\n",
            "x\nnot closed",
            "\tx\n...  \r\ntail",
        ] {
            for marker in [b'-', b'+'] {
                for split in 0..=body.len() {
                    let mut state = MetadataLookahead::new(marker, 37);
                    let before = state.feed(&body.as_bytes()[..split], false);
                    let result = if before == MetadataDecision::NeedInput {
                        state.feed(&body.as_bytes()[split..], true)
                    } else {
                        before
                    };
                    assert_eq!(
                        matches!(result, MetadataDecision::Accepted { .. }),
                        original_lookup(body.as_bytes(), marker),
                        "body={body:?} split={split}"
                    );
                    let mut closer = MetadataCloser::new(marker);
                    let before = closer.feed(&body.as_bytes()[..split], false);
                    let actual = if before == MetadataClosing::NeedInput {
                        closer.feed(&body.as_bytes()[split..], true)
                    } else {
                        before
                    };
                    assert_eq!(
                        actual,
                        original_closing(body.as_bytes(), marker)
                            .map_or(MetadataClosing::NotClosed, MetadataClosing::Closed)
                    );
                }
            }
        }
    }
    #[test]
    fn long_opaque_metadata_line_keeps_pending_anchors_without_body_snapshot() {
        let mut state = MetadataLookahead::new(b'-', 4);
        assert_eq!(state.feed(b"title\n", false), MetadataDecision::NeedInput);
        let content = [b'x'; 4096];
        for _ in 0..1024 {
            assert_eq!(state.feed(&content, false), MetadataDecision::NeedInput);
        }
        assert_eq!(
            state.feed(b"\n---\n| H |\n| - |\n", false),
            MetadataDecision::Accepted {
                closing_start: 4 + 6 + 4096 * 1024 + 1,
                closing_marker_end: 4 + 6 + 4096 * 1024 + 4,
            }
        );
        assert!(std::mem::size_of::<MetadataLookahead>() <= 128);
        // 只有header-looking文本而没有真实closing时，必须拒绝opening并回到其anchor重分类。
        let mut missing = MetadataLookahead::new(b'-', 4);
        assert_eq!(
            missing.feed(b"title\n| H |\n| - |\n", false),
            MetadataDecision::NeedInput
        );
        assert_eq!(missing.feed(&[], true), MetadataDecision::Rejected);
    }
}
