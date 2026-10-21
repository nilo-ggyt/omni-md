//! 正式场景只持当前可见表格格窗口；原结构票、显示映射与CPU绘图不建立第二份编辑图。

use super::*;
use crate::native_surface::{
    paged_line_viewport::NativeMarkdownComposedTableCell,
    text_attributes::{
        NATIVE_MARKDOWN_TABLE_BORDER, NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND,
        NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND,
    },
};
use omni_markdown_core::{
    VerifiedMarkdownLayoutTableRow, markdown_document_buffer::VerifiedMarkdownTableCellSource,
};

/// 真空显示不需要 Composer 或目录 FD；未知非空格不能借 Empty 当作已准备。
pub(in crate::native_surface::paged_body) enum GridCellContent {
    Empty(VerifiedMarkdownTableCellSource),
    /// 视口只碰真实行顶内边距，文字区尚在下方；保原格起点，不授 EOF 或已排完权限。
    BeforeText(VerifiedMarkdownTableCellSource),
    /// 原短格已完整排完，当前可见区域位于其文字下方；不能把未准备正文放入此状态。
    OutsideText(VerifiedMarkdownTableCellSource),
    Composed(NativeMarkdownComposedTableCell),
}

/// 每个格只持实际窗口或空格来源票；frame 包含原表格 padding，文字窗口另持其实际 clip。
pub(in crate::native_surface::paged_body) struct GridCellPublication {
    pub frame: NSRect,
    /// 同次实测格扣原padding，保整段真实已测前缀，不扣文字窗口scan.first_top。
    pub content_frame: NSRect,
    pub content: GridCellContent,
}

/// 尚未封存的长行只发布真实已测高度；complete 之前不画假的行尾，也不排列后续正文。
pub(in crate::native_surface::paged_body) struct GridRowPublication {
    pub row: VerifiedMarkdownLayoutTableRow,
    pub frame: NSRect,
    pub complete: bool,
    pub cells: Box<[GridCellPublication]>,
}

/// 结构行里的隐藏语法/离屏格属于Grid域；无可见矩形不能被当成普通文字继续查字号。
/// Ready只来自真实格窗口，Outside才允许调用方进入原正文几何，没有占位字形或伪高度。
pub(super) enum GridCharacterGeometry {
    Outside,
    Unavailable,
    Ready(NativeMarkdownPagedCharacterRect),
}

