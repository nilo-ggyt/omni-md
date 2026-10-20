// Copyright 2015 Google Inc. All rights reserved.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

//! Scanners for fragments of CommonMark syntax

use crate::line_start_stream::{
    LineStartFrame, LineStartOperation, LineStartProgress, LineStartState, LineStartStream,
    LineStartValue, MAX_LINE_START_STEP,
};

use crate::parse::HtmlScanGuard;
pub(crate) use crate::puncttable::{is_ascii_punctuation, is_punctuation};
use crate::strings::CowStr;
use crate::{Alignment, LinkType};
use crate::{BlockQuoteKind, HeadingLevel};

use memchr::memchr;

// sorted for binary search
const HTML_TAGS: [&str; 62] = [
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// Analysis of the beginning of a line, including indentation and container
/// markers.
#[derive(Clone)]
pub(crate) struct LineStart<'a> {
    bytes: &'a [u8],
    ix: usize,

    // The index in `bytes` after the last tab we scanned; initially
    // zero.
    //
    // Thus, there are no tab characters between `ix` and here, and for
    // the purpose of defining block structure, this position can be
    // considered to fall on a tab stop.
    //
    // This is only valid while scanning the initial portion of the
    // line; methods that work with interior structure don't bother to
    // update it.
    tab_start: usize,

    // In contexts where spaces help to define block structure, tabs
    // behave as if they were replaced by spaces with a tab stop of 4
    // characters.
    //
    // If we have scanned past a tab character but not consumed all
    // the horizontal width it contributed, this is the number of
    // spaces logically remaining, before the character at `ix`.
    spaces_remaining: usize,

    // no thematic breaks can occur before this offset.
    // this prevents scanning over and over up to a certain point
    min_hrule_offset: usize,
}

impl<'a> LineStart<'a> {
    #[cfg(feature = "omni-block-frontier")]
    /// 只接原短块 context 已证明的同物理行状态；部分 Tab 列不能由最终字节偏移反推。
    pub(crate) fn from_stream_state(bytes: &'a [u8], state: LineStartState) -> Self {
        let mut line = Self::new(bytes);
        line.apply_stream_state(state);
        line
    }

