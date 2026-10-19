//! 从已证明的逻辑选择计算原生导航目标；这里只持有同步调用所需的临时 TextKit 对象。

use super::*;
use objc2_app_kit::{
    NSTextContentManager, NSTextLocation, NSTextSelection, NSTextSelectionAffinity,
    NSTextSelectionGranularity, NSTextSelectionNavigation, NSTextSelectionNavigationDestination,
    NSTextSelectionNavigationDirection, NSTextSelectionNavigationModifier,
};

/// 标准系统动作按方向和边界分派；键盘解释仍由 AppKit 完成，不维护第二份按键映射。
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionMotion {
    pub(super) direction: NSTextSelectionNavigationDirection,
    pub(super) destination: NSTextSelectionNavigationDestination,
    pub(super) extending: bool,
}

/// 导航只交还有限标量；系统 selection、fragment 和正文对象均在返回之前释放。
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionTarget {
    pub(super) selection: MarkdownTextSelection,
    pub(super) head_upstream: bool,
    pub(super) line_offset: f64,
}

/// 第一次垂直动作的列由实际 caret 几何逆核；短行不能覆盖用户已经选定的目标列。
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionColumn {
    pub(super) line_offset: f64,
    pub(super) container_x: f64,
}

/// 一个真实鼠标事件的点、初始选择和粒度合并为有限值；不把上一帧结果误作新的锚点。
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionPointQuery {
    pub(super) point: NSPoint,
    pub(super) anchor: Option<(MarkdownTextSelection, NSTextSelectionGranularity)>,
    pub(super) extending: bool,
    pub(super) selecting: bool,
    pub(super) length: usize,
}

/// 临时图必须与此 view 登记的唯一图完全相同，不能通过兼容 layoutManager 建立第二套布局。
pub(super) struct NativeMarkdownSelectionGraph {
    // 同步计算期间保活刚核对的唯一图；这些 claims 不进入请求、缓存或后继回调。
    _manager: Retained<NSTextLayoutManager>,
    content: Retained<NSTextContentManager>,
    _container: Retained<NSTextContainer>,
    origin: Retained<ProtocolObject<dyn NSTextLocation>>,
    navigation: Retained<NSTextSelectionNavigation>,
}

/// 每个 Native 调用都守调用者的原动作票；失败不读取当前状态重新签发权限。
fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work)
        .map_err(|_| "选择动作对应的笔记、窗口或输入已经改变".to_owned())
}

