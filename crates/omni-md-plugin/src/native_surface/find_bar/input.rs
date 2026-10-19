//! 原字段协议栈与通知共同观察组合输入；只保弱身份和标量，不接管 AppKit 输入或历史。

use super::*;

/// 五种真实入口各最多 128 条；仅诊断已取得的标量，不为日志读取候选或添加 Native 操作。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_find_input(selector: &'static str, stage: &'static str, details: std::fmt::Arguments<'_>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTS: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
    let index = match selector {
        "marked" => 0,
        "unmark" => 1,
        "insert" => 2,
        "observe" => 3,
        _ => 4,
    };
    if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
        && COUNTS[index].fetch_add(1, Ordering::Relaxed) < 128
    {
        eprintln!("native_find_input selector={selector} stage={stage} {details} formal=false");
    }
}

/// 仅在一次真实公开输入协议栈内存在；票不拥有栏、字段、controller 或正文。
struct NativeMarkdownFindInputTicket {
    feedback: NativeMarkdownFindViewportFeedback,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    controller: Weak<NativeMarkdownController>,
    field: Weak<NSTextField>,
    field_address: usize,
    editor_address: usize,
    client_revision: u64,
    notified_change: bool,
    ended: bool,
    _selector: &'static str,
}

impl NativeMarkdownFindInputTicket {
    /// 输入前固定真实窗口 delegate 和具体客户端，getter 后始终核同一原票。
    fn capture(editor: &NSTextView, _selector: &'static str) -> Option<Self> {
        let session = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()?;
        let client_revision = session.input_client_revision();
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_begin",
            format_args!(
                "revision={client_revision} editor={}",
                std::ptr::from_ref(editor).addr()
            ),
        );
        let Some(window) = editor.window() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_window_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_window",
            format_args!(
                "window={} revision={}",
                std::ptr::from_ref(&*window).addr(),
                session.input_client_revision()
            ),
        );
        if session.input_client_revision() != client_revision {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_window_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None; // window getter 已重入重新交付，不能从后继客户端开始领取原输入票。
        }
        let Some(delegate) = window.delegate() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_delegate_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        if session.input_client_revision() != client_revision {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_delegate_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None; // delegate getter 同样可能换客户；此时尚无权限读取任何具体栏字段。
        }
        let object: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_delegate",
            format_args!(
                "delegate={} revision={}",
                std::ptr::from_ref(object).addr(),
                session.input_client_revision()
            ),
        );
        let Some(controller) = object.downcast_ref::<NativeMarkdownController>() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_foreign_delegate",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        if !session.belongs_to(&controller.ivars().undo, &controller.ivars().owned) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_foreign_session",
                format_args!("revision={client_revision}"),
            );
            return None; // 旧会话 editor 即使暂留在同窗口，也不能借新 controller 的输入能力。
        }
        let Some(bar) = controller.current_find_input_bar() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_bar_none",
                format_args!("revision={client_revision}"),
            );
            return None;
        };
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            controller,
            controller.find_input_intent(),
            false,
        ) else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_feedback_none",
                format_args!(
                    "revision={client_revision} intent={}",
                    controller.find_input_intent()
                ),
            );
            return None;
        };
        let current = || {
            feedback.current(controller)
                && session.input_client_revision() == client_revision
                && session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        };
        let Ok(weak_controller) =
            native_markdown_table_intent_step(&current, || Weak::from(controller))
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_weak_controller_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None;
        };
        let Ok(fields) = native_markdown_table_intent_step(&current, || bar.fields()) else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_fields_revoked",
                format_args!(
                    "expected={client_revision} actual={}",
                    session.input_client_revision()
                ),
            );
            return None;
        };
        let editor_address = std::ptr::from_ref(editor).addr();
        let mut original = None;
        for field in fields {
            // 诊断直接标识实际字段；普通构建不保留仅供日志使用的遍历编号。
            #[cfg(feature = "native-markdown-runtime-probe")]
            let field_address = std::ptr::from_ref(&*field).addr();
            let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
            else {
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_find_input(
                    _selector,
                    "capture_field_revoked",
                    format_args!(
                        "field_address={field_address} expected={client_revision} actual={} feedback={}",
                        session.input_client_revision(),
                        feedback.current(controller)
                    ),
                );
                return None;
            };
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_field",
                format_args!(
                    "field_address={field_address} editor={:?} original_editor={editor_address} revision={}",
                    actual
                        .as_ref()
                        .map(|value| std::ptr::from_ref(&**value).addr()),
                    session.input_client_revision()
                ),
            );
            if actual
                .as_ref()
                .is_some_and(|actual| std::ptr::from_ref(&**actual).addr() == editor_address)
            {
                if original.is_some() {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        _selector,
                        "capture_duplicate",
                        format_args!(
                            "field_address={field_address} editor={editor_address} revision={client_revision}"
                        ),
                    );
                    return None; // 两字段同时声明同一客户端没有唯一来源，不按遍历顺序任选一个。
                }
                let Ok(weak) =
                    native_markdown_table_intent_step(&current, || Weak::from_retained(&field))
                else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        _selector,
                        "capture_weak_field_revoked",
                        format_args!(
                            "field_address={field_address} expected={client_revision} actual={}",
                            session.input_client_revision()
                        ),
                    );
                    return None;
                };
                original = Some((weak, std::ptr::from_ref(&*field).addr()));
            }
        }
        let Some((field, field_address)) = original else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                _selector,
                "capture_no_client",
                format_args!("editor={editor_address} revision={client_revision}"),
            );
            return None;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "capture_end",
            format_args!(
                "field={field_address} expected={client_revision} actual={} current={}",
                session.input_client_revision(),
                current()
            ),
        );
        current().then_some(Self {
            feedback,
            bar: Rc::downgrade(&bar),
            controller: weak_controller,
            field,
            field_address,
            editor_address,
            client_revision,
            notified_change: false,
            ended: false,
            _selector,
        })
    }

    /// 通知只向原具体 field 的活协议栈归并；此检查无 Native 调用，可在短借用内完成。
    fn accepts_notification(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        client_revision: u64,
    ) -> bool {
        self.field_address == std::ptr::from_ref(field).addr()
            && self.client_revision == client_revision
            && self.feedback.current(controller)
    }

    /// 最外层 super 已结束才读取最终组合状态；绝不从 after 的当前客户重新捕获身份。
    fn finish(self, original_editor: &NSTextView) {
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "finish_begin",
            format_args!(
                "field={} editor={} expected_revision={} notified={} ended={}",
                self.field_address,
                self.editor_address,
                self.client_revision,
                self.notified_change,
                self.ended
            ),
        );
        let Some(session) = original_editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()
        else {
            return;
        };
        if std::ptr::from_ref(original_editor).addr() != self.editor_address {
            return; // 票只能交回仍在原实际协议或事件栈上的 editor 引用，不能由另一个receiver消费。
        }
        let Some(controller) = self.controller.load() else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_controller_none",
                format_args!("expected_revision={}", self.client_revision),
            );
            return; // 原窗口已结束，没有可恢复的反馈目标。
        };
        let current = || {
            self.feedback.current(&controller)
                && session.input_client_revision() == self.client_revision
                && session.belongs_to(&controller.ivars().undo, &controller.ivars().owned)
        };
        let Some(field) = native_markdown_table_intent_step(&current, || self.field.load())
            .ok()
            .flatten()
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_field_none_or_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return; // getter 重入、关闭或原字段销毁都丢弃本票，不观察后继客户端。
        };
        let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_client_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "finish_client",
            format_args!(
                "actual_editor={:?} original_editor={} expected_revision={} actual_revision={} ended={}",
                actual
                    .as_ref()
                    .map(|value| std::ptr::from_ref(&**value).addr()),
                self.editor_address,
                self.client_revision,
                session.input_client_revision(),
                self.ended
            ),
        );
        let marked = match actual {
            Some(actual)
                if !self.ended && std::ptr::from_ref(&*actual).addr() == self.editor_address =>
            {
                let Some(editor) = actual.downcast_ref::<NSTextView>() else {
                    return; // 只有原实际 NSTextView 能提供公开的组合状态。
                };
                let Ok(marked) =
                    native_markdown_table_intent_step(&current, || editor.hasMarkedText())
                else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_find_input(
                        self._selector,
                        "finish_marked_revoked",
                        format_args!(
                            "expected_revision={} actual_revision={} feedback={}",
                            self.client_revision,
                            session.input_client_revision(),
                            self.feedback.current(&controller)
                        ),
                    );
                    return;
                };
                marked
            }
            None if self.ended => false, // 同 field 的真实 didEnd 已证明结束，且没有重入的新客户端。
            _ => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_find_input(
                    self._selector,
                    "finish_client_mismatch",
                    format_args!(
                        "ended={} expected_revision={} actual_revision={}",
                        self.ended,
                        self.client_revision,
                        session.input_client_revision()
                    ),
                );
                return; // 同 editor 被别的字段复用、或旧 field 重新开始，都不能续接原输入。
            }
        };
        let Ok(confirmed) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_confirmation_revoked",
                format_args!(
                    "expected_revision={} actual_revision={} feedback={}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        };
        let same_client = if self.ended {
            confirmed.is_none() // 真实结束还须保持无客户端，不能仅凭早先一次 nil 续接。
        } else {
            confirmed
                .as_ref()
                .is_some_and(|editor| std::ptr::from_ref(&**editor).addr() == self.editor_address)
        };
        drop(confirmed); // 当前 editor 的临时 +1 归还也可能重入；最后仍核原票再写标量。
        if !same_client || !current() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_input(
                self._selector,
                "finish_final_rejected",
                format_args!(
                    "same_client={same_client} expected_revision={} actual_revision={} feedback={} marked={marked}",
                    self.client_revision,
                    session.input_client_revision(),
                    self.feedback.current(&controller)
                ),
            );
            return;
        }
        if current()
            && let Some(bar) = self.bar.upgrade()
        {
            bar.publish_input_state(
                &controller,
                &field,
                marked,
                self.notified_change,
                self._selector,
            );
        }
    }
}

