// Copyright 2015 Google Inc. All rights reserved.
// 原始判断顺序提取自pulldown-cmark0.13.4 firstpass.rs；完整MIT条款见LICENSE。

//! 结构producer与原AST builder共享的非metadata叶块起点决策。
#[cfg(test)]
use crate::scanners::*;
use crate::HeadingLevel;

#[derive(Clone, Copy, Debug)]
pub(crate) enum LeafStart {
    HtmlTerminated(&'static str),
    HtmlBlankTerminated,
    Rule(usize),
    Heading(HeadingLevel),
    Fence { marker: u8, count: usize },
    DefinitionOrParagraph,
}

/// 只判断已完整逻辑行的叶块起点；YAML的远处closure权限由外层pending-lookahead承担。
pub(crate) fn non_metadata_leaf_start(bytes: &[u8]) -> LeafStart {
    let mut probe = crate::leaf_stream::LeafProbe::new(0);
    loop {
        // 此连续入口拥有真正source末尾；分段producer必须由来源协议独立授予real_eof。
        let next = probe.next_byte();
        let end = next.saturating_add(64 * 1024).min(bytes.len());
        let (step, _, _) = probe.advance(&bytes[next..end], end == bytes.len(), 64 * 1024);
        if let crate::leaf_stream::LeafProbeStep::Complete(result) = step {
            return result;
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// HTML1–5的terminator匹配只保已匹配的pattern前缀长度；不存opaque正文或整行。
pub(crate) struct HtmlTerminator {
    pattern: &'static [u8],
    matched: usize,
    found: bool,
}

impl HtmlTerminator {
    #[cfg(feature = "omni-block-frontier")]
    pub(crate) fn is_found(&self) -> bool {
        self.found
    }
    pub(crate) fn new(pattern: &'static str) -> Self {
        Self {
            pattern: pattern.as_bytes(),
            matched: 0,
            found: false,
        }
    }

    /// 按真实LF行重置未完成的前缀，不能让分开两行的片段拼成原contains不认可的closing。
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> bool {
        if self.found {
            return true; // 同一个opaque块已经找到closing，不再重复改变其结束权限。
        }
        for byte in bytes {
            if *byte == b'\n' {
                self.matched = 0;
                continue;
            }
            while self.matched > 0 && self.pattern[self.matched] != *byte {
                // pattern来自固定get_html_end_tag，最长11字节；fallback不访问或拷贝源前缀。
                let previous = self.matched;
                self.matched -= 1;
                while self.matched > 0
                    && self.pattern[..self.matched]
                        != self.pattern[previous - self.matched..previous]
                {
                    self.matched -= 1;
                }
            }
            if self.pattern[self.matched] == *byte {
                self.matched += 1;
                if self.matched == self.pattern.len() {
                    self.found = true;
                    return true;
                }
            }
        }
        false
    }
}

/// 原AST builder仍传其一条真实LF行；frontier可传同一行的任意frame切片。
pub(crate) fn html_line_contains_end_tag(bytes: &[u8], end: &'static str) -> bool {
    HtmlTerminator::new(end).feed(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClosingFenceResult {
    NeedInput,
    Closed(usize),
    NotClosed,
}

#[derive(Clone, Copy, Debug)]
enum ClosingPhase {
    Markers,
    Spaces,
}

#[derive(Clone, Copy, Debug)]
/// 一个closing probe只存计数；opaque内容再长也不需要整行String。
pub(crate) struct CodeFenceCloser {
    marker: u8,
    minimum: usize,
    markers: usize,
    offset: usize,
    phase: ClosingPhase,
    result: Option<ClosingFenceResult>,
}

impl CodeFenceCloser {
    pub(crate) fn new(marker: u8, minimum: usize) -> Self {
        Self {
            marker,
            minimum,
            markers: 0,
            offset: 0,
            phase: ClosingPhase::Markers,
            result: None,
        }
    }

    /// frame尾不是EOF。只有遇真实EOL或真实EOF才确认仅含marker+ASCII space的closing行。
    pub(crate) fn feed(&mut self, bytes: &[u8], real_eof: bool) -> ClosingFenceResult {
        if let Some(result) = self.result {
            return result; // 已判定不再重复消耗输入；外层仍负责丢弃opaque余行至真实换行。
        }
        for byte in bytes {
            match self.phase {
                ClosingPhase::Markers if *byte == self.marker => {
                    self.markers += 1;
                    self.offset += 1;
                    continue;
                }
                ClosingPhase::Markers => self.phase = ClosingPhase::Spaces,
                ClosingPhase::Spaces => {}
            }
            let result = if self.markers < self.minimum {
                // 首个非marker之前的run不足，后续内容无法再使本行成为closing。
                Some(ClosingFenceResult::NotClosed)
            } else if *byte == b' ' {
                self.offset += 1;
                None
            } else if matches!(*byte, b'\r' | b'\n') {
                Some(ClosingFenceResult::Closed(self.offset))
            } else {
                // 原扫描器只接受ASCII space，不接受tab；不能借扩展顺手改变语法。
                Some(ClosingFenceResult::NotClosed)
            };
            if let Some(result) = result {
                self.result = Some(result);
                return result;
            }
        }
        if !real_eof {
            return ClosingFenceResult::NeedInput;
        }
        let result = if self.offset == 0 || self.markers >= self.minimum {
            // 原函数对真实空EOF返回Some(0)以关闭未闭合fence；空frame没有这一权限。
            ClosingFenceResult::Closed(self.offset)
        } else {
            ClosingFenceResult::NotClosed
        };
        self.result = Some(result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_terminators_match_per_lf_line_contains_for_every_frame_split() {
        for end in [
            "</pre>",
            "</style>",
            "</script>",
            "</textarea>",
            "-->",
            "?>",
            "]]>",
            ">",
        ] {
            for body in [
                format!("prefix{end}tail"),
                format!("prefix{}{end}tail", &end[..1]),
                format!("{}\n{}", &end[..1], &end[1..]),
                format!("{end}\nignored"),
                "opaque中😀---xx]x?x<//x\r\n".to_string(),
            ] {
                let expected = body.split_inclusive('\n').any(|line| line.contains(end));
                for split in 0..=body.len() {
                    // byte切点可位于UTF8内部；matcher只按ASCII pattern识别，不构造非法str。
                    let mut probe = HtmlTerminator::new(end);
                    probe.feed(&body.as_bytes()[..split]);
                    assert_eq!(
                        probe.feed(&body.as_bytes()[split..]),
                        expected,
                        "{end} {split}"
                    );
                }
            }
        }
        let mut probe = HtmlTerminator::new("-->");
        for _ in 0..1024 {
            // 巨长opaque行也仅保pattern匹配位置；末尾仍可识别真正closing而不丢后续块边界。
            assert!(!probe.feed(&[b'-'; 4096]));
        }
        assert!(probe.feed(b">"));
        assert!(std::mem::size_of::<HtmlTerminator>() <= 40);
    }

    /// 未改上游函数体作为纯oracle；保留完整原语法，不使用候选本身当expected。
    fn original_closing(bytes: &[u8], marker: u8, minimum: usize) -> Option<usize> {
        if bytes.is_empty() {
            return Some(0);
        }
        let mut i = 0;
        let count = scan_ch_repeat(&bytes[i..], marker);
        if count < minimum {
            return None;
        }
        i += count;
        i += scan_ch_repeat(&bytes[i..], b' ');
        scan_eol(&bytes[i..]).map(|_| i)
    }

    #[test]
    fn closing_probe_all_frame_splits_match_original_including_empty_eof_and_tab() {
        for line in [
            "",
            "`",
            "``",
            "```",
            "```   ",
            "```\t",
            "```` x",
            "```\r\nrest",
            "~~~\n",
            "   ```\n",
        ] {
            for marker in [b'`', b'~'] {
                for minimum in 3..6 {
                    let expected = original_closing(line.as_bytes(), marker, minimum);
                    for split in 0..=line.len() {
                        let mut probe = CodeFenceCloser::new(marker, minimum);
                        let first = probe.feed(&line.as_bytes()[..split], false);
                        let final_result = if first == ClosingFenceResult::NeedInput {
                            probe.feed(&line.as_bytes()[split..], true)
                        } else {
                            first
                        };
                        assert_eq!(
                            final_result,
                            expected
                                .map_or(ClosingFenceResult::NotClosed, ClosingFenceResult::Closed),
                            "line={line:?} split={split}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn very_long_space_tail_and_opaque_failure_keep_constant_probe_size() {
        let mut probe = CodeFenceCloser::new(b'`', 3);
        assert_eq!(probe.feed(b"```", false), ClosingFenceResult::NeedInput);
        let spaces = [b' '; 4096];
        for _ in 0..1024 {
            assert_eq!(probe.feed(&spaces, false), ClosingFenceResult::NeedInput);
        }
        assert_eq!(
            probe.feed(b"\n", false),
            ClosingFenceResult::Closed(3 + 4096 * 1024)
        );
        assert!(std::mem::size_of::<CodeFenceCloser>() <= 80);
        let mut opaque = CodeFenceCloser::new(b'`', 3);
        assert_eq!(opaque.feed(b"x", false), ClosingFenceResult::NotClosed);
        assert_eq!(opaque.feed(&spaces, false), ClosingFenceResult::NotClosed);
    }
}
