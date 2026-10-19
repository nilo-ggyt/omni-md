//! 匿名替换记录只保存范围；literal arena 属于外层动作，reader 不拥有其借用目标。

use std::borrow::Cow;
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::rc::Rc;

use super::{Found, ReplacementTemplate, TemplateToken};
use omni_markdown_core::{
    MarkdownDocumentBufferError, MarkdownReplacement, MarkdownReplacementPart,
    MarkdownTextSelection,
};

const MAX_SPOOL_BYTES: u64 = 256 * 1_024 * 1_024;
const IO_BYTES: usize = 32 * 1_024;

/// 每个 match 写入主范围和模板实际引用的去重 capture 槽；不复制相同 replacement 字节。
pub(super) struct ReplacementSpool<'a> {
    file: BufWriter<File>,
    template: &'a ReplacementTemplate,
    bytes: u64,
    count: usize,
    inserted: usize,
}

impl<'a> ReplacementSpool<'a> {
    pub(super) fn new(template: &'a ReplacementTemplate) -> Result<Self, String> {
        let file = tempfile::tempfile().map_err(|_| "无法创建匿名替换事务")?;
        let metadata = file.metadata().map_err(|_| "无法验证替换事务")?;
        if metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 0 {
            // 不接受共享访问或有名字的临时记录，失败发生在任何 canonical 编辑之前。
            return Err("替换事务未满足私有匿名存储要求".into());
        }
        Ok(Self {
            file: BufWriter::with_capacity(IO_BYTES, file),
            template,
            bytes: 0,
            count: 0,
            inserted: 0,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_SPOOL_BYTES)
            .ok_or("替换事务超过 256 MiB 临时记录上限")?;
        self.file.write_all(bytes).map_err(|_| "无法写入替换事务")?;
        self.bytes = total;
        Ok(())
    }

    /// 任一失败使调用方丢弃整个 spool；不能继续使用部分 header 或发布部分替换。
    pub(super) fn push(&mut self, found: &Found) -> Result<(), String> {
        self.write(&(found.range.start as u64).to_le_bytes())?;
        self.write(&(found.range.end as u64).to_le_bytes())?;
        for &capture in &self.template.captures {
            // 同一 capture 被模板引用多次也只写一个槽；未参与分支以单字节标记编码。
            match found.captures.get(capture).ok_or("替换捕获记录不完整")? {
                Some(range) => {
                    // 仅记录旧 snapshot 坐标；core 最终再检查范围及 scalar 边界。
                    self.write(&[1])?;
                    self.write(&(range.start as u64).to_le_bytes())?;
                    self.write(&(range.end as u64).to_le_bytes())?;
                }
                None => self.write(&[0])?, // 缺席与空 capture 不同，保留 baseline undefined 语义。
            }
        }
        let mut inserted = 0usize;
        for token in &self.template.tokens {
            let units = match token {
                TemplateToken::Literal { utf16, .. } => *utf16, // 宽度在解析时一次计算。
                TemplateToken::Capture(slot) => {
                    // slot 和输入 capture 的映射只读固定模板；没有每个 match 的临时 Vec。
                    let capture = self.template.captures[*slot];
                    found.captures[capture].as_ref().map_or(9, Range::len)
                }
            };
            inserted = inserted
                .checked_add(units)
                .ok_or("替换内容超过 UTF-16 长度范围")?;
        }
        self.inserted = self
            .inserted
            .checked_add(inserted)
            .ok_or("替换内容超过 UTF-16 长度范围")?;
        self.count = self.count.checked_add(1).ok_or("替换次数超过有效范围")?;
        Ok(())
    }

    pub(super) fn inserted_utf16(&self) -> usize {
        self.inserted
    }
    pub(super) fn count(&self) -> usize {
        self.count
    }

    /// 同一模板复用一个 capture scratch，坐标映射不保留全量 match/delta 数组。
    pub(super) fn map_selection(
        &mut self,
        before: MarkdownTextSelection,
    ) -> Result<MarkdownTextSelection, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法重置替换事务的读取位置")?;
        // seek 已 flush，底层 File 在 reader 生命周期内只读，不绕开未提交的写缓冲。
        let mut reader = BufReader::with_capacity(IO_BYTES, self.file.get_mut());
        let mut captures = vec![None; self.template.captures.len()];
        let mut after = before;
        let mut delta = 0isize;
        for _ in 0..self.count {
            let range =
                read_record(&mut reader, &mut captures).map_err(|_| "无法读取替换后的选区映射")?;
            let inserted = self.template.inserted_utf16(&captures)?;
            let removed = range
                .end
                .checked_sub(range.start)
                .ok_or("替换后的选区超过有效范围")?;
            let change = isize::try_from(inserted)
                .ok()
                .and_then(|inserted| {
                    isize::try_from(removed)
                        .ok()
                        .and_then(|removed| inserted.checked_sub(removed))
                })
                .ok_or("替换后的选区超过有效范围")?;
            for (original, mapped) in [
                (before.anchor_utf16, &mut after.anchor_utf16),
                (before.head_utf16, &mut after.head_utf16),
            ] {
                if original >= range.start {
                    // 旧坐标位于替换内时关联新片段末尾；之后的位置跟随累计长度变化。
                    let base = if original <= range.end {
                        range.start.checked_add(inserted)
                    } else {
                        original.checked_add_signed(change)
                    }
                    .ok_or("替换后的选区超过有效范围")?;
                    *mapped = base
                        .checked_add_signed(delta)
                        .ok_or("替换后的选区超过有效范围")?;
                }
            }
            delta = delta
                .checked_add(change)
                .ok_or("替换后的选区超过有效范围")?;
        }
        Ok(after)
    }

