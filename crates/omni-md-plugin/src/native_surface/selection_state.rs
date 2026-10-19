//! 与view/fold同生灭的有限选择事实；不持正文、Native对象或异步执行权限。

use super::{MarkdownTextSelection, NSRange};
use std::cell::Cell;

/// 原L1/L2光标所属的真实level与逻辑字素侧；不表示soft-wrap换行侧或屏幕左右。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "native-paged-layout-probe")]
pub struct NativeMarkdownBidiCaretSide {
    pub level: u8,
    /// true绑定位置后的字素，false绑定位置前的字素。
    pub logical_after: bool,
}

/// 连续垂直移动的目标列；两个横坐标分别属于行片段和container，不能互换。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownVerticalGoal {
    pub(super) line_offset: f64,
    pub(super) container_x: f64,
    pub(super) width_key: i64,
    pub(super) geometry_epoch: u64,
}

impl NativeMarkdownVerticalGoal {
    /// 只接收原布局实际caret给出的有限坐标；调用方另外核同Source、宽度和布局代次。
    /// geometry_epoch属于原排版坐标系，不能用随滚动改变的View事件代次冒充。
    #[must_use]
    pub fn new(
        line_offset: f64,
        container_x: f64,
        width_key: i64,
        geometry_epoch: u64,
    ) -> Option<Self> {
        if !line_offset.is_finite() || !container_x.is_finite() {
            // 非有限目标不能进入后继Native导航；保留原选择而非代入默认列。
            return None;
        }
        Some(Self {
            line_offset,
            container_x,
            width_key,
            geometry_epoch,
        })
    }

    pub(super) fn is_finite(self) -> bool {
        self.line_offset.is_finite() && self.container_x.is_finite()
    }

    /// 原行片段内的真实偏移；它与container_x属于不同坐标，不可互换。
    #[cfg(feature = "native-paged-layout-probe")]
    #[must_use]
    pub fn line_offset(self) -> f64 {
        self.line_offset
    }

    /// 原文本容器的实际横坐标；短行返回的目标位置不能覆盖这个连续垂直目标。
    #[cfg(feature = "native-paged-layout-probe")]
    #[must_use]
    pub fn container_x(self) -> f64 {
        self.container_x
    }

    /// 原文本容器实际行宽的键，不等于带边距的窗口或滚动视口宽度。
    #[cfg(feature = "native-paged-layout-probe")]
    #[must_use]
    pub fn width_key(self) -> i64 {
        self.width_key
    }

    /// 原producer的排版坐标代次，随Source、样式或布局宽度变化失效。
    #[cfg(feature = "native-paged-layout-probe")]
    #[must_use]
    pub fn layout_epoch(self) -> u64 {
        self.geometry_epoch
    }
}

/// 只有原收据声明的两个范围属于本次安装；其他通知必须交回真实输入路径。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownSelectionInstallObservation {
    OwnDeclared,
    Foreign,
}

#[derive(Clone, Copy)]
struct KnownSelection {
    selection: MarkdownTextSelection,
    head_is_upstream: Option<bool>,
    vertical_goal: Option<NativeMarkdownVerticalGoal>,
    #[cfg(feature = "native-paged-layout-probe")]
    bidi_side: Option<NativeMarkdownBidiCaretSide>,
}

#[derive(Clone, Copy)]
struct NativeInstall {
    id: u64,
    generation: u64,
    serial: u64,
    before: NSRange,
    after: NSRange,
    selection: MarkdownTextSelection,
}

/// 事实和栈内安装租约分别存放；已知方向本身不能授权旧动作跨Native重入继续。
pub(super) struct NativeMarkdownSelectionState {
    generation: Cell<u64>,
    serial: Cell<u64>,
    known: Cell<Option<KnownSelection>>,
    install: Cell<Option<NativeInstall>>,
    next_install_id: Cell<u64>,
    sealed: Cell<bool>,
}

impl NativeMarkdownSelectionState {
    pub(super) fn new(generation: u64, selection: MarkdownTextSelection) -> Self {
        Self {
            generation: Cell::new(generation),
            serial: Cell::new(0),
            known: Cell::new(Some(KnownSelection {
                selection,
                head_is_upstream: None,
                vertical_goal: None,
                #[cfg(feature = "native-paged-layout-probe")]
                bidi_side: None,
            })),
            install: Cell::new(None),
            next_install_id: Cell::new(0),
            sealed: Cell::new(false),
        }
    }

    pub(super) fn serial(&self) -> u64 {
        self.serial.get()
    }

    /// generation与当前Core选择都相同才返回事实；未被adapter观察的修改不会借旧方向续权。
    pub(super) fn known_selection(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<MarkdownTextSelection> {
        self.known_metadata(generation, selection)
            .map(|known| known.selection)
    }

    pub(super) fn known_head_is_upstream(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<bool> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.head_is_upstream)
    }

