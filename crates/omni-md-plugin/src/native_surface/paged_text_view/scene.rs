//! 绘图与几何只消费现成同源scene；延后编辑只借原输入票和唯一producer，不创建另一正文owner。

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use objc2::rc::Retained;
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
    /// 同次Native转换证明的lane零滚动X；与实际可见origin分列，不能把ruler/inset当水平滚动。
    pub layout_origin_x: f64,
    /// 完整clip inset lane的实际宽度；有限Body在文档边缘缩窄不改变原换行宽。
    pub layout_width: f64,
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
    /// 同一空样式正文在 Source 与 Preview 使用不同真实行槽；模式属于本次几何身份，不能从styles猜测。
    pub presentation_mode: omni_markdown_core::MarkdownPresentationMode,
    pub source: MarkdownLayoutTextAuthority,
    pub nonce: NativeMarkdownPagedSceneAuthority,
}
impl NativeMarkdownPagedSceneKey {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.request.same(&other.request)
            && self.presentation_mode == other.presentation_mode
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

/// Word 的逻辑方向与屏幕方向跨异步队列仍保持原 selector 含义；不能提前压成同一个布尔值。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedWordMotion {
    Backward,
    Forward,
    Left,
    Right,
}

impl NativeMarkdownPagedWordMotion {
    /// 只有逻辑动作直接给出 Source 前后方向；视觉动作必须由真实布局或完整零级证明解释。
    pub(crate) fn logical_forward(self) -> Option<bool> {
        match self {
            Self::Backward => Some(false),
            Self::Forward => Some(true),
            Self::Left | Self::Right => None,
        }
    }