    /// template owner 独立于返回值且活得更久；没有自引用 reader 或 unsafe lifetime 延长。
    pub(super) fn into_replacements(mut self) -> Result<SpoolReplacements<'a>, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "无法重置替换事务的读取位置")?;
        let file = self.file.into_inner().map_err(|_| "无法完成替换事务")?;
        Ok(SpoolReplacements {
            shared: Rc::new(SpoolReader {
                template: self.template,
                state: RefCell::new(ReaderState {
                    file: BufReader::with_capacity(IO_BYTES, file),
                    captures: vec![None; self.template.captures.len()],
                    pending_parts: 0,
                    failed: false,
                }),
            }),
            remaining: self.count,
        })
    }
}

struct ReaderState {
    file: BufReader<File>,
    captures: Vec<Option<Range<usize>>>,
    pending_parts: usize,
    failed: bool,
}
struct SpoolReader<'a> {
    template: &'a ReplacementTemplate,
    state: RefCell<ReaderState>,
}
pub(super) struct SpoolReplacements<'a> {
    shared: Rc<SpoolReader<'a>>,
    remaining: usize,
}
pub(super) struct SpoolParts<'a> {
    shared: Rc<SpoolReader<'a>>,
    next: usize,
}

impl<'a> Iterator for SpoolReplacements<'a> {
    type Item = Result<MarkdownReplacement<SpoolParts<'a>>, MarkdownDocumentBufferError>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut state = self.shared.state.borrow_mut();
        if self.remaining == 0 || state.failed {
            // 消费完成或任一先前错误之后，流没有重新开始或丢弃错误项的权限。
            return None;
        }
        if state.pending_parts != 0 {
            // 上一 parts 未消费，禁止覆盖同一 capture scratch 并误读下一 header。
            state.failed = true;
            return Some(Err(
                MarkdownDocumentBufferError::ReplacementInputUnavailable,
            ));
        }
        let ReaderState { file, captures, .. } = &mut *state;
        match read_record(file, captures) {
            Ok(range_utf16) => {
                // 只在完整 header/capture 读取后发布此 replacement，借用在 core 处理前结束。
                self.remaining -= 1;
                state.pending_parts = self.shared.template.tokens.len();
                Some(Ok(MarkdownReplacement {
                    range_utf16,
                    parts: SpoolParts {
                        shared: Rc::clone(&self.shared),
                        next: 0,
                    },
                }))
            }
            Err(error) => {
                // 短读/损坏使整个 canonical 事务失败，不能返回已消费前缀为成功。
                state.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl<'a> Iterator for SpoolParts<'a> {
    type Item = Result<MarkdownReplacementPart<'a>, MarkdownDocumentBufferError>;
    fn next(&mut self) -> Option<Self::Item> {
        let template: &'a ReplacementTemplate = self.shared.template;
        let token = template.tokens.get(self.next)?;
        let mut state = self.shared.state.borrow_mut();
        if state.failed {
            return None; // 外层曾在未完整消费时推进，旧 parts 不再拥有读取权限。
        }
        self.next += 1;
        state.pending_parts -= 1;
        Some(Ok(match token {
            TemplateToken::Literal { bytes, .. } => {
                // 借用目标属于外层 template；不借 RefCell 内容，不分配 String 或复制 literal。
                MarkdownReplacementPart::Literal(Cow::Borrowed(&template.arena[bytes.clone()]))
            }
            TemplateToken::Capture(slot) => match &state.captures[*slot] {
                Some(range) => MarkdownReplacementPart::SourceRange(range.clone()), // 原始正文仍由旧 canonical root 读取。
                None => MarkdownReplacementPart::Literal(Cow::Borrowed("undefined")), // 缺席 capture 保留既有模板结果。
            },
        }))
    }
}

impl Drop for SpoolParts<'_> {
    fn drop(&mut self) {
        if self.next != self.shared.template.tokens.len() {
            // 提前丢弃 parts 永久封闭该 reader；剩余 capture 没有变成下一记录的权限。
            self.shared.state.borrow_mut().failed = true;
        }
    }
}

fn read_usize(reader: &mut impl Read) -> Result<usize, MarkdownDocumentBufferError> {
    let mut bytes = [0; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)?;
    usize::try_from(u64::from_le_bytes(bytes))
        .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)
}

/// scratch 长度来自固定 template，记录不能自行声明巨型 Vec 或更改下一条记录布局。
fn read_record(
    reader: &mut impl Read,
    captures: &mut [Option<Range<usize>>],
) -> Result<Range<usize>, MarkdownDocumentBufferError> {
    let range = read_usize(reader)?..read_usize(reader)?;
    for capture in captures {
        let mut tag = [0];
        reader
            .read_exact(&mut tag)
            .map_err(|_| MarkdownDocumentBufferError::ReplacementInputUnavailable)?;
        *capture = match tag[0] {
            0 => None,                                           // 合法的未参与分支没有正文坐标。
            1 => Some(read_usize(reader)?..read_usize(reader)?), // range/scalar 合法性仍由 core 的一次事务验证。
            _ => return Err(MarkdownDocumentBufferError::ReplacementInputUnavailable), // 私有记录损坏也不忽略或重同步。
        };
    }
    Ok(range)
}

#[cfg(test)]
mod tests;