    pub(crate) fn new(bytes: &[u8]) -> LineStart<'_> {
        LineStart {
            bytes,
            tab_start: 0,
            ix: 0,
            spaces_remaining: 0,
            min_hrule_offset: 0,
        }
    }

    /// Try to scan a number of spaces.
    ///
    /// Returns true if all spaces were consumed.
    ///
    /// Note: consumes some spaces even if not successful.
    pub(crate) fn scan_space(&mut self, n_space: usize) -> bool {
        self.scan_space_inner(n_space) == 0
    }

    /// Scan a number of spaces up to a maximum.
    ///
    /// Returns number of spaces scanned.
    pub(crate) fn scan_space_upto(&mut self, n_space: usize) -> usize {
        n_space - self.scan_space_inner(n_space)
    }

    /// Returns unused remainder of spaces.
    fn scan_space_inner(&mut self, n_space: usize) -> usize {
        // 旧full-slice入口与分段frontier共用同一kernel，失败部分消费语义不另写一套。
        let (state, value) = self.stream_operation(LineStartOperation::Space(n_space));
        self.apply_stream_state(state);
        let LineStartValue::SpaceRemaining(remaining) = value else {
            unreachable!()
        };
        remaining
    }

    /// Scan all available ASCII whitespace (not including eol).
    pub(crate) fn scan_all_space(&mut self) {
        // kernel保留原方法不更新tab_start的interior规则。
        let (state, _) = self.stream_operation(LineStartOperation::AllSpace);
        self.apply_stream_state(state);
    }

    /// Determine whether we're at end of line (includes end of file).
    pub(crate) fn is_at_eol(&self) -> bool {
        // 原只读判定仍不改变scalar；真正EOF由完整借用slice给出。
        let (_, value) = self.stream_operation(LineStartOperation::AtEol);
        let LineStartValue::Bool(at_eol) = value else {
            unreachable!()
        };
        at_eol
    }

    fn scan_ch(&mut self, c: u8) -> bool {
        if self.ix < self.bytes.len() && self.bytes[self.ix] == c {
            self.ix += 1;
            true
        } else {
            false
        }
    }

    fn scan_case_insensitive(&mut self, tag: &[u8]) -> bool {
        if self.bytes.len() - self.ix < tag.len() {
            return false;
        }
        let prefix = &self.bytes[self.ix..self.ix + tag.len()];
        let ok = prefix.eq_ignore_ascii_case(tag);
        if ok {
            self.ix += tag.len();
        }
        ok
    }

    pub(crate) fn scan_blockquote_tag(&mut self) -> Option<BlockQuoteKind> {
        let saved_ix = self.ix;
        let tag = if self.scan_ch(b'[') && self.scan_ch(b'!') {
            let tag = if self.scan_case_insensitive(b"note") {
                Some(BlockQuoteKind::Note)
            } else if self.scan_case_insensitive(b"tip") {
                Some(BlockQuoteKind::Tip)
            } else if self.scan_case_insensitive(b"important") {
                Some(BlockQuoteKind::Important)
            } else if self.scan_case_insensitive(b"warning") {
                Some(BlockQuoteKind::Warning)
            } else if self.scan_case_insensitive(b"caution") {
                Some(BlockQuoteKind::Caution)
            } else {
                None
            };
            if tag.is_some() && self.scan_ch(b']') {
                if let Some(nl) = scan_blank_line(&self.bytes[self.ix..]) {
                    self.ix += nl;
                    tag
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if tag.is_none() {
            self.ix = saved_ix;
        }
        tag
    }

    pub(crate) fn scan_blockquote_marker(&mut self) -> bool {
        // 同一个quote kernel处理tab余宽，不使用缩短的伪行副本。
        let (state, value) = self.stream_operation(LineStartOperation::Quote);
        self.apply_stream_state(state);
        let LineStartValue::Bool(found) = value else {
            unreachable!()
        };
        found
    }

    /// Scan a definition marker.
    ///
    /// Definition markers are single colons, preceded by at most three spaces
    /// and followed by at most three spaces. The indentation of following
    /// lines is equal to the whole size of the marker, including the colon.
    ///
    /// If one is found, it will make the preceding paragraph into a definition
    /// list title.
    ///
    /// Return value is the amount of indentation, or `None` if it's not a
    /// definition list marker.
    pub(crate) fn scan_definition_list_definition_marker_with_indent(
        &mut self,
        indent: usize,
    ) -> Option<usize> {
        // 五格probe与回滚都保存在共享续体，full-slice不能有另一个语法分支。
        let (state, value) = self.stream_operation(LineStartOperation::Definition(indent));
        self.apply_stream_state(state);
        let LineStartValue::Definition(found) = value else {
            unreachable!()
        };
        found
    }

    /// Scan a list marker.
    ///
    /// Return value is the character, the start index, and the indent in spaces.
    /// For ordered list markers, the character will be one of b'.' or b')'. For
    /// bullet list markers, it will be one of b'-', b'+', or b'*'.
    pub(crate) fn scan_list_marker_with_indent(
        &mut self,
        indent: usize,
    ) -> Option<(u8, u64, usize)> {
        // thematic-break/blank lookahead与marker提交位置分离，返回值仍是原API。
        let (state, value) = self.stream_operation(LineStartOperation::List(indent));
        self.apply_stream_state(state);
        let LineStartValue::List(found) = value else {
            unreachable!()
        };
        found
    }

    /// Returns Some(is_checked) when a task list marker was found. Resets itself
    /// to original state otherwise.
    pub(crate) fn scan_task_list_marker(&mut self) -> Option<bool> {
        let save = self.clone();
        self.scan_space_upto(3);

        if !self.scan_ch(b'[') {
            *self = save;
            return None;
        }
        let is_checked = match self.bytes.get(self.ix) {
            Some(&c) if is_ascii_whitespace_no_nl(c) => {
                self.ix += 1;
                false
            }
            Some(b'x') | Some(b'X') => {
                self.ix += 1;
                true
            }
            _ => {
                *self = save;
                return None;
            }
        };
        if !self.scan_ch(b']') {
            *self = save;
            return None;
        }
        if !self
            .bytes
            .get(self.ix)
            .map(|&b| is_ascii_whitespace(b))
            .unwrap_or(false)
        {
            *self = save;
            return None;
        }
        Some(is_checked)
    }

    /// full-slice兼容入口只借有界frame驱动共享kernel；不缓存整行/正文。
    fn stream_operation(&self, operation: LineStartOperation) -> (LineStartState, LineStartValue) {
        let state = LineStartState {
            ix: self.ix,
            tab_start: self.tab_start,
            spaces_remaining: self.spaces_remaining,
            min_hrule_offset: self.min_hrule_offset,
        };
        let mut stream = LineStartStream::new(0, 0, self.bytes.len(), state, operation)
            .expect("原LineStart标量必须处于真实借用源范围内");
        let mut next_byte = stream.next_byte();
        loop {
            // 此入口确实拥有完整slice；分段frontier则由它自己的源借用器提供同样的frame。
            let end = next_byte
                .saturating_add(MAX_LINE_START_STEP)
                .min(self.bytes.len());
            let step = stream
                .advance(
                    LineStartFrame {
                        source_revision: 0,
                        byte_start: next_byte,
                        bytes: &self.bytes[next_byte..end],
                        real_eof: end == self.bytes.len(),
                    },
                    MAX_LINE_START_STEP,
                    MAX_LINE_START_STEP,
                )
                .expect("完整slice驱动不产生错误frame或超预算");
            match step.progress {
                // 仅实际Complete才能发布state.ix，lookahead读取位置不表示提交前进。
                LineStartProgress::Complete { state, value } => return (state, value),
                // 续体可能要求重借早先anchor，借slice不复制也不保留此前frame。
                LineStartProgress::Progress { next_byte: next }
                | LineStartProgress::NeedInput { next_byte: next } => next_byte = next,
            }
        }
    }

    /// 只在共享操作完成时原子更新四个原标量，失败marker的回滚也走同一发布点。
    fn apply_stream_state(&mut self, state: LineStartState) {
        self.ix = state.ix;
        self.tab_start = state.tab_start;
        self.spaces_remaining = state.spaces_remaining;
        self.min_hrule_offset = state.min_hrule_offset;
    }

    pub(crate) fn bytes_scanned(&self) -> usize {
        self.ix
    }

    pub(crate) fn remaining_space(&self) -> usize {
        self.spaces_remaining
    }
}

pub(crate) fn is_ascii_whitespace(c: u8) -> bool {
    (0x09..=0x0d).contains(&c) || c == b' '
}

pub(crate) fn is_ascii_whitespace_no_nl(c: u8) -> bool {
    c == b'\t' || c == 0x0b || c == 0x0c || c == b' '
}

fn is_ascii_alpha(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

fn is_ascii_alphanumeric(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
}

fn is_ascii_letterdigitdash(c: u8) -> bool {
    c == b'-' || is_ascii_alphanumeric(c)
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

pub(crate) fn is_valid_unquoted_attr_value_char(c: u8) -> bool {
    !matches!(
        c,
        b'\'' | b'"' | b' ' | b'=' | b'>' | b'<' | b'`' | b'\n' | b'\r'
    )
}

// scan a single character
pub(crate) fn scan_ch(data: &[u8], c: u8) -> usize {
    if !data.is_empty() && data[0] == c {
        1
    } else {
        0
    }
}

pub(crate) fn scan_while<F>(data: &[u8], mut f: F) -> usize
where
    F: FnMut(u8) -> bool,
{
    data.iter().take_while(|&&c| f(c)).count()
}

pub(crate) fn scan_rev_while<F>(data: &[u8], mut f: F) -> usize
where
    F: FnMut(u8) -> bool,
{
    data.iter().rev().take_while(|&&c| f(c)).count()
}

pub(crate) fn scan_ch_repeat(data: &[u8], c: u8) -> usize {
    scan_while(data, |x| x == c)
}

// Note: this scans ASCII whitespace only, for Unicode whitespace use
// a different function.
pub(crate) fn scan_whitespace_no_nl(data: &[u8]) -> usize {
    scan_while(data, is_ascii_whitespace_no_nl)
}

fn scan_attr_value_chars(data: &[u8]) -> usize {
    scan_while(data, is_valid_unquoted_attr_value_char)
}

pub(crate) fn scan_eol(bytes: &[u8]) -> Option<usize> {
    match bytes {
        &[] => Some(0),
        &[b'\n', ..] => Some(1),
        &[b'\r', b'\n', ..] => Some(2),
        &[b'\r', ..] => Some(1),
        _ => None,
    }
}

pub(crate) fn scan_blank_line(bytes: &[u8]) -> Option<usize> {
    // 完整源适配器与 Global 共用同一 no_nl/CRLF 状态机；不另保一套 blank grammar。
    let (_, value) = LineStart::new(bytes).stream_operation(LineStartOperation::BlankLine);
    let LineStartValue::BlankLine(count) = value else {
        unreachable!()
    };
    count
}

pub(crate) fn scan_nextline(bytes: &[u8]) -> usize {
    memchr(b'\n', bytes).map_or(bytes.len(), |x| x + 1)
}

// return: end byte for closing code fence, or None
// if the line is not a closing code fence
pub(crate) fn scan_closing_code_fence(
    bytes: &[u8],
    fence_char: u8,
    n_fence_char: usize,
) -> Option<usize> {
    // 连续源调用拥有真实slice末尾权限；分段frontier单独传false，不在任意frame尾关闭fence。
    let mut probe = crate::leaf_scan::CodeFenceCloser::new(fence_char, n_fence_char);
    match probe.feed(bytes, true) {
        crate::leaf_scan::ClosingFenceResult::Closed(offset) => Some(offset),
        crate::leaf_scan::ClosingFenceResult::NotClosed => None,
        crate::leaf_scan::ClosingFenceResult::NeedInput => unreachable!("完整slice具有EOF权限"),
    }
}

// return: end byte for closing metadata block, or None
// if the line is not a closing metadata block
pub(crate) fn scan_closing_metadata_block(bytes: &[u8], fence_char: u8) -> Option<usize> {
    let mut probe = crate::metadata_scan::MetadataCloser::new(fence_char);
    match probe.feed(bytes, true) {
        crate::metadata_scan::MetadataClosing::Closed(end) => Some(end),
        crate::metadata_scan::MetadataClosing::NotClosed => None,
        crate::metadata_scan::MetadataClosing::NeedInput => unreachable!("完整slice具有EOF权限"),
    }
}

// returned pair is (number of bytes, number of spaces)
pub(crate) fn calc_indent(text: &[u8], max: usize) -> (usize, usize) {
    let mut spaces = 0;
    let mut offset = 0;

    for (i, &b) in text.iter().enumerate() {
        offset = i;
        match b {
            b' ' => {
                spaces += 1;
                if spaces == max {
                    break;
                }
            }
            b'\t' => {
                let new_spaces = spaces + 4 - (spaces & 3);
                if new_spaces > max {
                    break;
                }
                spaces = new_spaces;
            }
            _ => break,
        }
    }

    (offset, spaces)
}

/// Scan hrule opening sequence.
///
/// Returns Ok(x) when it finds an hrule, where x is the
/// size of line containing the hrule, including the trailing newline.
///
/// Returns Err(x) when it does not find an hrule and x is
/// the offset in data before no hrule can appear.
pub(crate) fn scan_hrule(bytes: &[u8]) -> Result<usize, usize> {
    if bytes.len() < 3 {
        return Err(0);
    }
    let c = bytes[0];
    if !(c == b'*' || c == b'-' || c == b'_') {
        return Err(0);
    }
    let mut n = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' | b'\r' => {
                i += scan_eol(&bytes[i..]).unwrap_or(0);
                break;
            }
            c2 if c2 == c => {
                n += 1;
            }
            b' ' | b'\t' => (),
            _ => return Err(i),
        }
        i += 1;
    }
    if n >= 3 {
        Ok(i)
    } else {
        Err(i)
    }
}

/// Scan an ATX heading opening sequence.
///
/// Returns number of bytes in prefix and level.
pub(crate) fn scan_atx_heading(data: &[u8]) -> Option<HeadingLevel> {
    let level = scan_ch_repeat(data, b'#');
    if data.get(level).copied().map_or(true, is_ascii_whitespace) {
        HeadingLevel::try_from(level).ok()
    } else {
        None
    }
}

/// Scan a setext heading underline.
///
/// Returns number of bytes in line (including trailing newline) and level.
pub(crate) fn scan_setext_heading(data: &[u8]) -> Option<(usize, HeadingLevel)> {
    let c = *data.first()?;
    let level = if c == b'=' {
        HeadingLevel::H1
    } else if c == b'-' {
        HeadingLevel::H2
    } else {
        return None;
    };
    let mut i = 1 + scan_ch_repeat(&data[1..], c);
    i += scan_blank_line(&data[i..])?;
    Some((i, level))
}

// returns number of bytes in line (including trailing
// newline) and column alignments
pub(crate) fn scan_table_head(data: &[u8]) -> (usize, Vec<Alignment>) {
    let (mut i, spaces) = calc_indent(data, 4);
    if spaces > 3 || i == data.len() {
        return (0, vec![]);
    }
    let mut cols = vec![];
    let mut active_col = Alignment::None;
    let mut start_col = true;
    let mut found_pipe = false;
    let mut found_hyphen = false;
    let mut found_hyphen_in_col = false;
    if data[i] == b'|' {
        i += 1;
        found_pipe = true;
    }
    for c in &data[i..] {
        if let Some(n) = scan_eol(&data[i..]) {
            i += n;
            break;
        }
        match *c {
            b' ' => (),
            b':' => {
                active_col = match (start_col, active_col) {
                    (true, Alignment::None) => Alignment::Left,
                    (false, Alignment::Left) => Alignment::Center,
                    (false, Alignment::None) => Alignment::Right,
                    _ => active_col,
                };
                start_col = false;
            }
            b'-' => {
                start_col = false;
                found_hyphen = true;
                found_hyphen_in_col = true;
            }
            b'|' => {
                start_col = true;
                found_pipe = true;
                cols.push(active_col);
                active_col = Alignment::None;
                if !found_hyphen_in_col {
                    // It isn't a table head if it has back-to-back pipes.
                    return (0, vec![]);
                }
                found_hyphen_in_col = false;
            }
            _ => {
                // It isn't a table head if it has characters outside the allowed set.
                return (0, vec![]);
            }
        }
        i += 1;
    }

    if !start_col {
        cols.push(active_col);
    }
    if !found_pipe || !found_hyphen {
        // It isn't a table head if it doesn't have a least one pipe or hyphen.
        // It's a list, a header, or a thematic break.
        return (0, vec![]);
    }

    (i, cols)
}

/// Scan code fence.
///
/// Returns number of bytes scanned and the char that is repeated to make the code fence.
pub(crate) fn scan_code_fence(data: &[u8]) -> Option<(usize, u8)> {
    let c = *data.first()?;
    if !(c == b'`' || c == b'~') {
        return None;
    }
    let i = 1 + scan_ch_repeat(&data[1..], c);
    if i >= 3 {
        if c == b'`' {
            let suffix = &data[i..];
            let next_line = i + scan_nextline(suffix);
            // FIXME: make sure this is correct
            if suffix[..(next_line - i)].iter().any(|&b| b == b'`') {
                return None;
            }
        }
        Some((i, c))
    } else {
        None
    }
}

/// Scan metadata block, returning the number of delimiter bytes
/// (always 3 for now) and the delimiter character.
///
/// Differently to code blocks, metadata blocks must be closed with the closing
/// sequence not being a valid terminator the end of the file.
///
/// In addition, they cannot be empty (closing sequence in the next line) and
/// the next line cannot be an empty line.
pub(crate) fn scan_metadata_block(
    data: &[u8],
    yaml_style_enabled: bool,
    pluses_style_enabled: bool,
) -> Option<(usize, u8)> {
    let (opening, marker) =
        scan_metadata_block_opening(data, yaml_style_enabled, pluses_style_enabled)?;
    let body_start = opening + scan_nextline(&data[opening..]);
    let mut lookahead = crate::metadata_scan::MetadataLookahead::new(marker, body_start);
    match lookahead.feed(&data[body_start..], true) {
        crate::metadata_scan::MetadataDecision::Accepted { .. } => Some((opening, marker)),
        crate::metadata_scan::MetadataDecision::Rejected => None,
        crate::metadata_scan::MetadataDecision::NeedInput => unreachable!("完整slice具有EOF权限"),
    }
}

/// 只确认完整opening行；存在真实closing的权限必须另由可暂停lookahead给出。
pub(crate) fn scan_metadata_block_opening(
    data: &[u8],
    yaml_style_enabled: bool,
    pluses_style_enabled: bool,
) -> Option<(usize, u8)> {
    if !yaml_style_enabled && !pluses_style_enabled {
        return None;
    }
    let marker = *data.first()?;
    if !((marker == b'-' && yaml_style_enabled) || (marker == b'+' && pluses_style_enabled)) {
        return None;
    }
    let count = 1 + scan_ch_repeat(&data[1..], marker);
    let next_line = scan_nextline(&data[count..]);
    if !data[count..count + next_line]
        .iter()
        .all(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    (count == 3).then_some((count, marker))
}

pub(crate) fn scan_blockquote_start(data: &[u8]) -> Option<usize> {
    if data.first().copied() == Some(b'>') {
        let space = if data.get(1).copied() == Some(b' ') {
            1
        } else {
            0
        };
        Some(1 + space)
    } else {
        None
    }
}

/// return number of bytes scanned, delimiter, start index, and indent
pub(crate) fn scan_listitem(bytes: &[u8]) -> Option<(usize, u8, usize, usize)> {
    let mut c = *bytes.first()?;
    let (w, start) = match c {
        b'-' | b'+' | b'*' => (1, 0),
        b'0'..=b'9' => {
            let (length, start) = parse_decimal(bytes, 9);
            c = *bytes.get(length)?;
            if !(c == b'.' || c == b')') {
                return None;
            }
            (length + 1, start)
        }
        _ => {
            return None;
        }
    };
    // TODO: replace calc_indent with scan_leading_whitespace, for tab correctness
    let (mut postn, mut postindent) = calc_indent(&bytes[w..], 5);
    if postindent == 0 {
        scan_eol(&bytes[w..])?;
        postindent += 1;
    } else if postindent > 4 {
        postn = 1;
        postindent = 1;
    }
    if scan_blank_line(&bytes[w..]).is_some() {
        postn = 0;
        postindent = 1;
    }
    Some((w + postn, c, start, w + postindent))
}

// returns (number of bytes, parsed decimal)
fn parse_decimal(bytes: &[u8], limit: usize) -> (usize, usize) {
    match bytes
        .iter()
        .take(limit)
        .take_while(|&&b| is_digit(b))
        .try_fold((0, 0usize), |(count, acc), c| {
            let digit = usize::from(c - b'0');
            match acc
                .checked_mul(10)
                .and_then(|ten_acc| ten_acc.checked_add(digit))
            {
                Some(number) => Ok((count + 1, number)),
                // stop early on overflow
                None => Err((count, acc)),
            }
        }) {
        Ok(p) | Err(p) => p,
    }
}

// doesn't bother to check data[0] == '&'
pub(crate) fn scan_entity(bytes: &[u8]) -> (usize, Option<CowStr<'static>>) {
    let mut scanner = crate::inline_scan::EntityScanner::new();
    for (index, &byte) in bytes.iter().enumerate().skip(1) {
        match scanner.push(byte) {
            // 与分段接口使用同一digit/name状态，不能在切frame后采用第二套实体规则。
            crate::inline_scan::EntityTransition::Continue => {}
            crate::inline_scan::EntityTransition::Matched(value) => {
                return (index + 1, Some(value.into_cow()));
            }
            crate::inline_scan::EntityTransition::Rejected => return (0, None),
        }
    }
    // 真实整slice尾仍未见分号就是原来的未闭合实体；分段cursor则保留scanner等待后续frame。
    (0, None)
}

pub(crate) fn scan_wikilink_pipe(data: &str, start_ix: usize, len: usize) -> Option<(usize, &str)> {
    let bytes = data.as_bytes();
    let end_ix = std::cmp::min(start_ix + len, bytes.len());
    let mut i = start_ix;

    while i < end_ix {
        if bytes[i] == b'|' {
            return Some((i + 1, &data[start_ix..i]));
        }
        i += 1;
    }
    None
}

// note: dest returned is raw, still needs to be unescaped
// TODO: check that nested parens are really not allowed for refdefs
// TODO(performance): this func should probably its own unescaping
pub(crate) fn scan_link_dest(
    data: &str,
    start_ix: usize,
    max_next: usize,
) -> Option<(usize, &str)> {
    use crate::definition_scan::{DestinationResult, DestinationScanner};
    let (_, result) = DestinationScanner::new(max_next).feed(&data.as_bytes()[start_ix..], true);
    match result {
        DestinationResult::Complete {
            byte_count,
            content_start,
            content_end,
        } => Some((
            byte_count,
            &data[start_ix + content_start..start_ix + content_end],
        )),
        DestinationResult::Rejected => None,
        DestinationResult::NeedInput => unreachable!("原whole-slice调用拥有真实EOF权限"),
    }
}

/// Returns bytes scanned
fn scan_attribute_name(data: &[u8]) -> Option<usize> {
    let (&c, tail) = data.split_first()?;
    if is_ascii_alpha(c) || c == b'_' || c == b':' {
        Some(
            1 + scan_while(tail, |c| {
                is_ascii_alphanumeric(c) || c == b'_' || c == b'.' || c == b':' || c == b'-'
            }),
        )
    } else {
        None
    }
}

/// Returns the index immediately following the attribute on success.
/// The argument `buffer_ix` refers to the index into `data` from which we
/// should copy into `buffer` when we find bytes to skip.
fn scan_attribute(
    data: &[u8],
    mut ix: usize,
    newline_handler: Option<&dyn Fn(&[u8]) -> usize>,
    buffer: &mut Vec<u8>,
    buffer_ix: &mut usize,
) -> Option<usize> {
    ix += scan_attribute_name(&data[ix..])?;
    let ix_after_attribute = ix;
    ix = scan_whitespace_with_newline_handler_without_buffer(data, ix, newline_handler)?;
    if data.get(ix) == Some(&b'=') {
        ix = scan_whitespace_with_newline_handler(
            data,
            ix_after_attribute,
            newline_handler,
            buffer,
            buffer_ix,
        )?;
        ix += 1;
        ix = scan_whitespace_with_newline_handler(data, ix, newline_handler, buffer, buffer_ix)?;
        ix = scan_attribute_value(data, ix, newline_handler, buffer, buffer_ix)?;
        Some(ix)
    } else {
        // Leave whitespace for next attribute.
        Some(ix_after_attribute)
    }
}

/// Scans whitespace and possibly newlines according to the
/// behavior defined by the newline handler. When bytes are skipped,
/// all preceding non-skipped bytes are pushed to the buffer.
fn scan_whitespace_with_newline_handler(
    data: &[u8],
    mut i: usize,
    newline_handler: Option<&dyn Fn(&[u8]) -> usize>,
    buffer: &mut Vec<u8>,
    buffer_ix: &mut usize,
) -> Option<usize> {
    while i < data.len() {
        if !is_ascii_whitespace(data[i]) {
            return Some(i);
        }
        if let Some(eol_bytes) = scan_eol(&data[i..]) {
            let handler = newline_handler?;
            i += eol_bytes;
            let skipped_bytes = handler(&data[i..]);

            if skipped_bytes > 0 {
                buffer.extend(&data[*buffer_ix..i]);
                *buffer_ix = i + skipped_bytes;
            }

            i += skipped_bytes;
        } else {
            i += 1;
        }
    }

    Some(i)
}

/// Scans whitespace and possible newlines according to the behavior defined
/// by the newline handler.
///
/// Unlike [`scan_whitespace_with_newline_handler`], this function doesn't
/// copy skipped data into a buffer. Typically, if this function
/// returns `Some`, a call to `scan_whitespace_with_newline_handler` will
/// soon follow.
fn scan_whitespace_with_newline_handler_without_buffer(
    data: &[u8],
    mut i: usize,
    newline_handler: Option<&dyn Fn(&[u8]) -> usize>,
) -> Option<usize> {
    while i < data.len() {
        if !is_ascii_whitespace(data[i]) {
            return Some(i);
        }
        if let Some(eol_bytes) = scan_eol(&data[i..]) {
            let handler = newline_handler?;
            i += eol_bytes;
            let skipped_bytes = handler(&data[i..]);
            i += skipped_bytes;
        } else {
            i += 1;
        }
    }

    Some(i)
}

/// Returns the index immediately following the attribute value on success.
fn scan_attribute_value(
    data: &[u8],
    mut i: usize,
    newline_handler: Option<&dyn Fn(&[u8]) -> usize>,
    buffer: &mut Vec<u8>,
    buffer_ix: &mut usize,
) -> Option<usize> {
    match *data.get(i)? {
        b @ b'"' | b @ b'\'' => {
            i += 1;
            while i < data.len() {
                if data[i] == b {
                    return Some(i + 1);
                }
                if let Some(eol_bytes) = scan_eol(&data[i..]) {
                    let handler = newline_handler?;
                    i += eol_bytes;
                    let skipped_bytes = handler(&data[i..]);

                    if skipped_bytes > 0 {
                        buffer.extend(&data[*buffer_ix..i]);
                        *buffer_ix = i + skipped_bytes;
                    }
                    i += skipped_bytes;
                } else {
                    i += 1;
                }
            }
            return None;
        }
        b' ' | b'=' | b'>' | b'<' | b'`' | b'\n' | b'\r' => {
            return None;
        }
        _ => {
            // unquoted attribute value
            i += scan_attr_value_chars(&data[i..]);
        }
    }

    Some(i)
}

// Remove backslash escapes and resolve entities
pub(crate) fn unescape<'a, I: Into<CowStr<'a>>>(input: I, is_in_table: bool) -> CowStr<'a> {
    let input = input.into();
    let mut result = String::new();
    let mut mark = 0;
    let mut i = 0;
    let bytes = input.as_bytes();
    while i < bytes.len() {
        match bytes[i..] {
            // Tables are special, because they're parsed as-if the tables
            // were parsed in a discrete pass, changing `\|` to `|`, and then
            // passing the changed string to the inline parser.
            [b'\\', b'\\', b'|', ..] if is_in_table => {
                // even number of `\`s before pipe
                // odd number is handled in the normal way below
                result.push_str(&input[mark..i]);
                mark = i + 2;
                i += 3;
            }
            [b'\\', cx, ..] if is_ascii_punctuation(cx) => {
                result.push_str(&input[mark..i]);
                mark = i + 1;
                i += 2;
            }
            [b'&', ..] => match scan_entity(&bytes[i..]) {
                (n, Some(value)) => {
                    result.push_str(&input[mark..i]);
                    result.push_str(&value);
                    i += n;
                    mark = i;
                }
                _ => i += 1,
            },
            [b'\r', ..] => {
                result.push_str(&input[mark..i]);
                i += 1;
                mark = i;
            }
            _ => i += 1,
        }
    }
    if mark == 0 {
        input
    } else {
        result.push_str(&input[mark..]);
        result.into()
    }
}

/// Assumes `data` is preceded by `<`.
pub(crate) fn starts_html_block_type_6(data: &[u8]) -> bool {
    let i = scan_ch(data, b'/');
    let tail = &data[i..];
    let n = scan_while(tail, is_ascii_alphanumeric);
    if !is_html_tag(&tail[..n]) {
        return false;
    }
    // Starting condition says the next byte must be either a space, a tab,
    // the end of the line, the string >, or the string />
    let tail = &tail[n..];
    tail.is_empty()
        || tail[0] == b' '
        || tail[0] == b'\t'
        || tail[0] == b'\r'
        || tail[0] == b'\n'
        || tail[0] == b'>'
        || tail.len() >= 2 && &tail[..2] == b"/>"
}

fn is_html_tag(tag: &[u8]) -> bool {
    HTML_TAGS
        .binary_search_by(|probe| {
            let probe_bytes_iter = probe.as_bytes().iter();
            let tag_bytes_iter = tag.iter();

            probe_bytes_iter
                .zip(tag_bytes_iter)
                .find_map(|(&a, &b)| {
                    // We can compare case insensitively because the probes are
                    // all lower case alpha strings.
                    match a.cmp(&(b | 0x20)) {
                        std::cmp::Ordering::Equal => None,
                        inequality => Some(inequality),
                    }
                })
                .unwrap_or_else(|| probe.len().cmp(&tag.len()))
        })
        .is_ok()
}

/// Assumes that `data` starts with `<`.
/// Returns the index into data directly after the html tag on success.
#[cfg(test)]
pub(crate) fn scan_html_type_7(data: &[u8]) -> Option<usize> {
    // Block type html does not allow for newlines, so we
    // do not pass a newline handler.
    let (_span, i) = scan_html_block_inner(data, None)?;
    scan_blank_line(&data[i..])?;
    Some(i)
}

/// Assumes that `data` starts with `<`.
/// Returns the number of bytes scanned and the html in case of
/// success.
/// When some bytes were skipped, because the html was split over
/// multiple leafs (e.g. over multiple lines in a blockquote),
/// the html is returned as a vector of bytes.
/// If no bytes were skipped, the buffer will be empty.
pub(crate) fn scan_html_block_inner(
    data: &[u8],
    newline_handler: Option<&dyn Fn(&[u8]) -> usize>,
) -> Option<(Vec<u8>, usize)> {
    let mut buffer = Vec::new();
    let mut last_buf_index = 0;

    let close_tag_bytes = scan_ch(&data[1..], b'/');
    let l = scan_while(&data[(1 + close_tag_bytes)..], is_ascii_alpha);
    if l == 0 {
        return None;
    }
    let mut i = 1 + close_tag_bytes + l;
    i += scan_while(&data[i..], is_ascii_letterdigitdash);

    if close_tag_bytes == 0 {
        loop {
            let old_i = i;
            loop {
                i += scan_whitespace_no_nl(&data[i..]);
                if let Some(eol_bytes) = scan_eol(&data[i..]) {
                    if eol_bytes == 0 {
                        return None;
                    }
                    let handler = newline_handler?;
                    i += eol_bytes;
                    let skipped_bytes = handler(&data[i..]);

                    let data_len = data.len() - i;

                    debug_assert!(
                        skipped_bytes <= data_len,
                        "Handler tried to skip too many bytes, fed {}, skipped {}",
                        data_len,
                        skipped_bytes
                    );

                    if skipped_bytes > 0 {
                        buffer.extend(&data[last_buf_index..i]);
                        i += skipped_bytes;
                        last_buf_index = i;
                    }
                } else {
                    break;
                }
            }
            if let Some(b'/') | Some(b'>') = data.get(i) {
                break;
            }
            if old_i == i {
                // No whitespace, which is mandatory.
                return None;
            }
            i = scan_attribute(data, i, newline_handler, &mut buffer, &mut last_buf_index)?;
        }
    }

    i += scan_whitespace_no_nl(&data[i..]);

    if close_tag_bytes == 0 {
        i += scan_ch(&data[i..], b'/');
    }

    if data.get(i) != Some(&b'>') {
        None
    } else {
        i += 1;
        if !buffer.is_empty() {
            buffer.extend(&data[last_buf_index..i]);
        }
        Some((buffer, i))
    }
}

/// Returns (next_byte_offset, uri, type)
pub(crate) fn scan_autolink(text: &str, start_ix: usize) -> Option<(usize, CowStr<'_>, LinkType)> {
    scan_uri(text, start_ix)
        .map(|(bytes, uri)| (bytes, uri, LinkType::Autolink))
        .or_else(|| scan_email(text, start_ix).map(|(bytes, uri)| (bytes, uri, LinkType::Email)))
}

/// Returns (next_byte_offset, uri)
fn scan_uri(text: &str, start_ix: usize) -> Option<(usize, CowStr<'_>)> {
    let bytes = &text.as_bytes()[start_ix..];

    // scheme's first byte must be an ascii letter
    if bytes.is_empty() || !is_ascii_alpha(bytes[0]) {
        return None;
    }

    let mut i = 1;

    while i < bytes.len() {
        let c = bytes[i];
        i += 1;
        match c {
            c if is_ascii_alphanumeric(c) => (),
            b'.' | b'-' | b'+' => (),
            b':' => break,
            _ => return None,
        }
    }

    // scheme length must be between 2 and 32 characters long. scheme
    // must be followed by colon
    if !(3..=33).contains(&i) {
        return None;
    }

    while i < bytes.len() {
        match bytes[i] {
            b'>' => return Some((start_ix + i + 1, text[start_ix..(start_ix + i)].into())),
            b'\0'..=b' ' | b'<' => return None,
            _ => (),
        }
        i += 1;
    }

    None
}

/// Returns (next_byte_offset, email)
fn scan_email(text: &str, start_ix: usize) -> Option<(usize, CowStr<'_>)> {
    // using a regex library would be convenient, but doing it by hand is not too bad
    let bytes = &text.as_bytes()[start_ix..];
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];
        i += 1;
        match c {
            c if is_ascii_alphanumeric(c) => (),
            b'.' | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'/' | b'=' | b'?'
            | b'^' | b'_' | b'`' | b'{' | b'|' | b'}' | b'~' | b'-' => (),
            b'@' if i > 1 => break,
            _ => return None,
        }
    }

    loop {
        let label_start_ix = i;
        let mut fresh_label = true;

        while i < bytes.len() {
            match bytes[i] {
                c if is_ascii_alphanumeric(c) => (),
                b'-' if fresh_label => {
                    return None;
                }
                b'-' => (),
                _ => break,
            }
            fresh_label = false;
            i += 1;
        }

        if i == label_start_ix || i - label_start_ix > 63 || bytes[i - 1] == b'-' {
            return None;
        }

        if bytes.get(i) != Some(&b'.') {
            break;
        }
        i += 1;
    }

    if bytes.get(i) != Some(&b'>') {
        return None;
    }

    Some((start_ix + i + 1, text[start_ix..(start_ix + i)].into()))
}