/// 真实嵌套协议只共享一份弱票；结束后没有常驻事件、文本或第二个编辑 owner。
#[derive(Default)]
pub(in crate::native_surface) struct NativeMarkdownFindProtocolState {
    depth: Cell<usize>,
    ticket: RefCell<Option<NativeMarkdownFindInputTicket>>,
}

impl NativeMarkdownFindProtocolState {
    /// 先建立栈深度再调用可重入 getter；嵌套协议不能覆盖最外层原客户端票。
    pub(in crate::native_surface) fn enter<'a>(
        &'a self,
        editor: &'a NSTextView,
        _selector: &'static str,
    ) -> NativeMarkdownFindProtocolScope<'a> {
        let depth = self.depth.get();
        self.depth
            .set(depth.checked_add(1).expect("Find 输入栈深度溢出"));
        let scope = NativeMarkdownFindProtocolScope {
            state: self,
            editor,
            active: true,
            _selector,
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(_selector, "hook_enter", format_args!("depth={depth}"));
        if depth == 0 {
            let ticket = NativeMarkdownFindInputTicket::capture(editor, _selector);
            let previous = self.ticket.replace(ticket);
            drop(previous); // 弱 claim 的析构也在 RefCell 借用结束之后。
        }
        scope
    }

    /// 原字段通知只记录一次发生事实，最终是否提交由最外层真实 marked 状态决定。
    pub(in crate::native_surface) fn defer_notification(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        client_revision: u64,
        changed: bool,
        ended: bool,
    ) -> bool {
        let mut ticket = self.ticket.borrow_mut();
        let Some(ticket) = ticket
            .as_mut()
            .filter(|ticket| ticket.accepts_notification(controller, field, client_revision))
        else {
            return false; // 不属于该原栈的通知继续正常即时路径，不吞掉其他真实输入。
        };
        ticket.notified_change |= changed;
        ticket.ended |= ended;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            ticket._selector,
            "notification_deferred",
            format_args!(
                "changed={changed} ended={ended} accumulated_change={} accumulated_end={} revision={client_revision}",
                ticket.notified_change, ticket.ended
            ),
        );
        true
    }

    /// accepted 清理撤掉弱票，但深度仍由实际尚未退出的协议栈逐层归还。
    pub(in crate::native_surface) fn seal(&self) {
        let previous = self.ticket.take();
        drop(previous);
    }

    /// 只有真正最外层退出才移动票；没有 getter、状态借用或输入观察混在深度归还中。
    fn leave(&self) -> Option<NativeMarkdownFindInputTicket> {
        let depth = self
            .depth
            .get()
            .checked_sub(1)
            .expect("Find 输入栈未成对归还");
        self.depth.set(depth);
        (depth == 0).then(|| self.ticket.take()).flatten()
    }
}