impl Scene {
    /// 只遍历当前有限可见格；空格、未到文字区和已离开文字区都没有链接光标权限。
    /// 每个Composed窗口内部沿原真实cell clip裁剪，不为悬停准备离屏内容或读取目标字符串。
    pub(super) fn visit_grid_link_cursor_rects(&self, visit: &mut dyn FnMut(NSRect)) -> Result<()> {
        self.validate_grids(&|| self.current())?;
        for row in &self.grids {
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window
                        .visit_link_cursor_rects(&|| self.current(), visit)
                        .map_err(scene_error)?;
                }
            }
        }
        self.validate_grids(&|| self.current())
    }

    /// 返回真实命中格；不按列均分宽度或用可见数组下标推导原表格列坐标。
    pub(super) fn grid_cell_at(&self, local: NSPoint) -> Option<&GridCellPublication> {
        self.grids
            .iter()
            .filter(|row| inside(local, row.frame))
            .flat_map(|row| row.cells.iter())
            .find(|cell| inside(local, cell.frame))
    }

    /// 无文字窗口的格仍核原 Doc/raw 票；已删除的行不能靠旧 frame 继续交付点击位置。
    fn raw_cell_range(&self, cell: &VerifiedMarkdownTableCellSource) -> Result<Range<usize>> {
        let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
        let document = owner.try_borrow().map_err(|_| Error::Busy)?;
        cell.canonical_range_for(&document)
            .cloned()
            .map_err(|cause| scene_error(ViewportError::Table(cause)))
    }

    /// 所有显示窗口先保真实 Source 首错；current 的撤销只能在实际资源检查后返回。
    pub(super) fn validate_grids(&self, current: &dyn Fn() -> bool) -> Result<()> {
        for row in &self.grids {
            for cell in &row.cells {
                match &cell.content {
                    GridCellContent::Empty(source)
                    | GridCellContent::BeforeText(source)
                    | GridCellContent::OutsideText(source) => {
                        self.raw_cell_range(source)?;
                    }
                    GridCellContent::Composed(window) => {
                        window.validate_ready(current).map_err(scene_error)?
                    }
                }
            }
        }
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// 可见格交原映射返回 canonical caret；另一真实 BiDi 位置仍明确拒绝由本层猜主侧。
    pub(super) fn grid_hit(&self, local: NSPoint) -> Result<Option<usize>> {
        let Some(cell) = self.grid_cell_at(local) else {
            return Ok(None);
        };
        match &cell.content {
            GridCellContent::Empty(source) | GridCellContent::BeforeText(source) => {
                Ok(Some(self.raw_cell_range(source)?.start))
            }
            GridCellContent::OutsideText(source) => Ok(Some(self.raw_cell_range(source)?.end)),
            GridCellContent::Composed(window) => {
                let hit = window.hit(local, &|| self.current()).map_err(scene_error)?;
                if hit.other_source_utf16.is_some() {
                    return Err(Error::UnsupportedCommand);
                }
                Ok(Some(hit.source_utf16))
            }
        }
    }

    /// 激活交付真实行票、raw票和同次完整内容框；未进入文字的格只交真实端点等待原reveal。
    /// 普通格按实测列框命中，双击侧从显示窗口的真实GCB几何取得，不猜等宽列或LTR。
    pub(super) fn grid_pointer_hit(
        &self,
        local: NSPoint,
        word: bool,
    ) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit>> {
        let Some(row) = self.grids.iter().find(|row| inside(local, row.frame)) else {
            return Ok(None);
        };
        let Some(cell) = row.cells.iter().find(|cell| inside(local, cell.frame)) else {
            return Ok(None); // 当前横向窗口没有该列真实发布，不能由整行范围反推一个格。
        };
        let (source, head_utf16, head_affinity, backward_bias) = match &cell.content {
            GridCellContent::Composed(window) => {
                let (hit, backward_bias) = if word {
                    window
                        .word_hit(local, &|| self.current())
                        .map_err(scene_error)?
                } else {
                    (
                        window.hit(local, &|| self.current()).map_err(scene_error)?,
                        false,
                    )
                };
                if hit.other_source_utf16.is_some() {
                    return Err(Error::Pending); // 同点两个逻辑位置必须等真实侧，不能静默取一个作为编辑起点。
                }
                (
                    window.canonical_cell_source().clone(),
                    hit.source_utf16,
                    Affinity::from_upstream(
                        hit.source_affinity
                            == omni_markdown_core::MarkdownComposedLineAffinity::Upstream,
                    ),
                    backward_bias,
                )
            }
            GridCellContent::Empty(source) | GridCellContent::BeforeText(source) => {
                // BeforeText还没有字形侧；这里只保raw起点，childReady后由自己的Source选词/段落。
                (
                    source.clone(),
                    self.raw_cell_range(source)?.start,
                    Affinity::Downstream,
                    false,
                )
            }
            GridCellContent::OutsideText(source) => (
                source.clone(),
                self.raw_cell_range(source)?.end,
                Affinity::Upstream,
                false,
            ),
        };
        // 词命中与当前真实 raw 列坐标在同一个 Scene 健康门内读取；行起点来自实际发布行。
        // 这只是后继查询意图，不能代替 Producer 对最新 Source/Row/raw 的权限核验。
        let word_target = if word {
            let original = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
            let document = original.try_borrow().map_err(|_| Error::Busy)?;
            let (_, _, column) = source
                .coordinate_for(&document)
                .map_err(|cause| scene_error(ViewportError::Table(cause)))?;
            Some(
                crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget {
                    row_start: row.row.source_range().start,
                    column,
                    affinity: head_affinity,
                },
            )
        } else {
            None
        };
        Ok(Some(
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit {
                row: row.row.clone(),
                source,
                content_frame: cell.content_frame,
                head_utf16,
                head_affinity,
                backward_bias,
                word_target,
            },
        ))
    }

    /// Tab/Enter等真实目标来源只消费当前已发布格；另一publication的最新raw票须同Doc逐项核实。
    /// 这里只归还实际内容框和可见端点，不重签Row权限、不读正文，也不把离屏格补成Ready。
    pub(in crate::native_surface::paged_body) fn cell_source_hit(
        &self,
        source: &VerifiedMarkdownTableCellSource,
    ) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit>> {
        self.require()?;
        let (coordinate, range, kind) = {
            let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
            let document = owner.try_borrow().map_err(|_| Error::Busy)?;
            (
                source.coordinate_for(&document),
                source.canonical_range_for(&document).cloned(),
                source.source_for(&document),
            )
        };
        let coordinate = coordinate.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        let range = range.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        let kind = kind.map_err(|cause| scene_error(ViewportError::Table(cause)))?;
        for row in &self.grids {
            if row.row.coordinate() != (coordinate.0, coordinate.1) {
                continue;
            }
            for cell in &row.cells {
                let raw = match &cell.content {
                    GridCellContent::Empty(raw)
                    | GridCellContent::BeforeText(raw)
                    | GridCellContent::OutsideText(raw) => raw,
                    GridCellContent::Composed(window) => window.canonical_cell_source(),
                };
                let matches = {
                    // 比较来源事实时只借同一个真实Doc；空范围还要区分Present与隐式缺格，不能只认整数。
                    let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
                    let document = owner.try_borrow().map_err(|_| Error::Busy)?;
                    let coordinate = raw
                        .coordinate_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == coordinate;
                    let range = raw
                        .canonical_range_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == &range;
                    let kind = raw
                        .source_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        == kind;
                    coordinate && range && kind
                };
                if !matches {
                    continue;
                }
                let frame = cell.content_frame;
                let viewport = self.key.request.viewport;
                let visible = frame.origin.x.max(viewport.origin.x)
                    < (frame.origin.x + frame.size.width)
                        .min(viewport.origin.x + viewport.size.width)
                    && frame.origin.y.max(viewport.origin.y)
                        < (frame.origin.y + frame.size.height)
                            .min(viewport.origin.y + viewport.size.height);
                if !visible {
                    self.require()?;
                    return Ok(None);
                }
                let head_utf16 = match &cell.content {
                    GridCellContent::Composed(window) => {
                        match window.first_rect(
                            NSRange::new(range.start, 0),
                            Affinity::Downstream,
                            &|| self.current(),
                        ) {
                            Ok(value)
                                if value.actual_range.length == 0
                                    && range.start <= value.actual_range.location
                                    && value.actual_range.location <= range.end =>
                            {
                                value.actual_range.location
                            }
                            Ok(_) | Err(ViewportError::Pending) => {
                                self.require()?; // 真实资源首错仍先于离屏/尚未准备，不吞成等待。
                                return Ok(None);
                            }
                            Err(cause) => return Err(scene_error(cause)),
                        }
                    }
                    GridCellContent::Empty(_) if range.is_empty() => {
                        // 真空格没有显示字形，原raw空范围已证明唯一BOF；这里只交真实格框，未制造caret。
                        // 子Source仍须真实准备自己的字体行，正式输入与IME不能把本框当字符几何。
                        range.start
                    }
                    _ => {
                        self.require()?;
                        return Ok(None); // Before/Outside的端点尚不在内容窗口，原SourceTarget继续reveal。
                    }
                };
                self.require()?;
                return Ok(Some(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit {
                        row: row.row.clone(),
                        source: source.clone(),
                        content_frame: frame,
                        head_utf16,
                        head_affinity: Affinity::Downstream,
                        backward_bias: false,
                        word_target: None,
                    },
                )); // 非指针导航只借真实端点；没有选择词组的点击侧，不作word handoff。
            }
        }
        self.require()?;
        Ok(None)
    }

    /// Find/IME先确认真实行域，再消费实际格坐标；只有空caret可投影隐藏管道等语法边界。
    /// 非空语法没有显示字符，离屏/未准备格也不取邻格冒充；三态保证它们不会降到TableRow文字part。
    pub(super) fn grid_first_rect(
        &self,
        range: NSRange,
        affinity: Affinity,
    ) -> Result<GridCharacterGeometry> {
        // 两个调用边界已核本Scene健康；这里逐格核原raw票和实际窗口，不额外深扫整图。
        for row in &self.grids {
            let physical = row.row.source_range();
            let owns = if range.length == 0 && range.location != 0 && affinity.is_upstream() {
                physical.start < range.location && range.location <= physical.end
            } else {
                physical.contains(&range.location)
            };
            if !owns {
                continue; // 行尾上下游沿真实physical端点归属，不把下行起点重复认成上一表格行。
            }
            let mut previous: Option<(&GridCellPublication, Range<usize>, usize)> = None;
            let mut boundary = None;
            for cell in &row.cells {
                let raw = match &cell.content {
                    GridCellContent::Empty(raw)
                    | GridCellContent::BeforeText(raw)
                    | GridCellContent::OutsideText(raw) => raw,
                    GridCellContent::Composed(window) => window.canonical_cell_source(),
                };
                let (canonical, column) = {
                    // 同一结构票只在纯Core读取期借Doc；Native caret/current回调之前归还租约。
                    let owner = self.key.request.document.upgrade().ok_or(Error::Revoked)?;
                    let document = owner.try_borrow().map_err(|_| Error::Busy)?;
                    let canonical = raw
                        .canonical_range_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?
                        .clone();
                    let (_, _, column) = raw
                        .coordinate_for(&document)
                        .map_err(|cause| scene_error(ViewportError::Table(cause)))?;
                    (canonical, column)
                };
                // raw末端只给零范围caret，不把紧随其后的空白/管道非空请求截成一个假字符框。
                if canonical.contains(&range.location)
                    || range.length == 0 && range.location == canonical.end
                {
                    let end = range
                        .location
                        .checked_add(range.length)
                        .ok_or(Error::InvalidRange)?
                        .min(canonical.end);
                    return self.grid_cell_rect(
                        cell,
                        NSRange::new(range.location, end - range.location),
                        affinity,
                    );
                }
                if range.length != 0 {
                    continue; // 原管道/分隔线不是一个可见字符，不能用首格字符覆盖原非空请求。
                }
                if column == 0 && range.location < canonical.start {
                    // 只有原票证明的首列能接住本行前导语法；可见数组的第一个格可能实际是第十列。
                    boundary = Some((cell, canonical.start, Affinity::Downstream));
                    break;
                }
                if let Some((before, previous_range, previous_column)) = &previous
                    && previous_column + 1 == column
                    && previous_range.end < range.location
                    && range.location < canonical.start
                {
                    // 两个真实相邻列才证明中间隐藏管道的两侧；水平方向离屏缺列不能跳过后补票。
                    boundary = Some(if affinity.is_upstream() {
                        (*before, previous_range.end, Affinity::Upstream)
                    } else {
                        (cell, canonical.start, Affinity::Downstream)
                    });
                    break;
                }
                if column + 1 == row.row.columns() && range.location > canonical.end {
                    // 表头physical范围包含真实delimiter；其尾部语法沿本行真实末格的末caret投影。
                    boundary = Some((cell, canonical.end, Affinity::Upstream));
                    break;
                }
                previous = Some((cell, canonical, column));
            }
            let Some((cell, position, side)) = boundary else {
                return Ok(GridCharacterGeometry::Unavailable); // 同属此行但当前无真实可见格，不改写原请求。
            };
            return match self.grid_cell_rect(cell, NSRange::new(position, 0), side)? {
                GridCharacterGeometry::Ready(mut value) => {
                    // 这里只投影空间：canonical语义仍是原隐藏位置，不能把输入/Find范围移到格中文字。
                    value.actual_range = range;
                    Ok(GridCharacterGeometry::Ready(value))
                }
                unavailable => Ok(unavailable),
            };
        }
        Ok(GridCharacterGeometry::Outside)
    }

    /// 无文字的Before/Outside/Empty格没有可借用的字体行槽；真实窗口也可明确尚无此可见caret。
    /// 只把几何Pending归为Unavailable，Source/字体/原映射错误完整保留，不能吞成“不画光标”。
    fn grid_cell_rect(
        &self,
        cell: &GridCellPublication,
        range: NSRange,
        affinity: Affinity,
    ) -> Result<GridCharacterGeometry> {
        let GridCellContent::Composed(window) = &cell.content else {
            return Ok(GridCharacterGeometry::Unavailable);
        };
        match window.first_rect(range, affinity, &|| self.current()) {
            Ok(value) => Ok(GridCharacterGeometry::Ready(value)),
            Err(ViewportError::Pending) => Ok(GridCharacterGeometry::Unavailable),
            Err(cause) => Err(scene_error(cause)),
        }
    }

    /// 链接激活仍走原 mouseUp 与目标查询；此处只交真实可见标签的原文范围。
    pub(super) fn grid_link_hit(&self, local: NSPoint) -> Result<Option<(Range<usize>, usize)>> {
        let Some(cell) = self.grid_cell_at(local) else {
            return Ok(None);
        };
        let GridCellContent::Composed(window) = &cell.content else {
            return Ok(None);
        };
        let Some(range) = window
            .link_part_at(local, &|| self.current())
            .map_err(scene_error)?
        else {
            return Ok(None);
        };
        // 同一个真实LINK part内的目标一致，直接查询其已认证起点；不把末端caret当作链接外字符。
        let position = range.start;
        Ok(Some((range, position)))
    }

    /// 与原表格相同的底色和1pt边线；裁剪来自外层真实视口，不分配整行或整表 backing。
    pub(super) fn grid_decorations(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        self.validate_grids(current)?;
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        for row in &self.grids {
            self.color(
                context,
                if row.row.coordinate().1 == 0 {
                    NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND
                } else {
                    NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND
                },
            );
            CGContext::fill_rect(Some(context), row.frame);
            for cell in &row.cells {
                if let GridCellContent::Composed(window) = &cell.content {
                    window
                        .paint_code_backgrounds(context, current)
                        .map_err(paint_error)?;
                }
            }
            self.stroke_color(context, NATIVE_MARKDOWN_TABLE_BORDER);
            CGContext::set_line_width(Some(context), 1.0);
            CGContext::begin_path(Some(context));
            let top = row.frame.origin.y + row.frame.size.height;
            let left = row.frame.origin.x;
            let right = left + row.frame.size.width;
            CGContext::move_to_point(Some(context), left, top);
            CGContext::add_line_to_point(Some(context), right, top);
            // 未完成长行只有当前真实可见前缀，不能把前缀底部画成已封存整行的结尾。
            if row.complete {
                CGContext::move_to_point(Some(context), left, row.frame.origin.y);
                CGContext::add_line_to_point(Some(context), right, row.frame.origin.y);
            }
            for x in row
                .cells
                .iter()
                .map(|cell| cell.frame.origin.x)
                .chain(std::iter::once(right))
            {
                CGContext::move_to_point(Some(context), x, row.frame.origin.y);
                CGContext::add_line_to_point(Some(context), x, top);
            }
            CGContext::stroke_path(Some(context));
        }
        drop(restore);
        self.validate_grids(current)
    }

    /// 一个真实行背景只有一次固定矩形绘制，裁剪仍是同一外层视口。
    pub(super) fn paint_grid_row_background(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        // 固定行框没有正文I/O；量子入口/出口核全图，这里只守原取消票，不能每格重扫全部字体。
        if !current() {
            return Err(Error::Revoked);
        }
        CGContext::save_g_state(Some(context));
        let restore = Restore(context);
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        let row = &self.grids[row];
        self.color(
            context,
            if row.row.coordinate().1 == 0 {
                NATIVE_MARKDOWN_TABLE_HEADER_BACKGROUND
            } else {
                NATIVE_MARKDOWN_TABLE_ROW_BACKGROUND
            },
        );
        CGContext::fill_rect(Some(context), row.frame);
        drop(restore);
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// 一行路径只在代码底色完成后建立；跨量子仅追加真实边线，最终仍一次stroke以保接缝像素。
    pub(super) fn begin_grid_border(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let left = row.frame.origin.x;
        let right = left + row.frame.size.width;
        let top = row.frame.origin.y + row.frame.size.height;
        // SDK明确ClipToRect会清空current path，必须先裁剪再建立跨量子的原行路径。
        // 这份save由唯一Paint续体持有，完成或取消时都由该owner归还。
        CGContext::save_g_state(Some(context));
        CGContext::clip_to_rect(Some(context), self.key.request.viewport);
        CGContext::begin_path(Some(context));
        CGContext::move_to_point(Some(context), left, top);
        CGContext::add_line_to_point(Some(context), right, top);
        if row.complete {
            CGContext::move_to_point(Some(context), left, row.frame.origin.y);
            CGContext::add_line_to_point(Some(context), right, row.frame.origin.y);
        }
        Ok(()) // Paint在登记本栈图形状态归属后统一执行Source/current后核。
    }

    /// 每个可见格只追加一条原列边线，不按总列数在同一metadata项内循环。
    pub(super) fn append_grid_border(
        &self,
        row: usize,
        cell: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        // 路径只消费原封存行框的一个x坐标，无来源读取；真实Source失败仍由step出口提升。
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let x = row.cells[cell].frame.origin.x;
        CGContext::move_to_point(Some(context), x, row.frame.origin.y);
        CGContext::add_line_to_point(Some(context), x, row.frame.origin.y + row.frame.size.height);
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// 原整行同一路径最终只stroke一次，不能因分步拆线造成抗锯齿接缝重复叠色。
    pub(super) fn finish_grid_border(
        &self,
        row: usize,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        if !current() {
            return Err(Error::Revoked);
        }
        let row = &self.grids[row];
        let x = row.frame.origin.x + row.frame.size.width;
        CGContext::move_to_point(Some(context), x, row.frame.origin.y);
        CGContext::add_line_to_point(Some(context), x, row.frame.origin.y + row.frame.size.height);
        self.stroke_color(context, NATIVE_MARKDOWN_TABLE_BORDER);
        CGContext::set_line_width(Some(context), 1.0);
        CGContext::stroke_path(Some(context));
        if !current() {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// 跨格选区逐窗口映入其真实 display，不复制任何被选择的原文。
    pub(super) fn grid_selection(
        &self,
        context: &CGContext,
        current: &dyn Fn() -> bool,
    ) -> Result<()> {
        self.color(context, self.colors.selection);
        for row in &self.grids {
            let source = row.row.source_range();
            let first = self
                .selections
                .partition_point(|selection| selection.end <= source.start);
            for selection in &self.selections[first..] {
                if selection.start >= source.end {
                    break;
                }
                for cell in &row.cells {
                    if let GridCellContent::Composed(window) = &cell.content {
                        window
                            .selection_rects(selection.clone(), current, |frame| {
                                CGContext::fill_rect(Some(context), frame);
                                Ok(())
                            })
                            .map_err(scene_error)?;
                    }
                }
            }
        }
        self.validate_grids(current)
    }
}

/// 图形状态归还属于同步本栈，失败/取消均不能污染后继同CPU候选的状态。
struct Restore<'a>(&'a CGContext);
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        CGContext::restore_g_state(Some(self.0));
    }
}

/// 与原文字命中相同的半开框约定；边界共享时只由真实右/上邻格消费。
fn inside(point: NSPoint, frame: NSRect) -> bool {
    point.x >= frame.origin.x
        && point.x < frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y < frame.origin.y + frame.size.height
}