/// Scan comment, declaration, or CDATA section, with initial "<!" already consumed.
/// Returns byte offset on match.
pub(crate) fn scan_inline_html_comment(
    bytes: &[u8],
    mut ix: usize,
    scan_guard: &mut HtmlScanGuard,
) -> Option<usize> {
    let c = *bytes.get(ix)?;
    ix += 1;
    match c {
        // An HTML comment consists of `<!-->`, `<!--->`, or  `<!--`, a string of characters not
        // including the string `-->`, and `-->`.
        b'-' if ix > scan_guard.comment => {
            // HTML comment needs two hyphens after the !.
            if *bytes.get(ix)? != b'-' {
                return None;
            }
            // Yes, we're intentionally going backwards.
            // We want the cursor to point here:
            //
            //     <!--
            //       ^
            //
            // This way, the `<!-->` case is covered by the loop below.
            ix -= 1;

            while let Some(x) = memchr(b'-', &bytes[ix..]) {
                ix += x + 1;
                scan_guard.comment = ix;
                if bytes.get(ix) == Some(&b'-') && bytes.get(ix + 1) == Some(&b'>') {
                    return Some(ix + 2);
                }
            }
            None
        }
        // A CDATA section consists of the string `<![CDATA[`, a string of characters not
        // including the string `]]>`, and the string `]]>`.
        b'[' if bytes[ix..].starts_with(b"CDATA[") && ix > scan_guard.cdata => {
            ix += b"CDATA[".len();
            ix = memchr(b']', &bytes[ix..]).map_or(bytes.len(), |x| ix + x);
            let close_brackets = scan_ch_repeat(&bytes[ix..], b']');
            ix += close_brackets;

            if close_brackets == 0 || bytes.get(ix) != Some(&b'>') {
                scan_guard.cdata = ix;
                None
            } else {
                Some(ix + 1)
            }
        }
        // A declaration consists of the string `<!`, an ASCII letter, zero or more characters not
        // including the character >, and the character >.
        _ if c.is_ascii_alphabetic() && ix > scan_guard.declaration => {
            ix = memchr(b'>', &bytes[ix..]).map_or(bytes.len(), |x| ix + x);
            if bytes.get(ix) != Some(&b'>') {
                scan_guard.declaration = ix;
                None
            } else {
                Some(ix + 1)
            }
        }
        _ => None,
    }
}

