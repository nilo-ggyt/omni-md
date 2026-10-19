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

//! 测试专用固定0.13.4 oracle；不进入产品，不以新状态机生成期望值。

#[derive(Clone)]
pub(crate) struct OracleLineStart<'a> {
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

impl<'a> OracleLineStart<'a> {
    pub(crate) fn from_state(bytes: &[u8], state: super::LineStartState) -> OracleLineStart<'_> {
        OracleLineStart {
            bytes,
            ix: state.ix,
            tab_start: state.tab_start,
            spaces_remaining: state.spaces_remaining,
            min_hrule_offset: state.min_hrule_offset,
        }
    }
    pub(crate) fn state(&self) -> super::LineStartState {
        super::LineStartState {
            ix: self.ix,
            tab_start: self.tab_start,
            spaces_remaining: self.spaces_remaining,
            min_hrule_offset: self.min_hrule_offset,
        }
    }

    pub(crate) fn new(bytes: &[u8]) -> OracleLineStart<'_> {
        OracleLineStart {
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
    fn scan_space_inner(&mut self, mut n_space: usize) -> usize {
        // Consume any common prefix between the number of spaces we
        // want and the number of unscanned tab-introduced spaces.
        let n_from_remaining = self.spaces_remaining.min(n_space);
        self.spaces_remaining -= n_from_remaining;
        n_space -= n_from_remaining;

        while n_space > 0 && self.ix < self.bytes.len() {
            match self.bytes[self.ix] {
                b' ' => {
                    self.ix += 1;
                    n_space -= 1;
                }
                b'\t' => {
                    let spaces = 4 - (self.ix - self.tab_start) % 4;
                    self.ix += 1;
                    self.tab_start = self.ix;
                    let n = spaces.min(n_space);
                    n_space -= n;

                    // Record the unscanned portion of the tab.
                    self.spaces_remaining = spaces - n;
                }
                _ => break,
            }
        }
        n_space
    }

    /// Scan all available ASCII whitespace (not including eol).
    pub(crate) fn scan_all_space(&mut self) {
        self.spaces_remaining = 0;
        self.ix += self.bytes[self.ix..]
            .iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count();
    }

    /// Determine whether we're at end of line (includes end of file).
    pub(crate) fn is_at_eol(&self) -> bool {
        self.bytes
            .get(self.ix)
            .map(|&c| c == b'\r' || c == b'\n')
            .unwrap_or(true)
    }

    fn scan_ch(&mut self, c: u8) -> bool {
        if self.ix < self.bytes.len() && self.bytes[self.ix] == c {
            self.ix += 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn scan_blockquote_marker(&mut self) -> bool {
        if self.scan_ch(b'>') {
            let _ = self.scan_space(1);
            true
        } else {
            false
        }
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
        let save = self.clone();
        if self.scan_ch(b':') {
            let save = self.clone();
            if self.scan_space(5) {
                *self = save;
                Some(indent + 1 + self.scan_space_upto(1))
            } else {
                *self = save;
                Some(indent + 1 + self.scan_space_upto(5))
            }
        } else {
            *self = save;
            None
        }
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
        let save = self.clone();
        if self.ix < self.bytes.len() {
            let c = self.bytes[self.ix];
            if c == b'-' || c == b'+' || c == b'*' {
                if self.ix >= self.min_hrule_offset {
                    // there could be an hrule here
                    if let Err(min_offset) = scan_hrule(&self.bytes[self.ix..]) {
                        self.min_hrule_offset = min_offset;
                    } else {
                        *self = save;
                        return None;
                    }
                }
                self.ix += 1;
                if self.scan_space(1) || self.is_at_eol() {
                    return self.finish_list_marker(c, 0, indent + 2);
                }
            } else if c.is_ascii_digit() {
                let start_ix = self.ix;
                let mut ix = self.ix + 1;
                let mut val = u64::from(c - b'0');
                while ix < self.bytes.len() && ix - start_ix < 10 {
                    let c = self.bytes[ix];
                    ix += 1;
                    if c.is_ascii_digit() {
                        val = val * 10 + u64::from(c - b'0');
                    } else if c == b')' || c == b'.' {
                        self.ix = ix;
                        if self.scan_space(1) || self.is_at_eol() {
                            return self.finish_list_marker(c, val, indent + 1 + ix - start_ix);
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        *self = save;
        None
    }

    fn finish_list_marker(
        &mut self,
        c: u8,
        start: u64,
        mut indent: usize,
    ) -> Option<(u8, u64, usize)> {
        let save = self.clone();

        // skip the rest of the line if it's blank
        if scan_blank_line(&self.bytes[self.ix..]).is_some() {
            return Some((c, start, indent));
        }

        let post_indent = self.scan_space_upto(4);
        if post_indent < 4 {
            indent += post_indent;
        } else {
            *self = save;
        }
        Some((c, start, indent))
    }

    pub(crate) fn bytes_scanned(&self) -> usize {
        self.ix
    }

    pub(crate) fn remaining_space(&self) -> usize {
        self.spaces_remaining
    }
}

pub(crate) fn is_ascii_whitespace_no_nl(c: u8) -> bool {
    c == b'\t' || c == 0x0b || c == 0x0c || c == b' '
}

pub(crate) fn scan_whitespace_no_nl(data: &[u8]) -> usize {
    scan_while(data, is_ascii_whitespace_no_nl)
}

pub(crate) fn scan_while<F>(data: &[u8], mut f: F) -> usize
where
    F: FnMut(u8) -> bool,
{
    data.iter().take_while(|&&c| f(c)).count()
}

pub(crate) fn scan_eol(bytes: &[u8]) -> Option<usize> {
    match *bytes {
        [] => Some(0),
        [b'\n', ..] => Some(1),
        [b'\r', b'\n', ..] => Some(2),
        [b'\r', ..] => Some(1),
        _ => None,
    }
}

pub(crate) fn scan_blank_line(bytes: &[u8]) -> Option<usize> {
    let i = scan_whitespace_no_nl(bytes);
    scan_eol(&bytes[i..]).map(|n| i + n)
}

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
