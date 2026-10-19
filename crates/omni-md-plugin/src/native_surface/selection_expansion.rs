//! 标准全选、选词、选行和选段只计算明确目标；原动作的发布、安装与 IME 转发由共享入口负责。

use super::*;
use objc2_app_kit::NSTextSelectionGranularity;

/// 这些值对应 AppKit 已有的标准选择动作，不维护第二份键盘映射或隐式命令别名。
#[derive(Clone, Copy)]
pub(super) enum NativeMarkdownSelectionExpansion {
    All,
    Word,
    Line,
    Paragraph,
}

impl NativeMarkdownSelectionExpansion {
    /// 全选的两个端点由文档边界明确给定；局部展开必须知道原活动端及其换行侧。
    pub(super) fn requires_known_head(self) -> bool {
        !matches!(self, Self::All)
    }
}

/// 返回值只有有限坐标；调用方结束原 Graph 的临时 claims 后，还须沿原票核准一次发布。
pub(super) fn selection_expansion_target(
    expansion: NativeMarkdownSelectionExpansion,
    graph: Option<&NativeMarkdownSelectionGraph>,
    captured: Option<&NativeMarkdownCapturedSelection>,
    length: usize,
    current: &impl Fn() -> bool,
) -> Result<NativeMarkdownSelectionTarget, String> {
    if !current() {
        return Err("选择命令对应的笔记、窗口或输入已经改变".into()); // 过期命令不能计算后继代次的目标。
    }
    if length > isize::MAX as usize {
        return Err("当前笔记超出原生选择坐标范围".into()); // 明确边界也必须能由同一 Native 图表达。
    }

    let granularity = match expansion {
        NativeMarkdownSelectionExpansion::All => {
            // 这是命令明确指定的完整新方向，允许替代未知或多范围旧选择；不从旧选择猜主 head。
            return Ok(NativeMarkdownSelectionTarget {
                selection: MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: length,
                },
                head_upstream: false,
                line_offset: 0.0, // 此值不建立目标列；共享发布入口必须清除旧 vertical goal。
            });
        }
        NativeMarkdownSelectionExpansion::Word => {
            // 分词规则由当前 TextKit 图决定，包含中文和 Unicode；不扫描正文来猜词边界。
            Some(NSTextSelectionGranularity::Word)
        }
        NativeMarkdownSelectionExpansion::Line => {
            // 行动作在下方独立读取实际视觉行，不通过 enclosing 粒度接口近似。
            None
        }
        NativeMarkdownSelectionExpansion::Paragraph => {
            // 段落边界同样交给当前图，不复制或缓存整段正文。
            Some(NSTextSelectionGranularity::Paragraph)
        }
    };

    let captured = captured.ok_or("当前选择的活动端尚未确定")?;
    if !captured.proven_head || !captured.current() {
        return Err("当前选择的完整方向已经失效".into()); // 数值范围仍相同也不能恢复已撤销的方向事实。
    }
    let head_upstream = captured
        .head_is_upstream()
        .ok_or("当前活动端的换行侧尚未确定")?;
    let graph = graph.ok_or("当前选择没有可用的原文本布局")?;
    let target = if let Some(granularity) = granularity {
        // 全选已经返回；选词和选段必须扩展原完整范围，正向与反向都保留两个源端点。
        graph.enclosing_selection(
            captured.selection,
            head_upstream,
            granularity,
            length,
            current,
        )?
    } else {
        // 新行选择明确采用有序低端的下游实际行，不使用失真的系统内部锚点或段落近似。
        graph.visual_line_at(
            captured
                .selection
                .anchor_utf16
                .min(captured.selection.head_utf16),
            length,
            current,
        )?
    };
    // Graph 已结束本次粒度展开的临时结果；析构或 Native 回调撤权后，不得消费旧坐标。
    if !current() || !captured.current() {
        return Err("选择展开期间原输入权限已经失效".into());
    }
    Ok(target)
}
