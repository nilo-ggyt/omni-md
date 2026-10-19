//! 移动附件链接的有限源游标与匿名输出；旧grammar只作为测试oracle，不构造完整正文值。
use std::{
    io::{self, BufWriter, Seek, SeekFrom, Write},
    ops::Range,
};

use super::*;
use crate::original_backing::ORIGINAL_READ_BYTES;

type Result<T> = std::result::Result<T, MarkdownWorkspaceError>;

/// 真实完整输出已经flush并归零游标；匿名File在任何外层撤权或错误时直接Drop，不发布路径。
pub(in super::super) struct RewrittenMarkdown {
    pub(in super::super) file: File,
    pub(in super::super) byte_len: u64,
    pub(in super::super) revision: String,
}

/// 仅复制一个固定窗口到scratch；随机前瞻不保留token，换页不重新capture正文。
struct ByteCursor<'a> {
    source: &'a BoundedMarkdownText,
    bytes: [u8; ORIGINAL_READ_BYTES],
    start: usize,
    end: usize,
}
impl<'a> ByteCursor<'a> {
    fn new(source: &'a BoundedMarkdownText) -> Self {
        Self {
            source,
            bytes: [0; ORIGINAL_READ_BYTES],
            start: 0,
            end: 0,
        }
    }
    fn byte(&mut self, offset: usize) -> Result<Option<u8>> {
        if offset >= self.source.len() {
            return Ok(None); // 真正EOF没有虚构哨兵字符，所有grammar分支仍按原字节坐标判断。
        }
        if offset < self.start || offset >= self.end {
            self.start = offset / ORIGINAL_READ_BYTES * ORIGINAL_READ_BYTES;
            self.end = self
                .start
                .saturating_add(ORIGINAL_READ_BYTES)
                .min(self.source.len());
            self.source
                .with_bytes(self.start..self.end, &|| true, |bytes| {
                    self.bytes[..bytes.len()].copy_from_slice(bytes);
                })?;
        }
        Ok(Some(self.bytes[offset - self.start]))
    }
    fn line_end(&mut self, mut start: usize) -> Result<usize> {
        while start < self.source.len() {
            self.byte(start)?; // 只保证此固定窗口已加载，后面的搜索仍使用同scratch。
            let end = self.end;
            if let Some(local) = self.bytes[start - self.start..end - self.start]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                return Ok(start + local);
            }
            start = end;
        }
        Ok(self.source.len())
    }
    fn repeated(&mut self, mut start: usize, end: usize, expected: u8) -> Result<usize> {
        let origin = start;
        while start < end && self.byte(start)? == Some(expected) {
            start += 1; // 任意长度delimiter只累计计数，不在scratch外保留一份token。
        }
        Ok(start - origin)
    }
    fn starts_with(&mut self, start: usize, prefix: &[u8]) -> Result<bool> {
        let Some(end) = start
            .checked_add(prefix.len())
            .filter(|end| *end <= self.source.len())
        else {
            return Ok(false);
        };
        for (offset, expected) in (start..end).zip(prefix) {
            if self.byte(offset)? != Some(*expected) {
                return Ok(false); // 首个不同字节立即拒绝，保留原完整prefix匹配语义。
            }
        }
        Ok(true)
    }
    fn fence(&mut self, start: usize, end: usize) -> Result<Option<(u8, usize, usize)>> {
        let mut marker_start = start;
        while marker_start < end
            && marker_start - start < 4
            && self.byte(marker_start)? == Some(b' ')
        {
            marker_start += 1;
        }
        if marker_start - start > 3 {
            return Ok(None); // 四空格仍属于普通缩进文本，不能误入fence。
        }
        let Some(marker @ (b'`' | b'~')) = self.byte(marker_start)? else {
            return Ok(None);
        };
        let count = self.repeated(marker_start, end, marker)?;
        Ok((count >= 3).then_some((marker, count, marker_start + count)))
    }
    fn whitespace(&mut self, range: Range<usize>) -> Result<bool> {
        for offset in range {
            if !self
                .byte(offset)?
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                return Ok(false); // 与旧byte规则相同；只有ASCII空白可结束fence。
            }
        }
        Ok(true)
    }
    fn encoded_name(&mut self, start: usize, end: usize) -> Result<bool> {
        let mut offset = start;
        while offset < end
            && self.byte(offset)?.is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'%' | b'-' | b'.' | b'_' | b'~')
            })
        {
            offset += 1; // 不设URL长度上限，真实名字终点只由原grammar决定。
        }
        Ok(offset > start && self.byte(offset)? == Some(b')'))
    }
}

