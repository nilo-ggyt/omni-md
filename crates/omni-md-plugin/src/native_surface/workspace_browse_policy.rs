//! 目录交付的纯权限门禁；Native阶段前后使用同一固定任务和输入票据。

/// policy属于整个pager续页链；错误重选不能在后续Next偷偷恢复自动打开。
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownBrowseCompletionPolicy {
    #[default]
    OpenFirstIfEmpty,
    RequireReselection,
}

impl NativeMarkdownBrowseCompletionPolicy {
    /// 无正文不是自动选择的充分条件，失败后必须保留用户确认当前文件的机会。
    pub(super) fn permits_auto_open(self) -> bool {
        matches!(self, Self::OpenFirstIfEmpty)
    }

    /// 每一页都沿用同一提示；空结果也不把未核验文件误称“库为空”。
    pub(super) fn empty_message(self, actually_empty: bool) -> &'static str {
        match self {
            Self::RequireReselection => "请从目录重新选择笔记，并核对当前文件状态",
            Self::OpenFirstIfEmpty if actually_empty => "这里还没有 Markdown 笔记",
            Self::OpenFirstIfEmpty => "请选择笔记，或继续查看更多笔记",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownBrowseTicket {
    pub(super) generation: u64,
    pub(super) intent: u64,
}

impl NativeMarkdownBrowseTicket {
    /// 搜索模式、输入变化、关闭或另一任务任一成立，旧目录只能丢弃自己的载荷。
    pub(super) fn accepts(
        self,
        generation: u64,
        intent: u64,
        browse_intent: Option<u64>,
        searching: bool,
        closing: bool,
    ) -> bool {
        self.generation == generation
            && self.intent == intent
            && browse_intent == Some(self.intent)
            && !searching
            && !closing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancelable_mailbox::CancelableMailbox;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// 代次各自相同仍不足以通过其他维度；也覆盖getter/setter重入后的B意图。
    #[test]
    fn old_directory_ticket_cannot_cross_search_refresh_input_or_close_boundaries() {
        let ticket = NativeMarkdownBrowseTicket {
            generation: 7,
            intent: 11,
        };
        assert!(ticket.accepts(7, 11, Some(11), false, false));
        for state in [
            (7, 11, Some(11), true, false),
            (8, 11, Some(11), false, false),
            (7, 12, Some(11), false, false),
            (7, 12, Some(12), false, false),
            (7, 11, None, false, false),
            (7, 11, Some(11), false, true),
        ] {
            // 模式或意图变化不能用新的scalar重新贴到旧payload上继续安装。
            assert!(!ticket.accepts(state.0, state.1, state.2, state.3, state.4));
        }
    }

    struct Payload(Arc<AtomicUsize>);
    impl Drop for Payload {
        fn drop(&mut self) {
            // 纯计数证明取消在通知执行前已经销毁旧载荷，不涉及Native对象。
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 正在显示的B邮箱不会被旧A结果/错误取消；close同步排空且迟到worker不能再publish。
    #[test]
    fn directory_to_search_cancellation_releases_old_payload_without_touching_new_mailbox() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let old = CancelableMailbox::new();
        let current = CancelableMailbox::new();
        assert!(old.publish(Payload(Arc::clone(&dropped))));
        assert!(current.publish(42));
        old.cancel();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(!old.publish(Payload(Arc::clone(&dropped))));
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert!(old.take().is_none());
        assert_eq!(current.take(), Some(42));
    }
}

#[cfg(test)]
mod completion_policy_tests {
    use super::NativeMarkdownBrowseCompletionPolicy as Policy;

    #[test]
    /// 错误重选policy跨所有续页保持，空页/有笔记页都不能自动开文档或抹去错误提示。
    fn failure_reselection_is_explicit_on_all_pages_including_empty_result() {
        for actually_empty in [false, true] {
            let policy = Policy::RequireReselection;
            assert!(!policy.permits_auto_open());
            assert_eq!(
                policy.empty_message(actually_empty),
                "请从目录重新选择笔记，并核对当前文件状态"
            );
        }
        assert!(Policy::OpenFirstIfEmpty.permits_auto_open());
        assert_eq!(
            Policy::OpenFirstIfEmpty.empty_message(true),
            "这里还没有 Markdown 笔记"
        );
    }
}
