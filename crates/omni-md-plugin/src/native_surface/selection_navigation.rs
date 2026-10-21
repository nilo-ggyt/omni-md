//! Derive native navigation targets from proven logical selections, retaining TextKit objects only for synchronous calculation.

use super::*;
use objc2_app_kit::{
    NSTextContentManager, NSTextLocation, NSTextSelection, NSTextSelectionAffinity,
    NSTextSelectionGranularity, NSTextSelectionNavigation, NSTextSelectionNavigationDestination,
    NSTextSelectionNavigationDirection, NSTextSelectionNavigationModifier,
};

/// Describe system-interpreted direction/boundary actions without a second physical-key map.
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionMotion {
    pub(super) direction: NSTextSelectionNavigationDirection,
    pub(super) destination: NSTextSelectionNavigationDestination,
    pub(super) extending: bool,
}

/// Return finite navigation coordinates; temporary system selection, fragment and body claims end before returning.
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionTarget {
    pub(super) selection: MarkdownTextSelection,
    pub(super) head_upstream: bool,
    pub(super) line_offset: f64,
}

/// Establish the first vertical goal by inverse-checking actual caret geometry; a short line cannot replace that target.
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionColumn {
    pub(super) line_offset: f64,
    pub(super) container_x: f64,
}

/// Carry one actual mouse event's finite point, original selection and granularity, without adopting an old frame as a new anchor.
#[derive(Clone, Copy)]
pub(super) struct NativeMarkdownSelectionPointQuery {
    pub(super) point: NSPoint,
    pub(super) anchor: Option<(MarkdownTextSelection, NSTextSelectionGranularity)>,
    pub(super) extending: bool,
    pub(super) selecting: bool,
    pub(super) length: usize,
}

/// Require the exact graph registered by this view; never create a second layout through a compatibility layoutManager getter.
pub(super) struct NativeMarkdownSelectionGraph {
    // Keep the just-verified graph alive during this synchronous call only, outside requests, caches and successors.
    _manager: Retained<NSTextLayoutManager>,
    content: Retained<NSTextContentManager>,
    _container: Retained<NSTextContainer>,
    origin: Retained<ProtocolObject<dyn NSTextLocation>>,
    navigation: Retained<NSTextSelectionNavigation>,
}

/// Guard every native operation with the caller's original action ticket; failure cannot renew it from current state.
fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work)
        .map_err(|_| "选择动作对应的笔记、窗口或输入已经改变".to_owned())
}