/// 一次真实write成功后才累计摘要和字节数；缓冲上限4KiB，与源长度无关。
struct Output {
    file: BufWriter<File>,
    digest: Sha256,
    bytes: u64,
}
impl Output {
    fn new() -> Result<Self> {
        let file = tempfile::tempfile().map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        Ok(Self {
            file: BufWriter::with_capacity(ORIGINAL_READ_BYTES, file),
            digest: Sha256::new(),
            bytes: 0,
        })
    }
    fn finish(mut self) -> Result<RewrittenMarkdown> {
        self.file
            .flush()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let mut file = self
            .file
            .into_inner()
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        let digest = self.digest.finalize();
        let mut revision = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(revision, "{byte:02x}");
        }
        Ok(RewrittenMarkdown {
            file,
            byte_len: self.bytes,
            revision,
        })
    }
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        let total = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| io::Error::other("附件改写输出长度溢出"))?;
        self.file.write_all(bytes)?;
        self.digest.update(bytes);
        self.bytes = total;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// 原相对深度未变或没有真实generated destination时不创建输出文件。
pub(in super::super) fn rewrite_moved_note_attachment_links_streamed(
    content: &BoundedMarkdownText,
    source_relative_path: &str,
    destination_relative_path: &str,
) -> Result<Option<RewrittenMarkdown>> {
    let source_prefix = markdown_attachment_relative_prefix(source_relative_path)?;
    let destination_prefix = markdown_attachment_relative_prefix(destination_relative_path)?;
    content.ensure_validated(&|| true)?; // 独立调用也必须先证明整候选UTF8，不能只认可命中附近。
    if source_prefix == destination_prefix {
        return Ok(None);
    }
    let mut cursor = ByteCursor::new(content);
    let mut output = None::<Output>;
    let mut copied_until = 0;
    let mut line_start = 0;
    let mut inline_ticks = None;
    let mut fence = None;
    while line_start < content.len() {
        let line_end = cursor.line_end(line_start)?;
        if inline_ticks.is_none()
            && let Some((marker, count, marker_end)) = cursor.fence(line_start, line_end)?
        {
            if let Some((opening, opening_count)) = fence {
                if marker == opening
                    && count >= opening_count
                    && cursor.whitespace(marker_end..line_end)?
                {
                    fence = None; // 只有原同marker、足够长度和空白尾巴才关闭，整行保持不透明。
                }
            } else {
                fence = Some((marker, count)); // opening suffix不作额外约束，与旧grammar一致。
            }
            line_start = line_end.saturating_add(1);
            continue;
        }
        if fence.is_some() {
            line_start = line_end.saturating_add(1);
            continue; // fence内全部字节留在后续source span，不解析链接或inline状态。
        }
        let mut index = line_start;
        while index < line_end {
            match cursor.byte(index)? {
                Some(b'\\') if inline_ticks.is_none() => {
                    index = (index + 2).min(line_end); // 转义跳一个原byte，不能跨行吞下一行标记。
                }
                Some(b'`') => {
                    let count = cursor.repeated(index, line_end, b'`')?;
                    match inline_ticks {
                        Some(opening) if opening == count => inline_ticks = None,
                        None => inline_ticks = Some(count),
                        Some(_) => {} // 不同长度是代码内的字面内容，状态跨真实行延续。
                    }
                    index += count;
                }
                Some(b']')
                    if inline_ticks.is_none()
                        && cursor.byte(index + 1)? == Some(b'(')
                        && cursor.starts_with(index + 2, source_prefix.as_bytes())? =>
                {
                    let destination_start = index + 2;
                    let name_start = destination_start + source_prefix.len();
                    if cursor.encoded_name(name_start, line_end)? {
                        if output.is_none() {
                            output = Some(Output::new()?); // 真替换才取得匿名文件，失败不生成部分发布结果。
                        }
                        let writer = output.as_mut().expect("上支已经取得唯一output");
                        content.copy_range_to(copied_until..destination_start, writer)?;
                        writer
                            .write_all(destination_prefix.as_bytes())
                            .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
                        copied_until = name_start;
                        index = name_start; // 文件名和')'继续按原扫描顺序处理，不跳过潜在后续syntax。
                    } else {
                        index += 1; // 空名、未闭合或不合法字符保持原字节，仍继续扫描本行。
                    }
                }
                _ => index += 1, // 非grammar字节包括UTF8续字节只推进，输出由原source span保存。
            }
        }
        line_start = line_end.saturating_add(1);
    }
    if let Some(mut writer) = output {
        content.copy_range_to(copied_until..content.len(), &mut writer)?;
        Ok(Some(writer.finish()?))
    } else {
        Ok(None) // 没有实际替换，不分配匿名spool或完整正文副本。
    }
}

#[cfg(test)]
mod tests;
