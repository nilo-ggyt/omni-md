//! Pure directory-delivery admission preserves the original task and input ticket across native phases.

/// One policy spans the whole pager chain; later Next actions cannot restore auto-open after an error requires reselection.
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownBrowseCompletionPolicy {
    #[default]
    OpenFirstIfEmpty,
    RequireReselection,
}

impl NativeMarkdownBrowseCompletionPolicy {
    /// An empty editor alone does not permit auto-selection; failure must leave the user able to confirm the current file.
    pub(super) fn permits_auto_open(self) -> bool {
        matches!(self, Self::OpenFirstIfEmpty)
    }

    /// Preserve the same status on every page; an empty result cannot label unverified files as an empty vault.
    pub(super) fn empty_message(self, actually_empty: bool) -> &'static str {
        match self {
            // Reselection owns its error context regardless of whether this particular page has rows.
            Self::RequireReselection => "请从目录重新选择笔记，并核对当前文件状态",
            Self::OpenFirstIfEmpty if actually_empty => "这里还没有 Markdown 笔记",
            Self::OpenFirstIfEmpty => "请选择笔记，或继续查看更多笔记",
        }
    }
}

/// Copied generation and intent identify the original browse request without retaining its page payload or native owner.
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownBrowseTicket {
    pub(super) generation: u64,
    pub(super) intent: u64,
}

impl NativeMarkdownBrowseTicket {
    /// Search mode, changed input, close, or a replacement task revokes the old page's publication authority.
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

    /// Matching one generation is insufficient; getter/setter reentry that creates a new intent must also revoke old delivery.
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
            // A changed mode or intent cannot be relabeled onto an old payload to continue installation.
            assert!(!ticket.accepts(state.0, state.1, state.2, state.3, state.4));
        }
    }
}