/// 本 guard 只配对公开协议调用栈，Rust unwind 时不运行 Native 反馈或伪装成功。
pub(in crate::native_surface) struct NativeMarkdownFindProtocolScope<'a> {
    state: &'a NativeMarkdownFindProtocolState,
    editor: &'a NSTextView,
    active: bool,
    _selector: &'static str,
}

impl NativeMarkdownFindProtocolScope<'_> {
    /// super 返回后先结束所有栈状态借用，再允许反馈 setter 触发新的输入。
    pub(in crate::native_surface) fn finish(mut self) {
        self.active = false;
        let ticket = self.state.leave();
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            self._selector,
            "hook_return",
            format_args!(
                "remaining_depth={} ticket={}",
                self.state.depth.get(),
                ticket.is_some()
            ),
        );
        if let Some(ticket) = ticket {
            ticket.finish(self.editor);
        }
    }
}

impl Drop for NativeMarkdownFindProtocolScope<'_> {
    fn drop(&mut self) {
        if self.active {
            drop(self.state.leave()); // 异常路径只撤原弱能力，不更新组合标量或发布反馈。
        }
    }
}

impl NativeMarkdownFindBar {
    /// 先记录本字段最终组合状态，再撤旧反馈；setter 重入不能让外层覆盖后继状态。
    fn publish_input_state(
        &self,
        controller: &NativeMarkdownController,
        field: &NSTextField,
        marked: bool,
        changed: bool,
        _selector: &'static str,
    ) {
        let Some(refresh) = self.input_refresh_for_field(field) else {
            return;
        };
        let state = if refresh {
            &self.query_marked
        } else {
            &self.replacement_marked
        };
        let before = state.replace(marked);
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_input(
            _selector,
            "publish_state",
            format_args!(
                "query={refresh} before_marked={before} after_marked={marked} notified={changed} should_refresh={} intent={}",
                before != marked || (changed && !marked),
                controller.find_input_intent()
            ),
        );
        if (before != marked || (changed && !marked))
            && let Some(bar) = controller
                .current_find_input_bar()
                .filter(|bar| std::ptr::eq(&**bar, self))
        {
            // 组合开始/结束各撤一次；true→true 候选变化不读值也不反复刷新。
            controller.enqueue_find_input_feedback(&bar, refresh);
        }
    }
}