    pub(super) fn known_vertical_goal(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<NativeMarkdownVerticalGoal> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.vertical_goal)
    }

    #[cfg(feature = "native-paged-layout-probe")]
    pub(super) fn known_bidi_side(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<NativeMarkdownBidiCaretSide> {
        self.known_metadata(generation, selection)
            .and_then(|known| known.bidi_side)
    }

    fn known_metadata(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> Option<KnownSelection> {
        if self.sealed.get() || self.generation.get() != generation {
            // 换代和seal后没有可继承的Native方向或目标列。
            return None;
        }
        self.known
            .get()
            .filter(|known| known.selection == selection)
    }

    /// 仅在同一纯Core发布栈调用；每次发布都撤销旧capture，包括范围不变的方向/意图转换。
    pub(super) fn publish(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
        proven_head: bool,
        head_is_upstream: Option<bool>,
        vertical_goal: Option<NativeMarkdownVerticalGoal>,
    ) -> u64 {
        self.generation.set(generation);
        self.discard_install();
        let known = (proven_head || selection.anchor_utf16 == selection.head_utf16).then_some(
            KnownSelection {
                selection,
                head_is_upstream,
                vertical_goal: vertical_goal.filter(|goal| goal.is_finite()),
                #[cfg(feature = "native-paged-layout-probe")]
                bidi_side: None, // 普通输入/历史没有原视觉侧的继承权。
            },
        );
        self.known.set(known);
        self.advance_serial()
    }

    /// 同一次纯Core选择提交签入真实视觉侧；没有额外serial、回调或后置修补窗口。
    #[cfg(feature = "native-paged-layout-probe")]
    pub(super) fn publish_with_bidi_side(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
        proven_head: bool,
        head_is_upstream: Option<bool>,
        vertical_goal: Option<NativeMarkdownVerticalGoal>,
        bidi_side: Option<NativeMarkdownBidiCaretSide>,
    ) -> u64 {
        let serial = self.publish(
            generation,
            selection,
            proven_head,
            head_is_upstream,
            vertical_goal,
        );
        if let Some(mut known) = self.known.get() {
            known.bidi_side = bidi_side;
            self.known.set(Some(known));
        } // 仍在原无可重入操作的发布栈内；唯一serial只由上方原publish递增一次。
        serial
    }

    /// 实际新输入只撤销旧安装/动作权限；当前Core方向事实仍可供新动作捕获。
    pub(super) fn revoke_install(&self) {
        self.discard_install();
        self.advance_serial();
    }

    pub(super) fn discard_install(&self) {
        self.install.set(None);
    }

    pub(super) fn clear_vertical_goal(&self) {
        if let Some(mut known) = self.known.get() {
            // 布局变化不改逻辑anchor/head，但旧行内偏移不能应用到新几何。
            known.vertical_goal = None;
            known.head_is_upstream = None;
            #[cfg(feature = "native-paged-layout-probe")]
            {
                known.bidi_side = None;
            } // 原Source/宽度/样式的part侧不能用于下一套实际视觉行。
            self.known.set(Some(known));
        }
    }

    /// 未知选择或外部换代失去方向证据；不从有序范围伪造一个新head。
    pub(super) fn reset(&self, generation: u64) {
        self.generation.set(generation);
        self.known.set(None);
        self.revoke_install();
    }

    pub(super) fn seal(&self) {
        self.sealed.set(true);
        self.reset(self.generation.get());
    }

    /// 收据已经认证后才建立唯一栈内lease；覆盖前一个id使旧guard的Drop不能清掉后继。
    pub(super) fn begin_install(
        &self,
        generation: u64,
        serial: u64,
        before: NSRange,
        selection: MarkdownTextSelection,
    ) -> Option<u64> {
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let after = NSRange::new(start, end - start);
        if self.sealed.get()
            || self.generation.get() != generation
            || self.serial.get() != serial
            || !valid_declared_range(before)
            || !valid_declared_range(after)
        {
            // 过期收据和溢出范围都不能安装；不读取当前值给旧收据补票。
            return None;
        }
        let id = self.next_install_id.get().checked_add(1)?;
        self.next_install_id.set(id);
        self.install.set(Some(NativeInstall {
            id,
            generation,
            serial,
            before,
            after,
            selection,
        }));
        Some(id)
    }

    pub(super) fn install_matches(&self, generation: u64, serial: u64, id: u64) -> bool {
        !self.sealed.get()
            && self.generation.get() == generation
            && self.serial.get() == serial
            && self.install.get().is_some_and(|install| {
                install.id == id && install.generation == generation && install.serial == serial
            })
    }

    pub(super) fn observe_install(
        &self,
        range: NSRange,
    ) -> Option<NativeMarkdownSelectionInstallObservation> {
        let install = self.install.get()?;
        let declared = self.install_matches(install.generation, install.serial, install.id)
            && valid_declared_range(range)
            && (range == install.before || range == install.after);
        Some(if declared {
            NativeMarkdownSelectionInstallObservation::OwnDeclared
        } else {
            NativeMarkdownSelectionInstallObservation::Foreign
        })
    }

    pub(super) fn install_core_is_current(
        &self,
        generation: u64,
        selection: MarkdownTextSelection,
    ) -> bool {
        self.install.get().is_some_and(|install| {
            self.install_matches(generation, install.serial, install.id)
                && install.selection == selection
        })
    }

    pub(super) fn finish_install(&self, serial: u64, id: u64) {
        if self
            .install
            .get()
            .is_some_and(|install| install.serial == serial && install.id == id)
        {
            // 只清自己仍占有的slot；旧guard不能擦除重入后建立的新lease。
            self.install.set(None);
        }
    }

    fn advance_serial(&self) -> u64 {
        let serial = self
            .serial
            .get()
            .checked_add(1)
            .expect("selection serial overflow");
        self.serial.set(serial);
        serial
    }
}

/// 这里只验证有限声明，不把旧Native before范围误当当前正文的可读授权。
fn valid_declared_range(range: NSRange) -> bool {
    range.location != objc2_foundation::NSNotFound as usize
        && range.location.checked_add(range.length).is_some()
}