/// Scan processing directive, with initial "<?" already consumed.
/// Returns the next byte offset on success.
pub(crate) fn scan_inline_html_processing(
    bytes: &[u8],
    mut ix: usize,
    scan_guard: &mut HtmlScanGuard,
) -> Option<usize> {
    if ix <= scan_guard.processing {
        return None;
    }
    while let Some(offset) = memchr(b'?', &bytes[ix..]) {
        ix += offset + 1;
        if bytes.get(ix) == Some(&b'>') {
            return Some(ix + 1);
        }
    }
    scan_guard.processing = ix;
    None
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn overflow_list() {
        assert!(
            scan_listitem(b"4444444444444444444444444444444444444444444444444444444444!").is_none()
        );
    }

    #[test]
    fn overflow_by_addition() {
        assert!(scan_listitem(b"1844674407370955161615!").is_none());
    }

    #[test]
    fn good_emails() {
        const EMAILS: &[&str] = &[
            "<a@b.c>",
            "<a@b>",
            "<a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-@example.com>",
            "<a@sixty-three-letters-in-this-identifier-----------------------63>",
        ];
        for email in EMAILS {
            assert!(scan_email(email, 1).is_some());
        }
    }

    #[test]
    fn bad_emails() {
        const EMAILS: &[&str] = &[
            "<@b.c>",
            "<foo@-example.com>",
            "<foo@example-.com>",
            "<a@notrailingperiod.>",
            "<a(noparens)@example.com>",
            "<\"noquotes\"@example.com>",
            "<a@sixty-four-letters-in-this-identifier-------------------------64>",
        ];
        for email in EMAILS {
            assert!(scan_email(email, 1).is_none());
        }
    }
}