impl NativeMarkdownSelectionGraph {
    /// Match the view's registered and actual manager/container/content/storage identities before lending navigation.
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
        // Equal contents do not substitute for the sole layout manager registered by this view.
        if !std::ptr::eq(&*manager, &*actual_manager) {
            return Err("选择不能使用其他文本布局".into());
        }
        let container = step(current, || manager.textContainer())?.ok_or("文本容器已断开")?;
        let actual_container = step(current, || {
            // SAFETY: Borrow this live main-thread view's public nullable getter without creating a compatibility layout.
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
        // Content ownership must match too; a manager retained from a transferred graph is not sufficient.
        if !std::ptr::eq::<NSTextContentManager>(&*content, actual_content.as_super()) {
            return Err("选择不能使用其他文本内容".into());
        }
        let storage = step(current, || view.ivars().text_storage.load())?.ok_or("笔记已经关闭")?;
        let actual_storage =
            step(current, || actual_content.textStorage())?.ok_or("文本内容已失去原存储")?;
        // Require the actual original piece storage before deriving any logical-source coordinates.
        if !std::ptr::eq::<NSTextStorage>(storage.as_super(), &*actual_storage) {
            return Err("选择不能使用其他笔记的存储".into());
        }
        if step(current, || content.hasEditingTransaction())? {
            // Native editing has not committed its logical state; do not navigate an intermediate graph.
            return Err("文本输入尚未结束，暂时不能改变选择".into());
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range = step(current, || provider.documentRange())?;
        let origin = step(current, || document_range.location())?;
        let navigation = step(current, || manager.textSelectionNavigation())?;
        // Release incidental getter claims before the final pure authority check; input arising
        // during destruction cannot inherit a result calculated for the previous action.
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

    /// Derive explicit direction from the caller's Core selection, excluding unreliable nonempty affinity stored by the system view.
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
        // Nonempty affinity identifies the logical active endpoint; a caret's affinity instead selects its wrap side.
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
            // Install only a line-fragment-relative offset inverse-checked in this same layout, never a screen-space x.
            step(current, || native.setAnchorPositionOffset(offset))?;
        }
        Ok(native)
    }

    /// Require one actual caret; multiple ranges, a secondary bidi position or nonempty output cannot silently become its first item.
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
        // Accept only the two declared native affinities; unknown values cannot establish a wrap side.
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

    /// Resolve standard motion through the original native graph, preserving the Core anchor for extension.
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
            // Extend moves from the actual active endpoint and keeps the Core anchor,
            // without relying on the view's private persistent extension anchor.
            MarkdownTextSelection::caret(selection.head_utf16)
        } else {
            // Plain motion derives collapse semantics from the entire original selection,
            // rather than collapsing to its head and moving one additional character.
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

    /// Query one existing caret and inverse-check its position/affinity through the same container point, without a default zero column.
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
            // Interior positions can have identical geometry on both sides; ignore affinity
            // disagreement only after observing the exact same rectangle for both.
        }
        let origin = step(current, || view.textContainerOrigin())?;
        Ok(NativeMarkdownSelectionColumn {
            line_offset: inverse.line_offset,
            container_x: frame.origin.x - origin.x,
        })
    }

    /// Navigate actual container coordinates, retaining only finite output for this event and no cached native selection objects.
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
        // Translate viewport bounds and the pointer into the same original text-container coordinates.
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
            // Only an explicitly extending gesture supplies Extend; ordinary point selection uses no modifier.
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

    /// Double/triple clicks use the original TextKit graph's word/paragraph boundaries, without scanning or copying paragraph text.
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

    /// Preserve both explicitly supplied endpoints during granularity expansion; do not borrow an unproven system-view direction.
    pub(super) fn enclosing_selection(
        &self,
        selection: MarkdownTextSelection,
        head_upstream: bool,
        granularity: NSTextSelectionGranularity,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        if selection.anchor_utf16 > length || selection.head_utf16 > length {
            // An old out-of-range endpoint cannot be clamped into the successor document's EOF.
            return Err("当前选择已经超出笔记".into());
        }
        let target = (|| {
            // Expand the complete original direction on the same graph; reducing a nonempty
            // range to its head would omit the word/paragraph at its other endpoint.
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
            // Validate actual native output for one range, no secondary bidi position, original
            // source bounds and finite column values, without manufacturing a replacement result.
            self.range_target(&expanded, length, current)
        })();
        // Input selection, expanded result and internal range claims end before coordinates may leave.
        if !current() {
            return Err("粒度展开完成前原选择权限已经失效".into());
        }
        target
    }

    /// Select the actual downstream visual line at the ordered lower endpoint, without inheriting an unproven internal drag anchor.
    pub(super) fn visual_line_at(
        &self,
        low: usize,
        length: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownSelectionTarget, String> {
        if low > length {
            // An obsolete out-of-range position cannot be truncated into a legitimate EOF.
            return Err("选行起点已经超出当前笔记".into());
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
                // Estimated fragments cannot establish an actual laid-out visual line.
                return Err("选行起点的实际行布局尚未完成".into());
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
                // A transferred element cannot be interpreted relative to this graph's old origin.
                return Err("选行文本元素不属于原内容管理器".into());
            }

            // Convert public NSTextRange locations into document offsets. Neither object
            // addresses nor fragment-local indices are valid substitutes for source coordinates.
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
                // Ordinary endpoints choose downstream; the preceding fragment's trailing edge cannot substitute.
                return Err("选行片段没有覆盖原有序起点".into());
            }

            let line = step(current, || {
                fragment.textLineFragmentForTextLocation_isUpstreamAffinity(&location, false)
            })?
            .ok_or("选行起点没有实际下游视觉行")?;
            let local = step(current, || line.characterRange())?;
            if local.location == objc2_foundation::NSNotFound as usize {
                // An absent character range proves neither an empty line nor EOF.
                return Err("实际视觉行缺少有效字符范围".into());
            }
            let local_end = local
                .location
                .checked_add(local.length)
                .ok_or("实际视觉行的局部范围溢出")?;
            // characterRange is relative to the entire text element's source, even after
            // fragmentation; using the fragment start would translate the same units twice.
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
                    // Only an actually laid-out terminal empty line may produce this empty selection.
                    return Err("零长视觉行不是原文档的实际 EOF 空行".into());
                }
            } else if low < start || low > end || (low == end && low != length) {
                // The actual final line may end at EOF without a trailing newline; ordinary
                // line boundaries still choose downstream rather than reusing the previous line.
                return Err("实际下游视觉行没有包含选行起点".into());
            }

            // Getters can reenter layout. Recheck original source ranges, line values and
            // completed state before returning, rather than combining facts from different frames.
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
                // The new forward range inherits no target column; shared publication clears its vertical goal.
                line_offset: 0.0,
            })
        })();
        // Fragment, line, element, location and range claims remain confined to this synchronous stack.
        if !current() {
            return Err("选行完成前原笔记、窗口或输入已经改变".into());
        }
        target
    }

    /// Interpret navigation output derived directly from this explicit input, never bridged selection state read back from the view.
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
        // Native affinity supplies the output's logical direction; preserve ordered bounds while
        // reconstructing the original anchor/head meaning rather than guessing from screen order.
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
