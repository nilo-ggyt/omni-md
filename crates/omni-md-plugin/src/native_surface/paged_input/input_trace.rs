//! 正式分页输入只保原有限追踪payload；观察不读取正文、不持Doc或Native资源。

use super::*;
use crate::native_input_trace::{NativeInputTrace, SharedNativeInputTrace};
use crate::native_surface::input_trace::native_input_trace_now;
use crate::{NativeInputTraceAccessError, NativeInputTraceDocument, NativeInputTraceRejection};

impl NativeMarkdownPagedInput {
    /// 真实Source客户的arm只接当前状态；组合中、已关闭或借用中的输入都不能另起证据区间。
    pub(in crate::native_surface) fn arm_input_trace(
        &self,
        ticks: u64,
        numer: u32,
        denom: u32,
    ) -> std::result::Result<(), NativeInputTraceAccessError> {
        if self.closed.get() {
            return Err(NativeInputTraceAccessError::NoEditor);
        }
        if self.busy.get() {
            return Err(NativeInputTraceAccessError::Busy);
        }
        if self.is_cell() {
            return Err(NativeInputTraceAccessError::SourceRequired);
        } // 局部格的版本与canonical不同，不能给Source追踪混入另一坐标域。
        let state = self.state().map_err(|cause| match cause {
            NativeMarkdownPagedInputError::Closed
            | NativeMarkdownPagedInputError::DocumentReleased => {
                NativeInputTraceAccessError::NoEditor
            }
            _ => NativeInputTraceAccessError::Busy,
        })?;
        let (document, _) = self.input_trace_observation();
        let document = document.ok_or(NativeInputTraceAccessError::Busy)?;
        let previous = self.input_trace();
        let trace = previous
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(NativeInputTrace::default())));
        trace
            .borrow_mut()
            .arm(ticks, numer, denom, document, state.marked.is_some())?;
        if previous.is_none() {
            // 失败arm不会留下新heap；成功后唯一Input接管，View和scope都只在同步栈内借它。
            self.input_trace.replace(Some(trace));
        }
        Ok(())
    }

    /// 复制有限记录器句柄时不跨Native或Doc借用；未arm和close后的热路径均立即返回None。
    pub(in crate::native_surface) fn input_trace(&self) -> Option<SharedNativeInputTrace> {
        self.input_trace.borrow().clone()
    }

    /// 当前Input存在但尚未arm时保持NotArmed；关闭已取走payload时才报告NoEditor。
    pub(in crate::native_surface) fn drain_input_trace(
        &self,
    ) -> std::result::Result<crate::NativeInputTraceSnapshot, NativeInputTraceAccessError> {
        if self.closed.get() {
            return Err(NativeInputTraceAccessError::NoEditor);
        }
        let trace = self
            .input_trace()
            .ok_or(NativeInputTraceAccessError::NotArmed)?;
        let snapshot = trace.borrow_mut().drain()?;
        Ok(snapshot)
    }

    /// 观察原NSTextInputClient使用的marked真值和O(1)canonical计量，不执行Source读取或外部回调。
    pub(in crate::native_surface) fn input_trace_observation(
        &self,
    ) -> (Option<NativeInputTraceDocument>, bool) {
        let document = self.document_identity().upgrade().and_then(|original| {
            let document = original.try_borrow().ok()?;
            Some(NativeInputTraceDocument {
                generation: document.generation(),
                utf8_bytes: document.byte_len(),
                utf16_units: document.utf16_len(),
            })
        });
        (document, self.marked.get().is_some())
    }

    /// Core成功的原typed回执先于任何可重入显示后果记录；不把显示失败改写成canonical拒绝。
    pub(super) fn record_input_trace_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        document: &MarkdownDocumentBuffer,
    ) {
        let Some(trace) = self.input_trace() else {
            return;
        };
        let removed = commit
            .invalidation
            .replaced_utf16_range
            .as_ref()
            .map_or(0, Range::len);
        let inserted = removed
            .checked_add_signed(commit.invalidation.change_in_utf16_length)
            .expect("原Core编辑回执的移除量和长度差必须组成实际新增UTF16量");
        // 原publish已经拥有Doc租约；只复制三项计量，不重借、不触发Source或Native回调。
        let document = NativeInputTraceDocument {
            generation: document.generation(),
            utf8_bytes: document.byte_len(),
            utf16_units: document.utf16_len(),
        };
        let ticks = native_input_trace_now();
        trace
            .borrow_mut()
            .canonical(ticks, Some(document), removed, inserted, None);
    }

    /// 仅记录原回调的有限拒绝类别；没有错误字符串、正文、路径或额外Source查询。
    pub(in crate::native_surface) fn reject_input_trace(
        &self,
        rejection: NativeInputTraceRejection,
    ) {
        let Some(trace) = self.input_trace() else {
            return;
        };
        let (document, _) = self.input_trace_observation();
        let ticks = native_input_trace_now();
        trace
            .borrow_mut()
            .canonical(ticks, document, 0, 0, Some(rejection));
    }
}