impl NativeMarkdownController {
    /// 项目字段通知与协议栈共用实际状态观察；只认原栏的具体字段，不按 tag 归属。
    pub(in crate::native_surface) fn observe_find_control_input(
        &self,
        field: &NSTextField,
        changed: bool,
        ended: bool,
    ) -> bool {
        let Some(bar) = self
            .current_find_input_bar()
            .filter(|bar| bar.input_refresh_for_field(field).is_some())
        else {
            return false;
        };
        if self
            .ivars()
            .field_editor_provider
            .defer_find_input(self, field, changed, ended)
        {
            return true; // 历史和字段结束仍由原通知继续执行，仅 Find 延迟到实际最外层协议返回。
        }
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            self,
            self.find_input_intent(),
            false,
        ) else {
            return true; // 原栏已封闭或正文已换，不能把通知移到后继查询。
        };
        let current = || feedback.current(self);
        let Ok(actual) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            return true;
        };
        let (marked, editor_identity) = match actual {
            Some(actual) => {
                let Some(editor) = actual.downcast_ref::<NSTextView>() else {
                    return true;
                };
                let Some(session) = editor.downcast_ref::<NativeMarkdownSessionFieldEditor>()
                else {
                    return true; // 外来共享 editor 的标量不能写回本会话字段，历史路径仍保留原归属错误。
                };
                if !session.belongs_to(&self.ivars().undo, &self.ivars().owned) {
                    return true; // 类型相同也不能借另一窗口的 Undo/ledger 身份读取当前字段状态。
                }
                let address = std::ptr::from_ref(editor).addr();
                let revision = session.input_client_revision();
                let input_current = || {
                    current()
                        && self
                            .ivars()
                            .field_editor_provider
                            .current_input_client_revision(address)
                            == Some(revision)
                };
                let Ok(marked) =
                    native_markdown_table_intent_step(&input_current, || editor.hasMarkedText())
                else {
                    return true;
                };
                (marked, Some((address, revision)))
            }
            None if ended => (false, None), // 真实 didEnd 的原字段没有新客户端，组合状态可以正常结束。
            None => return true, // begin/change 失去实际 editor 时不猜未 marked，也不读取控件候选值。
        };
        let current = || {
            feedback.current(self)
                && editor_identity.is_none_or(|(address, revision)| {
                    self.ivars()
                        .field_editor_provider
                        .current_input_client_revision(address)
                        == Some(revision)
                })
        };
        let Ok(confirmed) = native_markdown_table_intent_step(&current, || field.currentEditor())
        else {
            return true;
        };
        let same_client = match editor_identity {
            Some((address, _)) => confirmed
                .as_ref()
                .is_some_and(|editor| std::ptr::from_ref(&**editor).addr() == address),
            None => confirmed.is_none(), // 结束后若已出现新客户端，旧通知不得清其组合状态。
        };
        drop(confirmed);
        if !same_client || !current() {
            return true; // 最后 Native claim 的析构也已结束，失权时不更新 marked 或安排旧反馈。
        }
        if current() {
            bar.publish_input_state(self, field, marked, changed, "notification");
        }
        true
    }

    /// 原 Panel 事件后只补状态跃迁；协议 hook 或通知已经完成的输入不会重复推进意图。
    pub(in crate::native_surface) fn observe_find_native_editor(&self, editor: &NSTextView) {
        if let Some(ticket) = NativeMarkdownFindInputTicket::capture(editor, "observe")
            && ticket.feedback.current(self)
        {
            ticket.finish(editor);
        }
    }
}
