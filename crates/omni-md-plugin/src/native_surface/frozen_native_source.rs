//! 冻结源的唯一封装入口：快照不向外泄漏，真实源资源先析构，家族claim最后结算。

use std::ops::Range;
use std::sync::{Arc, Weak};

use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownDocumentBufferError, MarkdownTextSnapshot,
};

use super::frozen_value_family::{FrozenValueClaim, FrozenValueFamily, FrozenValueFamilyError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FrozenReadKind {
    Character,
    Bulk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只携固定标量；非法请求也保留原参数和checked结果，不截断成另一个合法窗口。
pub(super) struct FrozenReadRequest {
    pub(super) kind: FrozenReadKind,
    pub(super) snapshot_utf16_len: usize,
    pub(super) receiver_start: usize,
    pub(super) receiver_length: usize,
    pub(super) local_start: usize,
    pub(super) requested_length: usize,
    pub(super) checked_global_start: Option<usize>,
    pub(super) checked_global_end: Option<usize>,
    pub(super) receiver_valid: bool,
    pub(super) request_valid: bool,
}

/// hook只能观察固定事实；不得捕获owner、调用Native或panic，生产authority由外部诊断sink负责。
pub(super) type FrozenReadHook = fn(FrozenReadRequest);

struct SourceData {
    // 字段按声明顺序真实析构；snapshot必须先于最后SourceClaim归还mmap/root/编辑段。
    snapshot: MarkdownTextSnapshot,
    generation: u64,
    read_hook: Option<FrozenReadHook>,
    claim: FrozenValueClaim,
}

#[derive(Clone)]
/// Clone只分享同一个完整SourceData；不会复制裸快照或制造另一份源claim。
pub(super) struct FrozenNativeSource {
    data: Arc<SourceData>,
}

#[derive(Clone)]
pub(super) struct FrozenNativeSourceWeak {
    data: Weak<SourceData>,
}

/// 只在已通知hook且完整验证后产生，私有快照借用不能从此令牌逃逸。
pub(super) struct FrozenNativeRead<'a> {
    snapshot: &'a MarkdownTextSnapshot,
    start: usize,
    length: usize,
}

impl FrozenNativeSource {
    pub(super) fn capture(
        document: &MarkdownDocumentBuffer,
        family: &Arc<FrozenValueFamily>,
        read_hook: Option<FrozenReadHook>,
    ) -> Result<Self, FrozenValueFamilyError> {
        // 封源先拒绝，不能先创建裸snapshot再试图补签原家族的来源。
        let claim = family.claim_source()?;
        let snapshot = document.text_snapshot();
        let generation = document.generation();
        Ok(Self {
            data: Arc::new(SourceData {
                snapshot,
                generation,
                read_hook,
                claim,
            }),
        })
    }

    pub(super) fn downgrade(&self) -> FrozenNativeSourceWeak {
        FrozenNativeSourceWeak {
            data: Arc::downgrade(&self.data),
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.data.generation // 只是捕获时的原代次，不能当作跨文档身份或重新签发live权限。
    }

    pub(super) fn utf16_len(&self) -> usize {
        self.data.snapshot.utf16_len()
    }

    pub(super) fn text_checking_paragraph_range(
        &self,
        range: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        // 仅代理冻结树的段落元数据查询，不授予或暴露裸snapshot、全文String或live文档权限。
        self.data.snapshot.text_checking_paragraph_range(range)
    }

    pub(super) fn claim_string_value(&self) -> Result<FrozenValueClaim, FrozenValueFamilyError> {
        self.data.claim.claim_string_value()
    }

    pub(super) fn request_read(
        &self,
        kind: FrozenReadKind,
        receiver_start: usize,
        receiver_length: usize,
        local_start: usize,
        requested_length: usize,
    ) -> Result<FrozenNativeRead<'_>, MarkdownDocumentBufferError> {
        let snapshot_utf16_len = self.utf16_len();
        let receiver_end = receiver_start.checked_add(receiver_length);
        let local_end = local_start.checked_add(requested_length);
        let checked_global_start = receiver_start.checked_add(local_start);
        let checked_global_end =
            checked_global_start.and_then(|start| start.checked_add(requested_length));
        let receiver_valid = receiver_end.is_some_and(|end| end <= snapshot_utf16_len);
        let request_valid = receiver_valid
            && local_end.is_some_and(|end| end <= receiver_length)
            && checked_global_end.is_some_and(|end| end <= snapshot_utf16_len);
        if let Some(hook) = self.data.read_hook {
            // 非法请求在异常之前同样交付原事实；只传fn和标量，不借文档或Native业务对象。
            hook(FrozenReadRequest {
                kind,
                snapshot_utf16_len,
                receiver_start,
                receiver_length,
                local_start,
                requested_length,
                checked_global_start,
                checked_global_end,
                receiver_valid,
                request_valid,
            });
        }
        if !request_valid {
            // 不形成原始输出slice，也不发放可读令牌；接收者外的source字符不能被借机读取。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        Ok(FrozenNativeRead {
            snapshot: &self.data.snapshot,
            start: checked_global_start.ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?,
            length: requested_length,
        })
    }
}

impl FrozenNativeSourceWeak {
    pub(super) fn upgrade(&self) -> Option<FrozenNativeSource> {
        // 升级沿用同一Data/SourceClaim；即使seal后也不凭Weak制造第二份snapshot。
        self.data.upgrade().map(|data| FrozenNativeSource { data })
    }
}

impl FrozenNativeRead<'_> {
    #[cfg(test)]
    pub(super) fn global_start(&self) -> usize {
        self.start
    }

    pub(super) fn length(&self) -> usize {
        self.length
    }

    pub(super) fn code_unit(&self) -> Result<u16, MarkdownDocumentBufferError> {
        if self.length != 1 {
            // 字符primitive只能消费真实一个UTF-16单元，不能把bulk请求缩成单字符冒充完成。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.snapshot.utf16_code_unit_at(self.start)
    }

    pub(super) fn copy_to(
        &self,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        if destination.len() != self.length {
            // 输出必须覆盖原请求全部单位；不接受短缓冲、不截断或补另一个范围。
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.snapshot.copy_utf16_code_units(self.start, destination)
    }
}
