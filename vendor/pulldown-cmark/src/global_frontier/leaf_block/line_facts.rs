// Copyright 2015 Google Inc. All rights reserved.
// 边界规则取自固定 pulldown-cmark 0.13.4 firstpass；完整 MIT 条款见 LICENSE。

//! 原 firstpass 与分段 producer 共用的有限边界状态；不保存行字节或解码后的正文。

use std::ops::Range;

use crate::scanners::{is_ascii_whitespace, is_ascii_whitespace_no_nl};

#[derive(Clone, Copy, Debug, Default)]
/// 只跟踪原 reverse trim 所需的最后一个字符与此前连续空格，不反向借取源。
struct HashTail {
    next: usize,
    last_end: usize,
    last_is_hash: bool,
    previous_is_hash: bool,
    spaces: usize,
    hash_start: usize,
    hash_spaces: usize,
}

impl HashTail {
    fn observe(&mut self, byte: u8) {
        if byte == b'#' && !self.previous_is_hash {
            self.hash_start = self.next;
            self.hash_spaces = self.spaces;
        }
        self.previous_is_hash = byte == b'#';
        self.spaces = if byte == b' ' { self.spaces + 1 } else { 0 };
        self.next += 1;
        if !matches!(byte, b'\n' | b'\r' | b' ') {
            self.last_end = self.next;
            self.last_is_hash = byte == b'#';
        }
    }

    fn result(self) -> (usize, Option<Range<usize>>) {
        // 原规则允许纯 # 正文变空；其他 closing run 前必须有 ASCII space。
        if self.last_is_hash && (self.hash_start == 0 || self.hash_spaces > 0) {
            (
                self.hash_start.saturating_sub(self.hash_spaces),
                Some(self.hash_start..self.last_end),
            )
        } else {
            (self.last_end, None)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeadingBounds {
    pub(crate) content_end: usize,
    pub(crate) trim_end: usize,
    pub(crate) closing_hashes: Option<Range<usize>>,
    pub(crate) attributes: Option<Range<usize>>,
}

#[derive(Clone, Copy, Debug, Default)]
/// attributes 的 reverse 搜索只可能遇到最后一个特殊字符；在 `{` 前保留一份标量快照即可。
pub(crate) struct HeadingBoundsScanner {
    tail: HashTail,
    last_special: Option<(u8, usize)>,
    before_open: HashTail,
    attribute_candidate: Option<(usize, usize, HashTail)>,
    last_attribute_nonspace: Option<u8>,
}

impl HeadingBoundsScanner {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let position = self.tail.next;
            // 原 attributes 搜索仅跳过 CR/LF/space/tab；VT/FF 会否定末尾 `}`。
            if !matches!(byte, b'\n' | b'\r' | b' ' | b'\t') {
                self.last_attribute_nonspace = Some(byte);
                self.attribute_candidate = if byte == b'}' {
                    match self.last_special {
                        Some((b'{', opening)) => Some((opening, position, self.before_open)),
                        _ => None,
                    }
                } else {
                    None
                };
            }
            // 这些字符恰是原 extract_attribute_block 的反向停止集合。
            if matches!(byte, b'{' | b'}' | b'<' | b'>' | b'\\' | b'\n' | b'\r') {
                if byte == b'{' {
                    self.before_open = self.tail;
                }
                self.last_special = Some((byte, position));
            }
            self.tail.observe(byte);
        }
    }

    pub(crate) fn finish(self, attributes_enabled: bool) -> HeadingBounds {
        // 只剥离真实末尾候选；属性值是否合法仍由原 parser 解释，不在边界层造 id/classes。
        if attributes_enabled && self.last_attribute_nonspace == Some(b'}') {
            if let Some((opening, closing, before)) = self.attribute_candidate {
                let (trim_end, closing_hashes) = before.result();
                return HeadingBounds {
                    content_end: opening,
                    trim_end,
                    closing_hashes,
                    attributes: Some(opening + 1..closing),
                };
            }
        }
        let (trim_end, closing_hashes) = self.tail.result();
        HeadingBounds {
            content_end: self.tail.next,
            trim_end,
            closing_hashes,
            attributes: None,
        }
    }
}

pub(crate) fn heading_bounds(bytes: &[u8], attributes_enabled: bool) -> HeadingBounds {
    let mut scanner = HeadingBoundsScanner::default();
    scanner.observe(bytes);
    scanner.finish(attributes_enabled)
}

#[derive(Clone, Copy, Debug, Default)]
/// fence info 先跳 no_nl 空白、再按所有 ASCII 空白修剪尾部；只保两个偏移。
pub(crate) struct FenceInfoBoundsScanner {
    next: usize,
    leading: usize,
    leading_done: bool,
    last_nonspace_end: usize,
}

impl FenceInfoBoundsScanner {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if !self.leading_done && is_ascii_whitespace_no_nl(byte) {
                self.leading += 1;
            } else {
                self.leading_done = true;
            }
            self.next += 1;
            if !is_ascii_whitespace(byte) {
                self.last_nonspace_end = self.next;
            }
        }
    }

    pub(crate) fn finish(self) -> Range<usize> {
        self.leading..self.last_nonspace_end.max(self.leading)
    }
}

pub(crate) fn fence_info_bounds(bytes: &[u8]) -> Range<usize> {
    let mut scanner = FenceInfoBoundsScanner::default();
    scanner.observe(bytes);
    scanner.finish()
}