    /// 屏幕方向只属于 Left/Right；不得从 RTL 段的逻辑前后反推原系统动作。
    pub(crate) fn visual_right(self) -> Option<bool> {
        match self {
            Self::Left => Some(false),
            Self::Right => Some(true),
            Self::Backward | Self::Forward => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 扩选只保原指针的逻辑锚点；词组拖动保首击真实GCB及点击侧，不保事件或正文。
pub enum NativeMarkdownPagedWordAnchor {
    Selection(usize),
    /// 仅首组尚未 Ready 时使用真实首击位置；同一原查询完成后必须由 Group 替换。
    Word {
        head_utf16: usize,
        backward_bias: bool,
        /// 首击若属于表格，查询仍须重获同原行列的真实格 Source，不能把 canonical head 交正文 UBA。
        cell: Option<super::NativeMarkdownPagedCellWordTarget>,
    },
    /// 原 Core 已完成的首击组；后继拖选与跨提交保两端，不重新在新显示中扩种子。
    Group {
        from_utf16: usize,
        to_utf16: usize,
        /// 首击实际命中只决定同组往返方向，不再参与词组范围查询；跨提交也须同趟真实映射。
        origin_utf16: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 这是一次原生选词请求的真实来源坐标；原Doc/Input/几何仍由同SceneRequest与permit约束。
pub struct NativeMarkdownPagedWordSelection {
    pub head_utf16: usize,
    pub backward_bias: bool,
    pub anchor: Option<NativeMarkdownPagedWordAnchor>,
    /// 仅最新目标的真实格查询意图；已 Ready 的首组不保 Source，原首查询则另保自己的 cell 域。
    pub cell: Option<super::NativeMarkdownPagedCellWordTarget>,
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

#[derive(Clone)]
/// 当前真实格的命中事实；原row/raw票仍管理来源，内容框来自同次实测行而非分页后缀高度。
/// 不持Source正文、Doc、Native view或系统事件，延后激活仍须由唯一Body核同一次请求。
pub struct NativeMarkdownPagedCellHit {
    pub row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    pub source: omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource,
    pub content_frame: NSRect,
    pub head_utf16: usize,
    pub head_affinity: NativeMarkdownPagedHeadAffinity,
    pub backward_bias: bool,
    /// 只由同一次 Grid 词命中填写；普通 caret/Tab 不伪装成已经取得词查询来源。
    pub word_target: Option<super::NativeMarkdownPagedCellWordTarget>,
}

#[derive(Clone)]
/// 一次已解释的格指针意图；手势弱票只说明原Down尚未Up，不使异步Ready复活拖选。
/// Shift与双击种子各保原有向来源；它们都不是可重播NSEvent或另一份选择缓存。
pub struct NativeMarkdownPagedCellPointer {
    pub hit: NativeMarkdownPagedCellHit,
    pub clicks: usize,
    pub extending: bool,
    pub anchor: Option<usize>,
    pub word_anchor: Option<NativeMarkdownPagedWordAnchor>,
    pub dragging: bool,
    pub intent: Weak<()>,
}

#[derive(Clone, Copy)]
/// 子格原Down确定的局部选择种子；词组不降为caret，Shift仍保原有向anchor。
pub enum NativeMarkdownPagedCellDragAnchor {
    Caret { anchor_utf16: usize },
    Word(NativeMarkdownPagedWordAnchor),
}

#[derive(Clone)]
/// 当前原生手势已经解码的一个有限终点；窗口点留待原 Scene Ready 后转换。
/// mouse_up 只由仍拥有原 Down 的实际 Up 入口设置。接纳后的终点可完成，不能重新授予拖动寿命。
/// 不保存 NSEvent、视图、正文或第二张输入票；原输入域、代次与几何仍属于同次 request。
pub struct NativeMarkdownPagedPointerLocation {
    pub window_point: NSPoint,
    pub clicks: usize,
    pub extending: bool,
    pub anchor: Option<NativeMarkdownPagedCellDragAnchor>,
    pub dragging: bool,
    pub intent: Weak<()>,
    pub mouse_up: bool,
}

#[derive(Clone)]
/// 活动格跨向正文的真实选择动作；词拖选保原种子，普通选择保真实有向端点与换行侧。
/// 唯一父Body先完成草稿发布，再沿同次实际变更映射它，不能让旧Scene直接改canonical。
pub enum NativeMarkdownPagedCellExtension {
    Selection {
        selection: MarkdownTextSelection,
        head_affinity: NativeMarkdownPagedHeadAffinity,
        intent: Weak<()>,
    },
    Word {
        selection: NativeMarkdownPagedWordSelection,
        intent: Weak<()>,
    },
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

    /// 仅枚举当前已封存、已裁剪的链接标签几何；不查URL、不推进Parser或创建链接目录。
    /// 调用者须另证完整CPU帧已发布；visit只收集Copy值，归还Scene后才可进入Native登记。
    fn visit_link_cursor_rects(&self, _visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        Ok(())
    }

    /// 命中只读取本次已封存的可见LINK part和原GCB，不在mousedown同步解析或打开URL。
    fn link_hit(&self, _local: NSPoint) -> Result<Option<(std::ops::Range<usize>, usize)>> {
        Ok(None)
    }
    /// mouseup接纳原手势的弱权限；成功表示唯一正文owner接管意图，目标仍须由真实来源准备。
    fn follow_link(&self, _index: usize, _intent: Weak<()>) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// 同文档标题只沿本张真实Scene的原正文owner解析；不要求存在全局工作区Surface。
    /// 成功是接纳原click查询，最终仍须由新选择的SourceTarget与完整CPU帧完成可见性。
    fn follow_heading(&self, _fragment: &str, _intent: Weak<()>) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// 仅真实链接producer可在当前命中位置给出已解析目标；普通Source/Cell不从可见文字猜URL。
    fn link_target(&self, _local: NSPoint) -> Result<Option<String>> {
        Ok(None)
    }
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
    /// 连续拖选可越出物理视口；仅真实窗口owner能投到已封存的最近可见行，原物理点不变。
    fn drag_selection_point(&self, local: NSPoint) -> Result<NSPoint> {
        Ok(local)
    }
    /// 双击必须消费原字形命中及真实点击侧；单个source整数不足以判断词边界的哪一组。
    fn word_hit(
        &self,
        _local: NSPoint,
    ) -> Result<(
        usize,
        bool,
        Option<super::NativeMarkdownPagedCellWordTarget>,
    )> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// 只把本次真实Grid命中交原活动格owner；word仅要求真实字形点击侧，不在此选择词范围。
    fn cell_hit(&self, _local: NSPoint, _word: bool) -> Result<Option<NativeMarkdownPagedCellHit>> {
        Ok(None)
    }
    /// 三击与菜单段落选择消费原Source已认证的段落范围，不以显示换行或空格猜一个段落。
    fn paragraph_selection(
        &self,
        _position: usize,
        _affinity: NativeMarkdownPagedHeadAffinity,
    ) -> Result<MarkdownTextSelection> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    fn mouse_selection(
        &self,
        local: NSPoint,
        clicks: usize,
        anchor: Option<usize>,
        extend: bool,
    ) -> Result<(MarkdownTextSelection, NativeMarkdownPagedHeadAffinity)>;
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
    /// 父正文有真实活动格时，右键编辑动作仍归那个客户；普通正文不创建替代输入视图。
    fn context_menu_client(
        &self,
    ) -> Result<Option<Rc<Retained<super::NativeMarkdownPagedTextView>>>> {
        Ok(None)
    }
    fn current(&self, epoch: u64) -> bool;
    /// 只报告原owner已经接受关闭，不把current=false、暂缺Scene或准备中推断为退出。
    /// 非Body客户没有Body关闭事务，默认false；这不授予输入、绘图或资源归还成功。
    fn cursor_rects_retired(&self) -> bool {
        false
    }
    /// bounds_width是本次真实有限正文宽；无滚动owner的独立组件直接消费此输入，不填默认宽。
    fn virtual_viewport(
        &self,
        epoch: u64,
        bounds_width: f64,
    ) -> Result<NativeMarkdownPagedVirtualViewport>;
    fn ready_scene(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>>;
    fn scene_current(&self, key: &NativeMarkdownPagedSceneKey) -> bool;
    /// 真实scrollWheel先撤旧模式位置，再沿原NSView responder链滚动；独立场景没有模式票可撤。
    fn user_scroll(&self, _epoch: u64) -> Result<()> {
        Ok(())
    }
    /// 同一真实Down越过视口后，原滚动owner按窗口终点移动有限距离；不读取或改写正文。
    fn drag_autoscroll(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _point: NSPoint,
        _intent: &Weak<()>,
    ) -> Result<bool> {
        Ok(false) // 无滚动owner的独立客户没有可滚动图，不借父responder猜目标。
    }
    /// 这里只通知已有coalesced调度入口；不允许在输入回调内整段排版或静默替换canonical。
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// 文档头尾已提交后，只接收这张原收据的显示意图；实际Source绑定和滚动由唯一正文
    /// producer继续。接收成功不是Ready，也不能让调用方重放已经发生的选择提交。
    fn reveal_committed_selection(&self, commit: &NativeMarkdownPagedInputCommit) -> Result<()>;
    /// 插表事务已发布，真实预览 Body 可接首格激活；独立 Source 客户保留上方原 caret reveal。
    /// 原收据只借本栈，不能保住旧 Scene、重新插表或从较新的选择创建另一张激活票。
    fn activate_inserted_table(&self, _commit: &NativeMarkdownPagedInputCommit) -> Result<()> {
        Ok(()) // 不拥有表格编辑图的原 Source/独立消费者没有需要挂载的 Cell。
    }
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
    /// 保留系统的逻辑/视觉 Word 意图，是否需要布局由原 Body 的实际消费者决定。
    fn move_word(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _motion: NativeMarkdownPagedWordMotion,
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
        _gesture: Option<Weak<()>>,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::UnsupportedCommand)
    }
    /// key尚未解释时保原event，marked回调仍同步；false表示既有直接键盘路径。
    fn defer_key_down(
        &self,
        _event: &objc2_app_kit::NSEvent,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 仅公开菜单在真实屏障后排队；不捕新Source或建立默认事件图。
    fn has_native_input_barrier(&self) -> bool {
        false
    }
    /// Paste携带动作到达时已捕获的有限新文字，其余只保原typed selector。
    fn defer_native_command(
        &self,
        _selector: objc2::runtime::Sel,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 文件路径与原鼠标命中只进入现存输入队列；None锚点表示等前一步真实选择后粘贴。
    /// drop的二元组为原canonical generation及真实Scene命中的UTF-16位置，不代表后继文稿坐标。
    fn defer_attachments(
        &self,
        _paths: Vec<std::path::PathBuf>,
        _drop: Option<(u64, usize)>,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// URL/批次在入队前拒绝也须等原链结束显示；true仅表示原owner接住了这一条有限业务反馈。
    fn defer_attachment_rejection(&self, _message: &str) -> bool {
        false
    }
    /// 原格式动作可排在已接纳导航之后；没有屏障的客户继续原同步事务。
    fn defer_format(
        &self,
        _action: omni_markdown_core::MarkdownFormatAction,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 插表保领域语义，不提前在旧caret生成/插入Markdown文本。
    fn defer_table_insertion(
        &self,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 原折叠菜单和行号意图可排在未完成导航之后；队首才解析真实当前段落。
    fn defer_source_fold(
        &self,
        _action: super::NativeMarkdownPagedSourceFoldAction,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 只有当前解释回调的原before与这张Rc收据能交接后继事件权限。
    fn begin_interpreted_commit(
        &self,
        _before: NativeMarkdownPagedInputState,
        _commit: Rc<NativeMarkdownPagedInputCommit>,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 前一步已接纳同张收据，通知后只完成原显示/permit交接，不读取后继状态补签。
    fn finish_interpreted_commit(
        &self,
        _commit: &NativeMarkdownPagedInputCommit,
        _reveal: bool,
    ) -> Result<()> {
        Err(super::NativeMarkdownPagedTextViewError::Revoked)
    }
    /// 文档端点在原Word/Visual链后解释；true只表示接纳，不代表选择已移动或EOF可见。
    fn defer_document_motion(
        &self,
        _end: bool,
        _extend: bool,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // 没有输入链的客户保留原同步端点合同，不建立备用事件队列。
    }
    /// 只保已有Body语义命令的到达顺序；真正续行/缩进必须消费前一步收据后的原Core事务。
    fn defer_source_command(
        &self,
        _command: omni_markdown_core::MarkdownEditingCommand,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // 没有原输入链的客户仍同步处理，Cell不借该入口伪造表格导航。
    }
    /// 原移动仍在逐量子准备时，普通退格/前删必须排在它后面；false保既有同步路径。
    /// 接纳只记录方向，实际完整GCB和原标记命令须由前一步真实提交后的同一个Source决定。
    fn defer_grapheme_delete(
        &self,
        _forward: bool,
        _permit: &crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false) // 不拥有在途输入链的客户不创建备用队列或改写原删除语义。
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
    /// 组合中的实际子格先同步消费这一个真实鼠标事件；不存事件，也不在父view移焦后补交。
    fn handle_cell_marked_mouse(&self, _event: &objc2_app_kit::NSEvent) -> Result<bool> {
        Ok(false)
    }
    /// 真实child越界Drag只在本同步栈借原事件；父Body从自己当前Scene命中目标，不重播mouse方法。
    fn drag_cell_pointer_outside(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedPointerLocation,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 跨出活动格的选择沿原父子提交边界；true已接管，调用方不得再消费旧Scene修改正文。
    fn extend_cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _extension: NativeMarkdownPagedCellExtension,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 真实父Body可接纳当前格指针，true才停止普通正文选择；其它实际客户保原Source行为。
    fn cell_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedCellPointer,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Scene 尚在准备时只接同一个 Caret 手势的最新真实窗口终点；false 明确表示没有接纳。
    /// 真实 Up 的准入必须在原 View 撤 Down 票之前完成，之后不以 Weak 活性猜测终点或取消。
    fn defer_pointer(
        &self,
        _request: &NativeMarkdownPagedSceneRequest,
        _pointer: NativeMarkdownPagedPointerLocation,
        _permit: crate::native_surface::paged_input::NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        Ok(false)
    }
    /// 新 Down、键盘、命令与关闭撤销旧的已接纳终点；纯撤权不申请新 Scene，也不撤其它 Cell 导航。
    fn cancel_deferred_pointer(&self) -> bool {
        false
    }
    /// 原Up只结束已存在的手势；即使Scene正在准备也不得重新打开格或补发Down。
    fn finish_cell_pointer(&self, _request: &NativeMarkdownPagedSceneRequest) -> Result<()> {
        Ok(())
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
