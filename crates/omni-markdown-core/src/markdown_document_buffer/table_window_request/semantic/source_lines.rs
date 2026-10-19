//! 当前窗口的有限真实行复制；允许frame切在UTF8内部，绝不复制省略区间或整表。

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    TABLE_WINDOW_MAX_BYTES, TABLE_WINDOW_MAX_UTF16,
};

#[derive(Clone, Debug)]
pub(super) struct LineSpec {
    pub bytes: Range<usize>,
    pub utf16: Range<usize>,
    pub source_line: usize,
    pub ordinal: usize,
    pub prefix: usize,
    pub delimiter: bool,
}

pub(super) struct OwnedLine {
    pub spec: LineSpec,
    pub text: String,
}
impl OwnedLine {
    pub fn borrowed(&self) -> MarkdownTableSourceLine<'_> {
        MarkdownTableSourceLine {
            text: &self.text,
            byte_start: self.spec.bytes.start,
            utf16_start: self.spec.utf16.start,
            source_line: self.spec.source_line,
            row_ordinal: self.spec.ordinal,
            container_prefix_bytes: self.spec.prefix,
        }
    }
}

pub(super) struct LineCopy {
    lines: Vec<OwnedLine>,
    index: usize,
    copied_current: usize,
    pending: [u8; 4],
    pending_len: usize,
}
impl LineCopy {
    pub fn new(ticket: &MarkdownTableStructuralWindow) -> Result<Self, Error> {
        let h = ticket.header;
        let mut specs = Vec::with_capacity(ticket.body.len() + 2);
        specs.push(LineSpec {
            bytes: h.header_byte_start..h.header_byte_end,
            utf16: ticket.header_utf16.clone(),
            source_line: h.header_source_line,
            ordinal: 0,
            prefix: h.header_prefix_bytes,
            delimiter: false,
        });
        specs.push(LineSpec {
            bytes: h.separator_byte_start..h.separator_byte_end,
            utf16: ticket.delimiter_utf16.clone(),
            source_line: h.separator_source_line,
            ordinal: 0,
            prefix: h.separator_prefix_bytes,
            delimiter: true,
        });
        for row in &ticket.body {
            // 已签发成员坐标，含真实EOL/prefix；没有任何window位置+base的ordinal推算。
            specs.push(LineSpec {
                bytes: row.byte_start..row.byte_end,
                utf16: row.utf16_start..row.utf16_end,
                source_line: row.source_line,
                ordinal: row.row_ordinal,
                prefix: row.container_prefix_bytes,
                delimiter: false,
            });
        }
        let mut bytes = 0usize;
        let mut units = 0usize;
        for spec in &specs {
            bytes = bytes
                .checked_add(
                    spec.bytes
                        .end
                        .checked_sub(spec.bytes.start)
                        .ok_or(Error::InvalidSource)?,
                )
                .ok_or(Error::SourceLimit)?;
            units = units
                .checked_add(
                    spec.utf16
                        .end
                        .checked_sub(spec.utf16.start)
                        .ok_or(Error::InvalidSource)?,
                )
                .ok_or(Error::SourceLimit)?;
            if bytes > TABLE_WINDOW_MAX_BYTES
                || units > TABLE_WINDOW_MAX_UTF16
                || spec.prefix > spec.bytes.len()
            {
                // 当前批全部真实源容量先准入，再创建任何正文String；H context也收费。
                return Err(Error::SourceLimit);
            }
        }
        Ok(Self {
            lines: specs
                .into_iter()
                .map(|spec| OwnedLine {
                    spec,
                    text: String::new(),
                })
                .collect(),
            index: 0,
            copied_current: 0,
            pending: [0; 4],
            pending_len: 0,
        })
    }

    pub fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
    ) -> Result<(SemanticWork, bool), Error> {
        if self.index == self.lines.len() {
            return Ok((SemanticWork::default(), true));
        }
        let tree = tree_height(document);
        // from_utf8遇到frame尾未完成scalar时会再次检查valid prefix；carry还可能重验至多4B。
        // 分配四份byte额度并保96固定余量，覆盖真实读/写/每次UTF8校验输入，不仅是distinct源byte。
        let available = budget.checked_sub(tree + 96).ok_or(Error::SourceLimit)? / 4;
        if available == 0 {
            return Err(Error::SourceLimit);
        }
        let line = &mut self.lines[self.index];
        let next = line.spec.bytes.start + self.copied_current;
        let wanted = available.min(line.spec.bytes.end - next);
        let mut work = SemanticWork {
            source_tree_steps: tree,
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        if line.text.capacity() == 0 {
            // 分配总payload已在new按整批核准；此处仅为当前真实行预留，不给所有表行预留。
            line.text.reserve_exact(line.spec.bytes.len());
            work.allocated_source_capacity = line.text.capacity();
        }
        let frame = document
            .table_source_frame(next, wanted.max(1))
            .map_err(|_| Error::InvalidSource)?;
        let bytes = &frame.bytes[..frame.bytes.len().min(wanted)];
        if wanted > 0 && bytes.is_empty() {
            return Err(Error::InvalidSource);
        }
        work.source_bytes = bytes.len();
        let before = line.text.len();
        let mut at = 0;
        while self.pending_len > 0 && at < bytes.len() {
            // carry最多3个未完成字节；补成一个scalar后立即写入，不保存任意长行尾。
            self.pending[self.pending_len] = bytes[at];
            self.pending_len += 1;
            at += 1;
            work.utf8_validation_bytes += self.pending_len;
            match std::str::from_utf8(&self.pending[..self.pending_len]) {
                Ok(text) => {
                    line.text.push_str(text);
                    self.pending_len = 0;
                }
                Err(error) if error.error_len().is_none() && self.pending_len < 4 => {}
                Err(_) => return Err(Error::InvalidSource),
            }
        }
        if at < bytes.len() {
            work.utf8_validation_bytes += bytes.len() - at;
            match std::str::from_utf8(&bytes[at..]) {
                Ok(text) => line.text.push_str(text),
                Err(error) if error.error_len().is_none() => {
                    // 仅frame尾的合法未完成scalar可续接；真实非法UTF8不能当作输入不足。
                    let prefix_end = at + error.valid_up_to();
                    work.utf8_validation_bytes += prefix_end - at;
                    line.text.push_str(
                        std::str::from_utf8(&bytes[at..prefix_end])
                            .map_err(|_| Error::InvalidSource)?,
                    );
                    let tail = &bytes[prefix_end..];
                    if tail.len() > 3 {
                        return Err(Error::InvalidSource);
                    }
                    self.pending[..tail.len()].copy_from_slice(tail);
                    self.pending_len = tail.len();
                }
                Err(_) => return Err(Error::InvalidSource),
            }
        }
        work.copied_source_bytes = line.text.len() - before;
        self.copied_current += bytes.len();
        if self.copied_current == line.spec.bytes.len() {
            if self.pending_len != 0 || line.text.len() != line.spec.bytes.len() {
                return Err(Error::InvalidSource);
            }
            // 真实完整行复制完才转下一行；不把frame结束当成EOL或构造人工newline。
            self.index += 1;
            self.copied_current = 0;
        }
        frame.finish().map_err(|_| Error::InvalidSource)?;
        Ok((work, self.index == self.lines.len()))
    }

    pub fn into_lines(self) -> Vec<OwnedLine> {
        self.lines
    }
}