impl NativeMarkdownSelectionGraph {
    pub(super) fn load(
        view: &PieceBackedMarkdownTextView,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let manager = step(current, || {
            view.ivars().text_layout_manager.get().and_then(Weak::load)
        })?
        .ok_or("文本布局已断开")?;
        let actual_manager =
            step(current, || view.textLayoutManager())?.ok_or("文本视图已失去原布局")?;
        if !std::ptr::eq(&*manager, &*actual_manager) {
            return Err("选择不能使用其他文本布局".into());
        }
        let container = step(current, || manager.textContainer())?.ok_or("文本容器已断开")?;
        let actual_container = step(current, || {
            // SAFETY: 主线程活 view 的公开 nullable getter；没有设置或创建兼容布局。
            unsafe { view.textContainer() }
        })?
        .ok_or("文本视图已失去原容器")?;
        if !std::ptr::eq(&*container, &*actual_container) {
            return Err("选择不能使用其他文本容器".into());
        }
        let content =
            step(current, || manager.textContentManager())?.ok_or("文本内容管理器已断开")?;
        let actual_content =
            step(current, || view.textContentStorage())?.ok_or("文本视图已失去原内容管理器")?;
        if !std::ptr::eq::<NSTextContentManager>(&*content, actual_content.as_super()) {
            return Err("选择不能使用其他文本内容".into());
        }
        let storage = step(current, || view.ivars().text_storage.load())?.ok_or("笔记已经关闭")?;
        let actual_storage =
            step(current, || actual_content.textStorage())?.ok_or("文本内容已失去原存储")?;
        if !std::ptr::eq::<NSTextStorage>(storage.as_super(), &*actual_storage) {
            return Err("选择不能使用其他笔记的存储".into());
        }
        if step(current, || content.hasEditingTransaction())? {
            return Err("文本输入尚未结束，暂时不能改变选择".into());
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range = step(current, || provider.documentRange())?;
        let origin = step(current, || document_range.location())?;
        let navigation = step(current, || manager.textSelectionNavigation())?;
        // 额外 getter 的局部 claims 先析构，再纯核原票；不能让析构中出现的输入继承旧结果。
        drop(document_range);
        drop(actual_storage);
        drop(storage);
        drop(actual_content);
        drop(actual_container);
        drop(actual_manager);
        if !current() {
            return Err("文本选择的原图权限已经失效".into());
        }
        Ok(Self {
            _manager: manager,
            content,
            _container: container,
            origin,
            navigation,
        })
    }

    /// 显式方向只来自调用方已持有的 Core 选择；系统 view 中失真的非空 affinity 不参与。
    fn selection(
        &self,
        selection: MarkdownTextSelection,
        head_upstream: bool,
        line_offset: Option<f64>,
        granularity: NSTextSelectionGranularity,
        current: &impl Fn() -> bool,
    ) -> Result<Retained<NSTextSelection>, String> {
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let start = isize::try_from(start).map_err(|_| "选择起点超出原生坐标范围")?;
        let end = isize::try_from(end).map_err(|_| "选择终点超出原生坐标范围")?;
        let from = step(current, || {
            self.content
                .locationFromLocation_withOffset(&self.origin, start)
        })?
        .ok_or("无法定位选择起点")?;
        let to = step(current, || {
            self.content
                .locationFromLocation_withOffset(&self.origin, end)
        })?
        .ok_or("无法定位选择终点")?;
        let allocated = step(current, NSTextRange::alloc)?;
        let range = step(current, || {
            NSTextRange::initWithLocation_endLocation(allocated, &from, Some(&to))
        })?
        .ok_or("原生选择范围无效")?;
        // 非空选择的 affinity 表示 head 所在逻辑端；caret 的 affinity 只表示换行侧。
        let upstream = if start == end {
            head_upstream
        } else {
            selection.head_utf16 < selection.anchor_utf16
        };
        let affinity = if upstream {
            NSTextSelectionAffinity::Upstream
        } else {
            NSTextSelectionAffinity::Downstream
        };
        let allocated = step(current, NSTextSelection::alloc)?;
        let native = step(current, || {
            NSTextSelection::initWithRange_affinity_granularity(
                allocated,
                &range,
                affinity,
                granularity,
            )
        })?;
        if let Some(offset) = line_offset {
            if !offset.is_finite() {
                return Err("选择目标列无效".into());
            }
            // 只安装同一布局中已经逆核的行片段相对 offset，不把屏幕 x 当作此属性。
            step(current, || native.setAnchorPositionOffset(offset))?;
        }
        Ok(native)
    }

    /// 一个目标必须是实际单 caret；多范围、双向次位置或非空结果都不能悄悄取第一项。
    fn caret_target(
        &self,
        native: &NSTextSelection,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<(usize, bool, f64), String> {
        if step(current, || native.secondarySelectionLocation())?.is_some() {
            return Err("当前双向文本插入位置尚不能唯一确定".into());
        }
        let ranges = step(current, || native.textRanges())?;
        if step(current, || ranges.count())? != 1 {
            return Err("当前选择不是单一文本范围".into());
        }
        let range = step(current, || ranges.objectAtIndex(0))?;
        let from = step(current, || range.location())?;
        let to = step(current, || range.endLocation())?;
        let from = step(current, || {
            self.content
                .offsetFromLocation_toLocation(&self.origin, &from)
        })?;
        let to = step(current, || {
            self.content
                .offsetFromLocation_toLocation(&self.origin, &to)
        })?;
        let offset = usize::try_from(from).map_err(|_| "原生选择位置无效")?;
        if from != to || offset > length {
            return Err("导航没有产生当前笔记中的唯一插入位置".into());
        }
        let affinity = step(current, || native.affinity())?;
        let head_upstream = if affinity == NSTextSelectionAffinity::Upstream {
            true
        } else if affinity == NSTextSelectionAffinity::Downstream {
            false
        } else {
            return Err("导航返回未知的换行侧".into());
        };
        let line_offset = step(current, || native.anchorPositionOffset())?;
        if !line_offset.is_finite() {
            return Err("导航返回无效目标列".into());
        }
        Ok((offset, head_upstream, line_offset))
    }

    pub(super) fn move_selection(
        &self,
        selection: MarkdownTextSelection,
        motion: NativeMarkdownSelectionMotion,
        head_upstream: bool,
        line_offset: Option<f64>,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        let input = if motion.extending {
            // Extend 从真实活动端移动，然后保留 Core 原 anchor；从不依赖 view 的私有持续扩选锚点。
            MarkdownTextSelection::caret(selection.head_utf16)
        } else {
            // 普通方向键由完整原选择计算折叠语义，不能先取 head 再额外移动一个字符。
            selection
        };
        let native = self.selection(
            input,
            head_upstream,
            line_offset,
            NSTextSelectionGranularity::Character,
            current,
        )?;
        let result = step(current, || {
            self.navigation
                .destinationSelectionForTextSelection_direction_destination_extending_confined(
                    &native,
                    motion.direction,
                    motion.destination,
                    false,
                    false,
                )
        })?
        .ok_or("当前选择没有可用的导航目标")?;
        let (head, head_upstream, line_offset) = self.caret_target(&result, length, current)?;
        let selection = if motion.extending {
            MarkdownTextSelection {
                anchor_utf16: selection.anchor_utf16,
                head_utf16: head,
            }
        } else {
            MarkdownTextSelection::caret(head)
        };
        Ok(NativeMarkdownSelectionTarget {
            selection,
            head_upstream,
            line_offset,
        })
    }

    /// 只查询一个已经存在的 caret，再以同一容器点逆核位置和换行侧，不从默认零列起步。
    pub(super) fn column_for_head(
        &self,
        view: &PieceBackedMarkdownTextView,
        head: usize,
        head_upstream: bool,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionColumn, String> {
        let frame = step(current, || {
            view.source_probe_segment_with_affinity(&(head..head), head_upstream, current)
        })?
        .map_err(|_| "当前插入位置尚无稳定行布局")?;
        if ![
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ]
        .iter()
        .all(|value| value.is_finite())
            || frame.size.height <= 0.0
        {
            return Err("当前插入位置的几何无效".into());
        }
        let inverse = self.selection_at_point(
            view,
            NativeMarkdownSelectionPointQuery {
                point: NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height * 0.5),
                anchor: None,
                extending: false,
                selecting: false,
                length,
            },
            current,
        )?;
        if inverse.selection != MarkdownTextSelection::caret(head) {
            return Err("当前插入位置和视觉行无法相互验证".into());
        }
        if inverse.head_upstream != head_upstream {
            let other = step(current, || {
                view.source_probe_segment_with_affinity(&(head..head), !head_upstream, current)
            })?
            .map_err(|_| "当前换行侧尚无稳定布局")?;
            if other != frame {
                return Err("当前插入位置的换行侧无法相互验证".into());
            }
            // 行内普通位置的两侧几何可以完全相同；只有实测同一矩形才允许忽略这个方向位。
        }
        let origin = step(current, || view.textContainerOrigin())?;
        Ok(NativeMarkdownSelectionColumn {
            line_offset: inverse.line_offset,
            container_x: frame.origin.x - origin.x,
        })
    }

    /// 点导航使用当前真实容器坐标；在一个事件里只保存系统结果的有限坐标，不缓存其对象。
    pub(super) fn selection_at_point(
        &self,
        view: &PieceBackedMarkdownTextView,
        query: NativeMarkdownSelectionPointQuery,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        let NativeMarkdownSelectionPointQuery {
            point,
            anchor,
            extending,
            selecting,
            length,
        } = query;
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err("鼠标选择位置无效".into());
        }
        let origin = step(current, || view.textContainerOrigin())?;
        let mut bounds = step(current, || view.visibleRect())?;
        bounds.origin.x -= origin.x;
        bounds.origin.y -= origin.y;
        let anchor = anchor
            .map(|(selection, granularity)| {
                self.selection(selection, false, None, granularity, current)
            })
            .transpose()?;
        let anchors = step(current, || match anchor.as_deref() {
            Some(anchor) => NSArray::from_slice(&[anchor]),
            None => NSArray::new(),
        })?;
        let modifiers = if extending {
            NSTextSelectionNavigationModifier::Extend
        } else {
            NSTextSelectionNavigationModifier::empty()
        };
        let selections = step(current, || {
            self.navigation
                .textSelectionsInteractingAtPoint_inContainerAtLocation_anchors_modifiers_selecting_bounds(
                    NSPoint::new(point.x - origin.x, point.y - origin.y),
                    &self.origin,
                    &anchors,
                    modifiers,
                    selecting,
                    bounds,
                )
        })?;
        if step(current, || selections.count())? != 1 {
            return Err("鼠标位置不能确定单一文本选择".into());
        }
        let native = step(current, || selections.objectAtIndex(0))?;
        self.range_target(&native, length, current)
    }

    /// 双击和三击只交由原 TextKit 图计算词或段边界，不扫描或复制整个段落来猜边界。
    pub(super) fn selection_granularity_at_point(
        &self,
        view: &PieceBackedMarkdownTextView,
        point: NSPoint,
        granularity: NSTextSelectionGranularity,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        let origin = step(current, || view.textContainerOrigin())?;
        let native = step(current, || {
            self.navigation
                .textSelectionForSelectionGranularity_enclosingPoint_inContainerAtLocation(
                    granularity,
                    NSPoint::new(point.x - origin.x, point.y - origin.y),
                    &self.origin,
                )
        })?
        .ok_or("当前位置没有可选的完整词或段落")?;
        self.range_target(&native, length, current)
    }

    /// 粒度展开保留调用方明确给出的两个端点；不借用 view 里无法证明方向的非空选择。
    pub(super) fn enclosing_selection(
        &self,
        selection: MarkdownTextSelection,
        head_upstream: bool,
        granularity: NSTextSelectionGranularity,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        if selection.anchor_utf16 > length || selection.head_utf16 > length {
            return Err("当前选择已经超出笔记".into()); // 任一旧端点越界都不能截断为新正文的 EOF。
        }
        let target = (|| {
            // 原完整方向交同一图展开；非空范围不能缩成 head 而遗漏另一端的词或段落。
            let input = self.selection(
                selection,
                head_upstream,
                None,
                NSTextSelectionGranularity::Character,
                current,
            )?;
            let expanded = step(current, || {
                self.navigation
                    .textSelectionForSelectionGranularity_enclosingTextSelection(
                        granularity,
                        &input,
                    )
            })?;
            // 唯一范围、双向次位置、原文边界及有限列值沿现有实际 Native 结果验证，不补造结果。
            self.range_target(&expanded, length, current)
        })(); // 输入选择、展开结果和内部范围 claims 全部结束后才允许返回有限坐标。
        if !current() {
            return Err("粒度展开完成前原选择权限已经失效".into());
        }
        target
    }

    /// 选行明确使用有序低端的下游视觉行；不继承系统内部无法认证的持续拖选锚点。
    pub(super) fn visual_line_at(
        &self,
        low: usize,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        if low > length {
            return Err("选行起点已经超出当前笔记".into()); // 越界旧位置不能被截断成合法 EOF。
        }
        let offset = isize::try_from(low).map_err(|_| "选行起点超出原生坐标范围")?;
        let target = (|| {
            let location = step(current, || {
                self.content
                    .locationFromLocation_withOffset(&self.origin, offset)
            })?
            .ok_or("无法定位当前选行起点")?;
            let fragment = step(current, || {
                self._manager.textLayoutFragmentForLocation(&location)
            })?
            .ok_or("选行起点尚无实际布局片段")?;
            if step(current, || fragment.state())?.0
                < objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable.0
            {
                return Err("选行起点的实际行布局尚未完成".into()); // 估计片段不能授权实际视觉行。
            }
            let manager =
                step(current, || fragment.textLayoutManager())?.ok_or("选行布局片段已经断开")?;
            if !std::ptr::eq(&*manager, &*self._manager) {
                return Err("选行布局片段不属于原文本图".into());
            }
            let element =
                step(current, || fragment.textElement())?.ok_or("选行布局片段缺少原文本元素")?;
            let element_content =
                step(current, || element.textContentManager())?.ok_or("选行文本元素已经断开")?;
            if !std::ptr::eq(&*element_content, &*self.content) {
                return Err("选行文本元素不属于原内容管理器".into()); // 迁移后的元素不能沿旧 origin 解释。
            }

            // NSTextRange 使用文档坐标；逐个公开 location 转换，不能把对象地址或局部序号当偏移。
            let source_range = |range: &NSTextRange| -> Result<std::ops::Range<usize>, String> {
                let from = step(current, || range.location())?;
                let to = step(current, || range.endLocation())?;
                let from = usize::try_from(step(current, || {
                    self.content
                        .offsetFromLocation_toLocation(&self.origin, &from)
                })?)
                .map_err(|_| "选行源范围起点无效")?;
                let to = usize::try_from(step(current, || {
                    self.content
                        .offsetFromLocation_toLocation(&self.origin, &to)
                })?)
                .map_err(|_| "选行源范围终点无效")?;
                if from > to || to > length {
                    return Err("选行源范围不属于当前笔记".into());
                }
                Ok(from..to)
            };
            let element_range =
                step(current, || element.elementRange())?.ok_or("选行文本元素缺少真实源范围")?;
            let fragment_range = step(current, || fragment.rangeInElement())?;
            let element_source = source_range(&element_range)?;
            let fragment_source = source_range(&fragment_range)?;
            if element_source.start > fragment_source.start
                || fragment_source.end > element_source.end
                || low < fragment_source.start
                || low > fragment_source.end
                || (low == fragment_source.end && low != length)
            {
                return Err("选行片段没有覆盖原有序起点".into()); // 普通端点取下游，不能误用前一片段的末端。
            }

            let line = step(current, || {
                fragment.textLineFragmentForTextLocation_isUpstreamAffinity(&location, false)
            })?
            .ok_or("选行起点没有实际下游视觉行")?;
            let local = step(current, || line.characterRange())?;
            if local.location == objc2_foundation::NSNotFound as usize {
                return Err("实际视觉行缺少有效字符范围".into()); // 缺失范围不等于空行或 EOF。
            }
            let local_end = local
                .location
                .checked_add(local.length)
                .ok_or("实际视觉行的局部范围溢出")?;
            // characterRange 相对整个 textElement 的原文；分片后也不能改用 fragment 起点作基准。
            let start = element_source
                .start
                .checked_add(local.location)
                .ok_or("实际视觉行的源起点溢出")?;
            let end = element_source
                .start
                .checked_add(local_end)
                .ok_or("实际视觉行的源终点溢出")?;
            if start < fragment_source.start || end > fragment_source.end || start > end {
                return Err("实际视觉行超出原文本元素或布局片段".into());
            }
            if start == end {
                if start != low || low != length {
                    return Err("零长视觉行不是原文档的实际 EOF 空行".into()); // 仅真实已布局的终端空行可保持空选区。
                }
            } else if low < start || low > end || (low == end && low != length) {
                // 无末尾换行的实际末行可在 EOF 结束；普通行末仍必须取下游，不能复用上一视觉行。
                return Err("实际下游视觉行没有包含选行起点".into());
            }

            // getter 可重入布局；交还目标前再次核同源范围、同一行值与已完成状态，不拼接两帧结果。
            let final_element_range = step(current, || element.elementRange())?
                .ok_or("选行完成前文本元素已失去源范围")?;
            let final_fragment_range = step(current, || fragment.rangeInElement())?;
            if source_range(&final_element_range)? != element_source
                || source_range(&final_fragment_range)? != fragment_source
                || step(current, || line.characterRange())? != local
                || step(current, || fragment.state())?.0
                    < objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable.0
            {
                return Err("选行期间实际行布局或源关联已经改变".into());
            }
            Ok(NativeMarkdownSelectionTarget {
                selection: MarkdownTextSelection {
                    anchor_utf16: start,
                    head_utf16: end,
                },
                head_upstream: false,
                line_offset: 0.0, // 新 forward 范围不继承目标列；共享入口须清除 vertical goal。
            })
        })(); // fragment、line、element、location 和范围 claims 只活在此同步局部栈。
        if !current() {
            return Err("选行完成前原笔记、窗口或输入已经改变".into());
        }
        target
    }

    /// 这里只解释从本次明确输入直接得到的 navigation 结果，不能用来读取 view 的桥接状态。
    fn range_target(
        &self,
        native: &NSTextSelection,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        if step(current, || native.secondarySelectionLocation())?.is_some() {
            return Err("当前双向选择不能唯一确定主端点".into());
        }
        let ranges = step(current, || native.textRanges())?;
        if step(current, || ranges.count())? != 1 {
            return Err("当前鼠标操作产生了多个文本范围".into());
        }
        let range = step(current, || ranges.objectAtIndex(0))?;
        let from = step(current, || range.location())?;
        let to = step(current, || range.endLocation())?;
        let from = usize::try_from(step(current, || {
            self.content
                .offsetFromLocation_toLocation(&self.origin, &from)
        })?)
        .map_err(|_| "鼠标选择起点无效")?;
        let to = usize::try_from(step(current, || {
            self.content
                .offsetFromLocation_toLocation(&self.origin, &to)
        })?)
        .map_err(|_| "鼠标选择终点无效")?;
        if from > to || to > length {
            return Err("鼠标选择已经超出当前笔记".into());
        }
        let affinity = step(current, || native.affinity())?;
        let head_upstream = if affinity == NSTextSelectionAffinity::Upstream {
            true
        } else if affinity == NSTextSelectionAffinity::Downstream {
            false
        } else {
            return Err("鼠标导航返回了未知的选择方向".into());
        };
        let line_offset = step(current, || native.anchorPositionOffset())?;
        if !line_offset.is_finite() {
            return Err("鼠标导航返回了无效的目标列".into());
        }
        let selection = if head_upstream {
            MarkdownTextSelection {
                anchor_utf16: to,
                head_utf16: from,
            }
        } else {
            MarkdownTextSelection {
                anchor_utf16: from,
                head_utf16: to,
            }
        };
        Ok(NativeMarkdownSelectionTarget {
            selection,
            head_upstream,
            line_offset,
        })
    }
}
