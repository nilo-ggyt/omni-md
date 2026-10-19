//! 绘图与几何只消费现成同源scene；延后编辑只借原输入票和唯一producer，不创建另一正文owner。

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGContext;

mod paint;
use objc2_foundation::{NSPoint, NSRange, NSRect};
use omni_markdown_core::{
    MarkdownDocumentBuffer, MarkdownLayoutTextAuthority, MarkdownTextSelection,
};
pub use paint::*;

use super::{NativeMarkdownPagedInputCommit, NativeMarkdownPagedInputState, Result};
use crate::native_surface::paged_input::{NativeMarkdownBidiCaretSide, NativeMarkdownVerticalGoal};

#[derive(Clone)]
/// 每次真实Ready发布使用独立nonce；相同Doc/g/宽度的后继scene不能冒充旧回调。
pub struct NativeMarkdownPagedSceneAuthority(Weak<()>);

/// 唯一scene发布owner持有；弱票不延长scene或原Source生命周期。
pub struct NativeMarkdownPagedSceneNonce(Rc<()>);

impl Default for NativeMarkdownPagedSceneNonce {
    fn default() -> Self {
        Self::new()
    }
}
impl NativeMarkdownPagedSceneNonce {
    #[must_use]
    pub fn new() -> Self {
        Self(Rc::new(()))
    }
    #[must_use]
    pub fn authority(&self) -> NativeMarkdownPagedSceneAuthority {
        NativeMarkdownPagedSceneAuthority(Rc::downgrade(&self.0))
    }
}
impl NativeMarkdownPagedSceneAuthority {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0) && self.0.strong_count() != 0
    }
}

#[derive(Clone)]
/// 全部是原身份与标量；请求不能保留NSEvent、键盘字符串或原生视图。
pub struct NativeMarkdownPagedSceneRequest {
    pub document: Weak<RefCell<MarkdownDocumentBuffer>>,
    pub target: crate::native_surface::paged_input::NativeMarkdownPagedTargetDomain,
    pub input: NativeMarkdownPagedInputState,
    pub viewport: NSRect,
    pub bounds_width: f64,
    pub frame_in_parent: NSRect,
    pub geometry_epoch: u64,
    /// 来自原virtual-scroll owner，不能用固定局部visibleRect的0代替正文滚动来源。
    pub virtual_viewport: NativeMarkdownPagedVirtualViewport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownPagedVirtualViewport {
    pub document_origin: NSPoint,
    pub scroll_epoch: u64,
}

impl NativeMarkdownPagedSceneRequest {
    /// 原Doc allocation和所有原始几何均相同才可消费；同文本/同generation不替代身份。
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.document, &other.document)
            && self.target.same(&other.target)
            && self.input == other.input
            && self.viewport == other.viewport
            && self.bounds_width == other.bounds_width
            && self.frame_in_parent == other.frame_in_parent
            && self.geometry_epoch == other.geometry_epoch
            && self.virtual_viewport == other.virtual_viewport
    }
}

