//! 工作区冻结候选只借出有限字节；UTF8残余跨窗口保留，不能用命中提前结束校验。
use std::{cell::Cell, ops::Range};

use crate::original_backing::{ORIGINAL_READ_BYTES, OriginalBacking};

use super::MarkdownWorkspaceError;

type Result<T> = std::result::Result<T, MarkdownWorkspaceError>;

/// 此owner没有全片借用入口。窗口回收失败永久撤权，最终close检查真正的映射归还。
pub(in super::super) struct BoundedMarkdownText {
    backing: Option<OriginalBacking>,
    bytes: usize,
    validated: Cell<bool>,
}
impl BoundedMarkdownText {
    pub(super) fn new(backing: Option<OriginalBacking>, bytes: usize) -> Result<Self> {
        if backing.as_ref().map_or(0, OriginalBacking::len) != bytes {
            // capture的空源和真实extent必须一致，不把缺失backing当成空正文。
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        Ok(Self {
            backing,
            bytes,
            validated: Cell::new(false),
        })
    }

    pub(in super::super) fn len(&self) -> usize {
        self.bytes
    }

    /// 每次至多4KiB；callback结果只在同一lease真实回收成功且原查询仍有效时返回。
    pub(in super::super) fn with_bytes<R>(
        &self,
        range: Range<usize>,
        is_current: &impl Fn() -> bool,
        visit: impl FnOnce(&[u8]) -> R,
    ) -> Result<R> {
        if !is_current() {
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        if range.start > range.end || range.end > self.bytes || range.len() > ORIGINAL_READ_BYTES {
            // 在借出任何源字节之前拒绝非法大范围，不退回普通整Mmap。
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        let output = match &self.backing {
            Some(backing) => backing
                .with_bytes(range, visit)
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?,
            None => visit(&[]), // 只有经过new验证的真实空源才允许没有映射。
        };
        if !is_current() {
            // 纯Rust callback也可能撤销请求；其部分结果不能在回收后被发布。
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        Ok(output)
    }

    /// 全候选验证和消费者共享每个原窗口；EOF前的任何无效序列都否定整个结果。
    pub(in super::super) fn visit_validated_bytes(
        &self,
        is_current: &impl Fn() -> bool,
        mut visit: impl FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut utf8 = Utf8Continuation::default();
        let mut start = 0;
        while start < self.bytes {
            let end = start.saturating_add(ORIGINAL_READ_BYTES).min(self.bytes);
            self.with_bytes(start..end, is_current, |bytes| {
                // 先检验本窗口；边界半标量只是候选，下一窗或真实EOF仍可使整次扫描失败。
                utf8.push(bytes)?;
                visit(start, bytes)
            })??;
            start = end;
        }
        if !is_current() {
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        utf8.finish()?;
        self.validated.set(true); // 只有真实EOF与完整标量都成功，才发布不可变源的校验事实。
        Ok(())
    }

    pub(in super::super) fn ensure_validated(&self, is_current: &impl Fn() -> bool) -> Result<()> {
        if self.validated.get() {
            if is_current() {
                return Ok(()); // 同一冻结源无需因摘要二次读取而重复校验全部正文。
            }
            return Err(MarkdownWorkspaceError::SearchSuperseded);
        }
        // 泛型检查者若只读取局部，也不能绕过原接口的整候选有效UTF8发布条件。
        self.visit_validated_bytes(is_current, |_, _| Ok(()))
    }

    /// 导入/复制只写出同一个冻结源的有限窗口；任一写入错误都停止且由外层guard回滚。
    pub(in super::super) fn copy_to(&self, output: &mut impl std::io::Write) -> Result<()> {
        self.visit_validated_bytes(&|| true, |_, bytes| {
            output
                .write_all(bytes)
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
        })
    }

    /// 复制原源的真实半开范围；只改变writer，不能取得或保留整段字节。
    pub(in super::super) fn copy_range_to(
        &self,
        range: Range<usize>,
        output: &mut impl std::io::Write,
    ) -> Result<()> {
        if range.start > range.end || range.end > self.bytes {
            return Err(MarkdownWorkspaceError::StorageUnavailable);
        }
        let mut start = range.start;
        while start < range.end {
            let end = start.saturating_add(ORIGINAL_READ_BYTES).min(range.end);
            self.with_bytes(start..end, &|| true, |bytes| {
                output
                    .write_all(bytes)
                    .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)
            })??;
            start = end;
        }
        Ok(())
    }

    pub(super) fn close(mut self) -> Result<()> {
        if let Some(backing) = self.backing.take() {
            // 归还失败优先于callback成功；Drop不能把晚到的unmap故障隐藏在已返回结果后。
            backing
                .close()
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
        }
        Ok(())
    }
}

/// 仅保存一个跨窗标量，不保留已验证前缀或整段文本镜像。
#[derive(Default)]
struct Utf8Continuation {
    bytes: [u8; 4],
    used: usize,
    needed: usize,
}
impl Utf8Continuation {
    fn push(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.used != 0 {
            let take = (self.needed - self.used).min(bytes.len());
            self.bytes[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used != self.needed {
                return Ok(()); // 下个窗口继续同一真实标量；EOF另行拒绝残余。
            }
            std::str::from_utf8(&self.bytes[..self.used])
                .map_err(|_| MarkdownWorkspaceError::InvalidUtf8Document)?;
            self.used = 0;
        }
        match std::str::from_utf8(bytes) {
            Ok(_) => Ok(()),
            Err(error) if error.error_len().is_none() => {
                // std已证明前缀有效，末尾只能是1..3个尚未完成的真实UTF8字节。
                let tail = &bytes[error.valid_up_to()..];
                self.needed = match tail[0] {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err(MarkdownWorkspaceError::InvalidUtf8Document),
                };
                self.bytes[..tail.len()].copy_from_slice(tail);
                self.used = tail.len();
                Ok(())
            }
            Err(_) => Err(MarkdownWorkspaceError::InvalidUtf8Document),
        }
    }
    fn finish(self) -> Result<()> {
        if self.used != 0 {
            Err(MarkdownWorkspaceError::InvalidUtf8Document)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
