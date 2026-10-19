//! 原 Find 动作的巨型代码段背景验收；只保存原 Native Weak、文档 Weak 和有限源坐标。

use super::*;
use crate::native_surface::preview_blocks::diagnostic::{
    CodePaintReceipt, MAX_PROBE_CODE_REQUEST_UTF16, NativeMarkdownPreviewBlockProbeSnapshot,
    code_paint_sequence, code_snapshot_matches, matching_code_paint, observe_code_paint,
};

pub(super) struct ExpectedCodePaint {
    body: Rc<Weak<PieceBackedMarkdownTextView>>,
    identity: WeakMarkdownDocument,
    owner: usize,
    generation: u64,
    target: std::ops::Range<usize>,
    after: u64,
    facts: MemoryWorkloadFacts,
    epoch: MemoryActionEpoch,
    bound: RefCell<Option<NativeMarkdownPreviewBlockProbeSnapshot>>,
    binding_started: Cell<bool>,
    failure: RefCell<Option<String>>,
}

impl ExpectedCodePaint {
    /// 在原动作首 Native 之前固定 Weak 和序号；前次背景不能冒充本次 Find 的实际 paint。
    pub(super) fn capture(
        view: &Retained<PieceBackedMarkdownTextView>,
        facts: &MemoryWorkloadFacts,
        epoch: MemoryActionEpoch,
        target: &std::ops::Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let after = code_paint_sequence()?;
        let end = target.start.checked_add(1).ok_or("代码背景目标范围溢出")?;
        if end > target.end {
            return Err("代码背景目标缺少实际首字符".into());
        }
        let body = native_markdown_table_intent_step(current, || Weak::from_retained(view))
            .map_err(|_| "代码背景原正文弱身份构造时已撤权")?;
        Ok(Self {
            body: Rc::new(body),
            identity: facts.document.clone(),
            owner: std::ptr::from_ref(&**view).addr(),
            generation: facts.generation,
            target: target.start..end, // 原固定中文查询的首标量；不要求整个匹配一次出现在视口内。
            after,
            facts: facts.clone(),
            epoch,
            bound: RefCell::new(None),
            binding_started: Cell::new(false),
            failure: RefCell::new(None),
        })
    }

    /// 原Find终点之后只捕获一次背景准备，随后仅同一实际paint能通知原验证。
    pub(super) fn after_find_terminal(
        self: Rc<Self>,
        request: Option<Rc<NativeMarkdownFindRevealRequest>>,
        observer: Box<dyn FnOnce()>,
    ) {
        if self.binding_started.replace(true) {
            self.reject("原代码背景终点不能重复绑定".to_owned());
            observer();
            return;
        }
        let Some(request) = request else {
            self.reject("原Find终点票已释放，不能改用后继请求".to_owned());
            observer();
            return;
        };
        let completed = request.probe_completed();
        if completed != Some(true) {
            println!(
                "native_code_paint_endpoint find_terminal={completed:?} background_binding=false"
            );
            self.reject("原Find没有真实成功终点，禁止等待背景paint".to_owned());
            observer();
            return;
        }
        let snapshot = with_active_native_markdown_surface(|surface| {
            let current = || {
                memory_workload_facts_current(surface, &self.facts)
                    && self.epoch.permits(surface)
                    && request.probe_completed() == Some(true)
            };
            self.snapshot(surface, &current)
        }); // Native图与局部getter强claim均先归还，再安装纯弱等待或通知Host。
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.reject(error);
                observer();
                return;
            }
        };
        *self.bound.borrow_mut() = Some(snapshot.clone()); // 仅这一次原终点可绑定，后继paint不能换票。
        let after = self.after;
        observe_code_paint(snapshot, after, move |successful| {
            if !successful {
                self.reject("原代码背景准备或视口已撤权，或实际绘制收据无效".to_owned());
            }
            observer(); // 实际paint返回/明确取消只提供一次原验证事件权限。
        });
    }

    fn reject(&self, error: String) {
        let mut failure = self.failure.borrow_mut();
        if failure.is_none() {
            *failure = Some(error); // 后来的清理或取消不能覆盖原失败。
        }
    }

    /// 只取得当前同原body的有限准备事实；Native getter前后继续使用原Find权限。
    fn snapshot(
        &self,
        surface: &NativeMarkdownWindowSurface,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownPreviewBlockProbeSnapshot, String> {
        let editor = surface.editor.as_ref().ok_or("代码背景验收缺少原编辑器")?;
        let original = native_markdown_table_intent_step(current, || self.body.load())
            .map_err(|_| "代码背景原正文弱引用读取时已撤权")?
            .ok_or("代码背景的原正文已经释放")?;
        let same_body = std::ptr::eq(&*original, &*editor.backend.text_view)
            && std::ptr::from_ref(&*original).addr() == self.owner;
        drop(original); // 不把 Native 强 claim 留到 cache getter 或下一事件；析构后再核原权限。
        if !current() || !same_body {
            return Err("代码背景当前正文不属于原 Find 动作".into());
        }
        let snapshot = native_markdown_table_intent_step(current, || {
            editor
                .backend
                .text_view
                .preview_block_probe_snapshot(&self.target)
        })
        .map_err(|_| "代码背景 cache 观察时原 Find 权限已失效")?
        .ok_or("原 Find 可见目标没有同批代码背景准备 cache")?;
        if !WeakMarkdownDocument::ptr_eq(&snapshot.identity, &self.identity)
            || snapshot.key.body_owner != self.owner
            || snapshot.key.generation != self.generation
            || snapshot.key.mode != MarkdownPresentationMode::InlinePreview
            || snapshot.kind != NativeMarkdownPreviewBlockKind::Code as u8
            || snapshot.fragment_source.len() <= MAX_PROBE_CODE_REQUEST_UTF16
            || snapshot.semantic_window.is_empty()
            || snapshot.semantic_window.len() > MAX_PROBE_CODE_REQUEST_UTF16
            || snapshot.semantic_source.start > self.target.start
            || self.target.end > snapshot.semantic_source.end
        {
            return Err("代码背景不是原巨型 fragment 的有限可见语义准备".into());
        }
        Ok(snapshot)
    }

    /// cache 只是准备证据；真正通过还需要原动作之后同 key/source/viewport 的实际绘图收据。
    pub(super) fn verify(
        &self,
        surface: &NativeMarkdownWindowSurface,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if let Some(error) = self.failure.borrow().clone() {
            return Err(error);
        }
        let bound = self
            .bound
            .borrow()
            .clone()
            .ok_or("代码背景缺少原Find终点绑定")?;
        let snapshot = self.snapshot(surface, current)?;
        if !code_snapshot_matches(&bound, &snapshot) {
            return Err("代码背景当前key、源或viewport已不属于原等待票".to_owned());
        }
        let paint: CodePaintReceipt = matching_code_paint(&bound, self.after)?
            .ok_or("原 Find 的巨型代码背景尚无同 key、源与视口的真实绘图收据")?;
        if !current() {
            return Err("代码背景实际绘制收据读取后原权限已失效".into());
        }
        println!(
            "native_memory_code_background verified=true fragment_units={} semantic_request_units={} semantic_source_units={} paint_sequence={} paint_width={} paint_height={} viewport_width={} viewport_height={} same_native_owner=true same_source=true actual_paint_returned=true",
            snapshot.fragment_source.len(),
            snapshot.semantic_window.len(),
            snapshot.semantic_source.len(),
            paint.sequence,
            paint.paint.size.width,
            paint.paint.size.height,
            snapshot.viewport.size.width,
            snapshot.viewport.size.height,
        );
        Ok(())
    }
}