#[derive(Clone)]
pub struct NativeMarkdownPagedSceneKey {
    pub request: NativeMarkdownPagedSceneRequest,
    pub source: MarkdownLayoutTextAuthority,
    pub nonce: NativeMarkdownPagedSceneAuthority,
}
impl NativeMarkdownPagedSceneKey {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.request.same(&other.request)
            && self.source == other.source
            && self.nonce.same(&other.nonce)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 来自系统已经解释的语义选择动作；没有新增物理按键或Host注册。
pub enum NativeMarkdownPagedNavigation {
    Backward,
    Forward,
    Left,
    Right,
    Up,
    Down,
    WordBackward,
    WordForward,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    LineLeft,
    LineRight,
    ParagraphStart,
    ParagraphEnd,
    DocumentStart,
    DocumentEnd,
    PageUp,
    PageDown,
    SelectWord,
    SelectLine,
    SelectParagraph,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 扩选只保原指针的逻辑锚点；词组拖动保首击真实GCB及点击侧，不保事件或正文。
pub enum NativeMarkdownPagedWordAnchor {
    Selection(usize),
    Word {
        head_utf16: usize,
        backward_bias: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 这是一次原生选词请求的真实来源坐标；原Doc/Input/几何仍由同SceneRequest与permit约束。
pub struct NativeMarkdownPagedWordSelection {
    pub head_utf16: usize,
    pub backward_bias: bool,
    pub anchor: Option<NativeMarkdownPagedWordAnchor>,
}

impl NativeMarkdownPagedNavigation {
    /// 与旧原生路径相同，只有连续逐行Up/Down保留第一条真实caret目标列。
    #[must_use]
    pub fn preserves_vertical_goal(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// 上/下游只表示原soft-wrap换行侧；Bidi所属part由独立的level/逻辑字素侧表达。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedHeadAffinity {
    Upstream,
    Downstream,
}
impl NativeMarkdownPagedHeadAffinity {
    #[must_use]
    pub fn from_upstream(upstream: bool) -> Self {
        if upstream {
            Self::Upstream
        } else {
            Self::Downstream
        }
    }
    #[must_use]
    pub fn is_upstream(self) -> bool {
        self == Self::Upstream
    }
}

#[derive(Clone)]
/// 原producer已准备的排版坐标系；layout_epoch随Source、字体/样式和布局改变撤销，
/// 不能使用每次clip滚动就改变的View geometry_epoch或虚拟滚动scroll_epoch。
pub struct NativeMarkdownPagedNavigationGeometry {
    pub source: MarkdownLayoutTextAuthority,
    /// 原组合器实际可用行宽的键，不能用窗口bounds宽猜文本容器宽。
    pub width_key: i64,
    pub layout_epoch: u64,
}
impl NativeMarkdownPagedNavigationGeometry {
    /// 比较同producer的有限坐标身份；不能领取新Source或新布局为旧导航补签。
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.source == other.source
            && self.width_key == other.width_key
            && self.layout_epoch == other.layout_epoch
    }
    /// Source必须属于原SceneKey，正行宽也只来自实际布局；活性仍由原scene guard核准。
    #[must_use]
    pub fn permits(&self, key: &NativeMarkdownPagedSceneKey) -> bool {
        self.source == key.source && self.width_key > 0
    }
    /// 容器宽度和原排版代次同时相同才可继承目标列；滚动事件代次没有此语义。
    #[must_use]
    pub fn permits_goal(&self, goal: NativeMarkdownVerticalGoal) -> bool {
        goal.width_key() == self.width_key && goal.layout_epoch() == self.layout_epoch
    }
}

/// 仅原Input唯一选择槽的事实；没有独立缓存、历史或Native对象。
pub struct NativeMarkdownPagedNavigationContext {
    pub geometry: NativeMarkdownPagedNavigationGeometry,
    pub head_affinity: Option<NativeMarkdownPagedHeadAffinity>,
    pub vertical_goal: Option<NativeMarkdownVerticalGoal>,
    pub bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

/// 原scene在同一个有向选择上给出的完整目标；短行新head列不可覆盖原vertical_goal。
pub struct NativeMarkdownPagedNavigationTarget {
    pub selection: MarkdownTextSelection,
    pub head_affinity: Option<NativeMarkdownPagedHeadAffinity>,
    pub vertical_goal: Option<NativeMarkdownVerticalGoal>,
    pub bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedEditCommand {
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    DeleteToLineStart,
    DeleteToLineEnd,
    InsertNewline,
    InsertTab,
    InsertBacktab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 原field已经解释的客户动作；没有新增按键，后继controller必须消费原许可并核原客户边界。
pub enum NativeMarkdownPagedCellAction {
    Next,
    Previous,
    NextRow,
    Cancel,
}

pub struct NativeMarkdownPagedEditPlan {
    pub replacement: NSRange,
    /// 仅本次新增文字；不得把原全文或离屏段落复制进plan。
    pub inserted: String,
}

pub struct NativeMarkdownPagedCharacterRect {
    /// 属于原正文view的本地坐标；screen转换只能由持同票的实际view/window完成。
    pub rect: NSRect,
    pub actual_range: NSRange,
}

/// 实现者只返回已Ready的有限行/命中/绘图结果，不在任何方法中同步shape或全段布局。
/// paint必须用同原Source/Font/输出nonce的sealed spool painter绘真实glyph，并保持原背景、
/// 选择和装饰合同；成功不是“有缓存”，也不允许空image冒充非空正文。
pub trait NativeMarkdownPagedTextScene {
    fn key(&self) -> &NativeMarkdownPagedSceneKey;
    /// 原完整CPU帧已提交且真实计数增长后的单次通知；不另授Source或Native可见权限。
    /// 实现者只排持弱身份的后继通知，归还当前绘图栈后仍须核原key和实际发布计数。
    fn cpu_frame_published(&self, _paints: usize) {
        // 没有观察者的scene无需保存额外状态；默认实现不排队、不读取正文或重入Native。
    }
    /// 必须核原Source、Font和输出真实健康；空glyph读取仍经过原Reader权限门，不能默认成功。
    /// 所有current和Native调用都在借用外，Source首错优先；本方法不排版、不读取正文或创建图像。
    fn validate_ready(&self, current: &dyn Fn() -> bool) -> Result<()>;
    fn first_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect>;
    fn hit(&self, local: NSPoint) -> Result<usize>;
    /// 双击必须消费原字形命中及真实点击侧；单个source整数不足以判断词边界的哪一组。
    fn word_hit(&self, _local: NSPoint) -> Result<(usize, bool)> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<MarkdownTextSelection>;
    /// 尚无实际排版坐标producer时保持Pending；Ready绘图本身不授权导航。
    fn navigation_geometry(&self) -> Result<NativeMarkdownPagedNavigationGeometry> {
        Err(super::NativeMarkdownPagedTextViewError::Pending)
    }
    fn navigate(
        &self,
        action: NativeMarkdownPagedNavigation,
        extend: bool,
        context: &NativeMarkdownPagedNavigationContext,
    ) -> Result<NativeMarkdownPagedNavigationTarget>;
    fn edit_plan(
        &self,
        action: NativeMarkdownPagedEditCommand,
    ) -> Result<NativeMarkdownPagedEditPlan>;
    /// 创建原CPU候选的消费式绘制；没有实际分批producer的实现必须明确Pending。
    fn begin_cpu_paint(
        self: Rc<Self>,
        context: CFRetained<CGContext>,
        current: &dyn Fn() -> bool,
    ) -> Result<Box<dyn NativeMarkdownPagedScenePaintSession>>;
    fn paint(&self, context: &CGContext, current: &dyn Fn() -> bool) -> Result<()>;
}

/// composition root实现弱委托。current可重入，所以View调用前后都结束借用并复核原状态。
/// ready_scene只查已发布scene；尚未Ready必须Pending，不保存或重放本次Native事件。
pub trait NativeMarkdownPagedTextViewDelegate {
    fn current(&self, epoch: u64) -> bool;
    fn virtual_viewport(&self, epoch: u64) -> Result<NativeMarkdownPagedVirtualViewport>;
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>>;
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool;
    /// 这里只通知已有coalesced调度入口；不允许在输入回调内整段排版或静默替换canonical。
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// 文档头尾已提交后，只接收这张原收据的显示意图；实际Source绑定和滚动由唯一正文
    /// producer继续。接收成功不是Ready，也不能让调用方重放已经发生的选择提交。
    fn reveal_committed_selection(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// 正文owner可接纳一次真实垂直目标并延后排版；true仅表示已接纳，不表示选择已移动。
    /// 其他客户继续原Scene同步导航，不创建备用正文或把未准备结果当Ready。
    fn navigate_vertical(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _action: NativeMarkdownPagedNavigation,
        _extend: bool,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 原正文/Cell owner可接纳一个空caret的逻辑词组删除；Ready范围仍必须消费同张输入票。
    /// 没有原词组producer的客户明确拒绝，不把它近似为一个scalar或临时构造第二份Source。
    fn delete_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _forward: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// A Body may enqueue one visual intent; false retains direct navigation for an already prepared finite Scene.
    fn move_visual(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _right: bool,
        _extend: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 原全源UniformZero准备完成后才执行标准Word移动；Pending不等于已改变选择。
    fn move_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _forward: bool,
        _extend: bool,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// 原Body/Cell逐量子完成逻辑选词；接纳不是选择已提交，也不授予混合Bidi视觉移动。
    fn select_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _selection: NativeMarkdownPagedWordSelection,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Body已有词组删除在途时，可接纳随后普通输入；载荷只同步借用，false保原直接输入。
    /// 接纳者必须复制本次有限新增载荷且保序，不能保留Native对象或凭旧permit稍后重放。
    fn defer_insert(
        &self,
        _text: &objc2::runtime::AnyObject,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // 没有延后输入owner的Scene不创建备用队列或第二份Input。
    }
    /// 裸Page是纯滚动；Source owner等待真实下一页高度再调用系统动作，不提交文本选择。
    fn scroll_page(&self, _request: &NativeMarkdownPagedSceneRequest, _down: bool) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// Cell候选尚未接原RequiredRow/BorrowedEditor控制器时必须Pending，不退回插入Tab/换行或发布正文。
    fn cell_action(
        &self,
        _action: NativeMarkdownPagedCellAction,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::Pending)
    }
    /// 仅显式Copy/Cut使用既有系统剪贴板生产者；成功后才允许View用原permit执行Cut。
    /// 生产者必须保持原完整选择和Source，禁止把按性能截短的内容伪装成完整复制。
    fn copy_selection(&self, request: &NativeMarkdownPagedSceneRequest) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_markdown_core::MarkdownLayoutTextSource;
    use std::io::Write;

    #[test]
    fn navigation_coordinates_keep_original_source_width_and_layout_identity() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        let document = MarkdownDocumentBuffer::open(file.path(), 4096).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let other = MarkdownLayoutTextSource::capture(&document);
        let geometry = NativeMarkdownPagedNavigationGeometry {
            source: source.authority(),
            width_key: 576 * 64,
            layout_epoch: 11,
        };
        // 这里只核纯契约，不以合成列值冒充Native布局或方向行为已经通过。
        let goal =
            NativeMarkdownVerticalGoal::new(38.5, 74.25, geometry.width_key, geometry.layout_epoch)
                .unwrap();
        assert!(geometry.permits_goal(goal));
        assert!(geometry.same(&geometry.clone()));
        let wrong_width = NativeMarkdownVerticalGoal::new(38.5, 74.25, 640 * 64, 11).unwrap();
        let wrong_epoch = NativeMarkdownVerticalGoal::new(38.5, 74.25, 576 * 64, 12).unwrap();
        assert!(!geometry.permits_goal(wrong_width) && !geometry.permits_goal(wrong_epoch));
        assert!(!geometry.same(&NativeMarkdownPagedNavigationGeometry {
            source: other.authority(),
            ..geometry.clone()
        }));
        assert!(NativeMarkdownVerticalGoal::new(f64::NAN, 74.25, 576 * 64, 11).is_none());
        assert!(NativeMarkdownVerticalGoal::new(38.5, f64::INFINITY, 576 * 64, 11).is_none());
        assert!(NativeMarkdownPagedNavigation::Up.preserves_vertical_goal());
        assert!(NativeMarkdownPagedNavigation::Down.preserves_vertical_goal());
        assert!(!NativeMarkdownPagedNavigation::PageDown.preserves_vertical_goal());
        assert!(!NativeMarkdownPagedNavigation::WordLeft.preserves_vertical_goal());
    }
}
