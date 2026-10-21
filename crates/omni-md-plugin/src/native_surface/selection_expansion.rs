//! Compute explicit standard selection targets; the shared action entry owns publication, installation and IME forwarding.

use super::*;
use objc2_app_kit::NSTextSelectionGranularity;

/// Represent existing AppKit selection actions without another keyboard map or implicit command alias.
#[derive(Clone, Copy)]
pub(super) enum NativeMarkdownSelectionExpansion {
    All,
    Word,
    Line,
    Paragraph,
}

impl NativeMarkdownSelectionExpansion {
    /// Select-all supplies both document endpoints; local expansion needs the original active endpoint and line affinity.
    pub(super) fn requires_known_head(self) -> bool {
        !matches!(self, Self::All)
    }
}

/// Return finite coordinates only; after releasing temporary Graph claims, the caller still validates the original publication ticket.
pub(super) fn selection_expansion_target(
    expansion: NativeMarkdownSelectionExpansion,
    graph: Option<&NativeMarkdownSelectionGraph>,
    captured: Option<&NativeMarkdownCapturedSelection>,
    length: usize,
    current: &impl Fn() -> bool,
) -> Result<NativeMarkdownSelectionTarget, String> {
    if !current() {
        // An expired command cannot calculate a target for a successor document, window or Input.
        return Err("选择命令对应的笔记、窗口或输入已经改变".into());
    }
    if length > isize::MAX as usize {
        // Even explicitly supplied endpoints must fit the same native graph's signed coordinate domain.
        return Err("当前笔记超出原生选择坐标范围".into());
    }

    let granularity = match expansion {
        NativeMarkdownSelectionExpansion::All => {
            // This action explicitly specifies the complete new direction. It may replace an unknown
            // or multi-range selection without guessing a primary active endpoint from old state.
            return Ok(NativeMarkdownSelectionTarget {
                selection: MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: length,
                },
                head_upstream: false,
                // This value establishes no target column; shared publication clears the old vertical goal.
                line_offset: 0.0,
            });
        }
        NativeMarkdownSelectionExpansion::Word => {
            // The current TextKit graph supplies Unicode/Chinese word boundaries without a second body scan.
            Some(NSTextSelectionGranularity::Word)
        }
        NativeMarkdownSelectionExpansion::Line => {
            // Read the actual visual line below instead of approximating it through enclosing granularity.
            None
        }
        NativeMarkdownSelectionExpansion::Paragraph => {
            // The same current graph supplies paragraph boundaries without copying or caching paragraph text.
            Some(NSTextSelectionGranularity::Paragraph)
        }
    };

    let captured = captured.ok_or("当前选择的活动端尚未确定")?;
    if !captured.proven_head || !captured.current() {
        // Numerically equal endpoints cannot restore direction authority that has already been revoked.
        return Err("当前选择的完整方向已经失效".into());
    }
    let head_upstream = captured
        .head_is_upstream()
        .ok_or("当前活动端的换行侧尚未确定")?;
    let graph = graph.ok_or("当前选择没有可用的原文本布局")?;
    let target = if let Some(granularity) = granularity {
        // Select-all already returned. Word/paragraph expansion starts from the complete original
        // range, preserving both source endpoints for either forward or reversed selection.
        graph.enclosing_selection(
            captured.selection,
            head_upstream,
            granularity,
            length,
            current,
        )?
    } else {
        // Line selection explicitly uses the actual downstream line at the ordered lower endpoint,
        // avoiding an unreliable internal system anchor or a paragraph-shaped approximation.
        graph.visual_line_at(
            captured
                .selection
                .anchor_utf16
                .min(captured.selection.head_utf16),
            length,
            current,
        )?
    };
    // Temporary Graph results have ended. Destruction or native callbacks may revoke authority,
    // so stale coordinates cannot leave this boundary merely because expansion itself succeeded.
    if !current() || !captured.current() {
        return Err("选择展开期间原输入权限已经失效".into());
    }
    Ok(target)
}
