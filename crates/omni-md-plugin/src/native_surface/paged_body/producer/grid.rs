//! 表格语义沿原请求流式认证，字形只续真实视口前缀，字体仍由正文唯一线程处理。
//!
//! 完成行离屏后归还格资源；回滚只从原封存行目录重建当前真实视口。

use super::super::scene::{GridCellContent, GridCellPublication, GridRowPublication};
mod target;
use super::*;
use crate::native_surface::{
    NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING as HORIZONTAL,
    NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING as VERTICAL,
    NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH as MAXIMUM_WIDTH,
    NATIVE_MARKDOWN_TABLE_ROW_HEIGHT as MINIMUM_HEIGHT,
    paged_line_viewport::NativeMarkdownComposedTableCell,
    table_geometry::{native_markdown_table_column_widths, native_markdown_table_content_width},
};
use objc2_foundation::{NSRect, NSSize};
use omni_markdown_core::markdown_document_buffer::SemanticWork;
use omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource;
use omni_markdown_core::{
    MarkdownLineTableRowRequest, MarkdownTableCellDisplayPreparation,
    MarkdownTableCellDisplayPreparationStep, MarkdownTableCellDisplaySource,
};
use std::ops::Range;
use target::{CellTarget, RowTarget};
pub(super) mod word;

/// 仅这一轮真实格工作；原Request已计CellInline，Display差量与Composer量子单列避免重复。
#[derive(Default)]
pub(super) struct Work {
    /// 原请求未计入的显示转换工作；CellInline 已在同一原请求总账中，不能再加一次。
    pub display: SemanticWork,
    /// Composer 实际读取，不等同原文件唯一字节数；同范围的 Unicode 子任务可分别收费。
    pub bytes: usize,
    /// 原 Unicode/整形游标实际推进量，用来显示真实工作而非依据文本长度估算。
    pub scalars: usize,
    /// 同目录的实际处理记录；它与源读取、字形数量有不同计量单位。
    pub metadata: usize,
    /// 最大值来自单次原 step，不能把多个串行回合的总量误当单回合预算。
    pub maximum_bytes: usize,
    pub maximum_scalars: usize,
    pub maximum_metadata: usize,
    /// 字体试次读取由外层原 FontTrial 单独累计，这里只包含原前沿任务自己的 Source 读取。
    pub frontier_source_bytes: usize,
    /// 实际交给整形器的输入量；原源扫描和复制不能冒充已经排完的字符。
    pub shaped_input_bytes: usize,
}

/// 同一物理表的列宽在原宽度 epoch 内固定，后续行不因滚动重新分配列宽。
pub(super) struct TableWidths {
    pub(super) header: usize,
    pub(super) widths: Rc<[f64]>,
}

/// 当前格的真实工作请求仍交 Producer 唯一 FontTrial/FontWorker，不为格新增线程或 Body。
pub(super) enum Advance {
    Progress,
    Style {
        column: usize,
        request: MarkdownLineStyleRequest,
    },
    Control {
        column: usize,
        request: omni_markdown_core::MarkdownLineInlineControl,
    },
    Visible,
    Complete,
    Target {
        request: Rc<super::super::target::SourceTargetRequest>,
        rect: NSRect,
    },
}

/// 只有最终格高全部完成才给原 Composer 签 TableRow；可见前缀不冒充这一完成条件。
pub(super) struct Grid {
    /// 原 Core 请求 nonce 与结构票同寿；最终几何只能归还这一尚未响应的请求。
    request: Option<MarkdownLineTableRowRequest>,
    /// 恢复行也必须持当前同源的真实结构票；它没有待提交的 Core 请求。
    row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    /// 只有原输出真正封存的行才有最终高，格前缀的 EOF 状态仍独立保留。
    sealed_height: Option<f64>,
    /// y-down 文档坐标，来源是前面真实 LineEnd 的累计高度，不是字符数或预估行数。
    pub(super) top: f64,
    /// 原宽度 epoch 的最终列框宽；统一为26.6后才给 Composer 和 Scene 两侧使用。
    widths: Rc<[f64]>,
    /// 只为首次表头测宽；一个格达到真实 cap 就结束该格，不预排巨格全部字形。
    natural: Option<NaturalWidths>,
    /// 列数来自原结构票；这些格只随当前可见行或唯一未提交行存活。
    cells: Vec<Cell>,
    /// 在同一行的实际列之间轮转，避免第一列巨段落阻止后面可见列获得准备机会。
    current: usize,
    /// 仅表示最终几何已归还；Core 后续仍须完成自己的结构/GCB/目录步骤才能产 LineReady。
    pub(super) submitted: bool,
    /// Find 保原弱业务请求与有限格游标，不持第二份输入、文档或 Source。
    target: Option<RowTarget>,
    /// 每次 advance 后交给外层，临时自然宽作业退场也不会丢已发生的读取工作。
    work: Work,
}

/// 自然宽测量串行复用一个格，达到原 cap 后无需继续排完整个巨格来证明更大的宽度。
struct NaturalWidths {
    values: Vec<f64>,
    active: Cell,
}

/// 真空格不建 Composer；非空格只持当前可见行所需的真实续体和原输出。
struct Cell {
    /// 作业位置不授予来源权限；真正范围仍来自下方的原结构票和显示源。
    column: usize,
    /// 先借原请求取得真正 CellInline，再逐4KiB转换；Pending 没有可读显示 Source。
    preparation: Option<MarkdownTableCellDisplayPreparation>,
    /// 只读显示目录及其 canonical 映射，不是可写草稿，也不进入正文 Input 域。
    display: Option<Rc<MarkdownTableCellDisplaySource>>,
    /// 只有 raw 范围与完成后的显示都为空才进入此分支，未知非空格不能冒充空格。
    empty: Option<VerifiedMarkdownTableCellSource>,
    /// 与正文共享 FontCatalog，保自己实际显示 Source 的行目录，不创建单格 Body 或线程。
    composer: Option<MarkdownLineComposer>,
    /// 生命周期结束先撤原 Core 子任务，防止迟到工作借旧 Source 继续产新字形。
    cancellation: MarkdownLineComposeCancellation,
    /// 原 prefix Reader 与 Scene 共享同一实例，复制 Rc 不另开目录 FD。
    output: Option<Rc<RefCell<MarkdownComposedLines>>>,
    /// 一条已封存行也可能有很多 part；未完成的真实字体度量必须在原位置续完。
    pending: Option<LineMetrics>,
    /// 只累已经完整度量的行，未消费的 pending 行不提前参与行高。
    height: f64,
    lines: usize,
    /// 真实 Composer EOF 或双空证明才完成，视口覆盖不能设置此位。
    complete: bool,
    width: f64,
    natural_line: f64,
    natural_maximum: f64,
    scan: Option<CellScan>,
    target: Option<CellTarget>,
}

/// 行目录仍只在原 prefix Reader 中；窗口准备只保有限弱句柄和下一条真实度量游标。
struct CellScan {
    /// 每个几何请求独立准备窗口；旧宽度/滚动票不能直接给后继 Scene 签 Ready。
    request: NativeMarkdownPagedSceneRequest,
    /// 句柄只弱指向原 Reader，不把离屏字形或整格文字搬进 Native 数组。
    cursor: Option<MarkdownComposedLine>,
    metrics: Option<LineMetrics>,
    bottom: f64,
    first_top: f64,
    handles: VecDeque<NativeMarkdownMeasuredLine>,
    ready: bool,
}

impl Grid {
    /// 原块起点和原 NeedStyle nonce 已由 Core 接纳；这里不从裸范围制造行或输入图。
    pub(super) fn new(
        request: MarkdownLineTableRowRequest,
        top: f64,
        widths: &Option<TableWidths>,
    ) -> Self {
        let columns = request.row().columns();
        let header = request.row().coordinate().0;
        let retained = widths.as_ref().filter(|widths| widths.header == header);
        // 同表后续行复用已解算列宽，避免每向下滚一行就重新分配列位置。
        // 缓存属于原 Producer/宽度 epoch，不能在宽度换代或另一张表之间借用。
        let (widths, natural) = match retained {
            Some(retained) => (retained.widths.clone(), None),
            None => {
                // 原顺序 flow 先经过真实表头；违反这一点是装配错误，不能以默认列宽掩盖。
                // 旧行恢复需要从真实目录读已封存列宽，不在这里静默重测某条正文行。
                assert_eq!(
                    request.row().coordinate().1,
                    0,
                    "顺序 flow 先经过原真实表头"
                );
                (
                    Rc::from([]),
                    Some(NaturalWidths {
                        values: Vec::with_capacity(columns),
                        active: Cell::new(0, MAXIMUM_WIDTH),
                    }),
                )
            }
        };
        let cells = if natural.is_none() {
            // 这里只分配有限列元数据，实际 Source/Composer 在该列被推进时才创建。
            // 不因原文包含大量格而一次装配整表字形或原生输入对象。
            widths
                .iter()
                .enumerate()
                .map(|(column, width)| Cell::new(column, *width))
                .collect()
        } else {
            Vec::new()
        };
        let row = request.row().clone();
        Self {
            request: Some(request),
            row,
            sealed_height: None,
            top,
            widths,
            natural,
            cells,
            current: 0,
            submitted: false,
            target: None,
            work: Work::default(),
        }
    }

    /// 只接原 Reader 认证的最终高度和列宽；恢复不会伪造未响应的 TableRowRequest。
    pub(super) fn restored(
        row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        top: f64,
        height: f64,
        widths: Rc<[f64]>,
    ) -> Self {
        let cells = widths
            .iter()
            .enumerate()
            .map(|(column, width)| Cell::new(column, *width))
            .collect();
        Self {
            request: None,
            row,
            sealed_height: Some(height),
            top,
            widths,
            natural: None,
            cells,
            current: 0,
            submitted: true, // 原行已真实封存，当前对象没有新的 Core 提交责任。
            target: None,
            work: Work::default(),
        }
    }

    /// Native 重入前后仍由外层原票约束；纯格作业另核同一结构行与 canonical Source。
    fn require(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> Result<()> {
        // Doc 只在纯 Core 核来源时短借；返回前归还，后续字体/窗口回调才能安全重入。
        // 同数值范围的新 publication 无权为旧结构行续票。
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("表格持原Body文档")
            .clone();
        self.row.require_for(&document.borrow(), source)?;
        Ok(())
    }

    /// 当前唯一 FontTrial 指向真实在途格，不从列号重新捕获 Source 或换成另一坐标域。
    pub(super) fn display(&self, column: usize) -> Rc<MarkdownTableCellDisplaySource> {
        self.cell(column)
            .display
            .as_ref()
            .expect("格Font请求只在真实显示源完成后产生")
            .clone()
    }
    pub(super) fn source(&self, column: usize) -> &MarkdownLayoutTextSource {
        self.cell(column)
            .display
            .as_ref()
            .expect("Cell请求持真实显示源")
            .source()
    }

    /// 已发生的读量在退休Cell前移交原Producer，不能随临时自然宽作业被丢掉。
    pub(super) fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }

    /// 同一请求的响应回到原 Cell Composer；不是把局部 range 注入外层 Body Composer。
    pub(super) fn composer(&mut self, column: usize) -> &mut MarkdownLineComposer {
        self.cell_mut(column)
            .composer
            .as_mut()
            .expect("活Font/control请求持原格Composer")
    }

    fn cell(&self, column: usize) -> &Cell {
        match &self.natural {
            Some(natural) => {
                // 测宽只有一个活格；异步字体响应必须回这一格，不能落到后继列。
                assert_eq!(natural.active.column, column);
                &natural.active
            }
            None => &self.cells[column],
        }
    }
    fn cell_mut(&mut self, column: usize) -> &mut Cell {
        match &mut self.natural {
            Some(natural) => {
                // 索引错位属于原作业装配错误，不能返回默认 Cell 或重新捕一份显示源。
                assert_eq!(natural.active.column, column);
                &mut natural.active
            }
            None => &mut self.cells[column],
        }
    }

    /// 行高只有真实已结束的 Cell LineEnd 参与；未排后缀和未见格不会签最终 max。
    pub(super) fn height(&self) -> f64 {
        if let Some(height) = self.sealed_height {
            return height; // 原封存几何不随恢复格的可见前缀高度缩短，后续正文位置保持不变。
        }
        // 所有格共享原最小38pt行框；更高内容只有实际行度量结束才抬高当前前缀。
        // 这里的 max 在 complete 前仍是下界，禁止据此排列后续正文。
        self.cells.iter().fold(MINIMUM_HEIGHT, |height, cell| {
            height.max((cell.height + VERTICAL * 2.0).ceil())
        })
    }

    pub(super) fn complete(&self) -> bool {
        // 测宽完成只固定列位置，不证明实际最终宽度下的字形布局已经结束。
        // 最终高需要每个真实列都结束，不能只检查当前可见的短格。
        self.natural.is_none() && self.cells.iter().all(|cell| cell.complete)
    }

    /// 列宽只在真实自然宽solver完成后固定到26.6；零代表测宽尚未完成，不授额外水平范围。
    /// 原恢复行沿封存目录取得同一widths，滚动不会重新按列数或平均字符宽估算。
    pub(super) fn width(&self) -> f64 {
        self.widths.iter().sum()
    }

    /// 只判断当前可见列是否缺真实前缀；封存行高不能冒充尚未恢复的文字已经可读。
    pub(super) fn needs_visible_work(
        &self,
        scene: &NativeMarkdownPagedSceneRequest,
        padding: f64,
    ) -> bool {
        let needed = (scene.virtual_viewport.document_origin.y + scene.viewport.size.height
            - self.top
            - VERTICAL)
            .max(0.0);
        self.cells.iter().enumerate().any(|(column, cell)| {
            self.column_visible(column, scene, padding)
                && !cell.complete
                && (cell.pending.is_some() || cell.output.is_none() || cell.height < needed)
        })
    }

    /// 在途语义准备必须归还原请求单格槽以后才可退休；不以取消布尔替代实际完成。
    pub(super) fn has_preparation(&self) -> bool {
        self.natural
            .as_ref()
            .is_some_and(|natural| natural.active.preparation.is_some())
            || self.cells.iter().any(|cell| cell.preparation.is_some())
    }

    /// 已定位过的原 Find 请求不重复接管；新请求仍须走该格的真实首标量行几何。
    pub(super) fn needs_target(
        &self,
        target: &Rc<super::super::target::SourceTargetRequest>,
    ) -> bool {
        self.source_range().contains(&target.range.location)
            && !self
                .target
                .as_ref()
                .is_some_and(|old| Rc::ptr_eq(&old.request, target))
    }

    /// 收据只观察当前实际资源，不保活来源、文件或任务，也不记录已经退休的历史格。
    pub(super) fn resource_receipts(
        &self,
    ) -> Result<Vec<super::super::NativeMarkdownPagedCellResourceReceipts>> {
        let cells = self
            .cells
            .iter()
            .chain(self.natural.iter().map(|natural| &natural.active));
        cells.map(Cell::resource_receipt).collect()
    }

    /// 原布局字段明确区分全行完成和已测前缀；巨格达到可见底边就暂停整形。
    pub(super) fn advance(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        scene: &NativeMarkdownPagedSceneRequest,
        word_target: Option<
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget,
        >,
        widths: &mut Option<TableWidths>,
        semantics: &mut super::super::semantics::RootLeaf,
    ) -> Result<Advance> {
        self.require(owner, source)?;
        if let Some(natural) = self.natural.as_mut() {
            // 原显示认证也按同一量子推进。该分支不在同回合再推进正式列宽下的 Composer。
            // 达到 cap 的证明来自真实字形 advance，不来自字符个数或字体名称。
            let result = natural.active.advance(
                owner,
                source,
                &self.row,
                true,
                semantics,
                &mut self.work,
            )?;
            if natural.active.complete || natural.active.natural_maximum >= MAXIMUM_WIDTH {
                // 栏宽包含左右padding，边线在框内绘制，不额外把边宽加入目录sum。
                let width = natural.active.natural_maximum.min(MAXIMUM_WIDTH) + HORIZONTAL * 2.0;
                natural.values.push(width);
                // 这个作业只证明被 cap 后的自然宽，取消剩余字形工作不改变原文或原语义。
                natural.active.release_layout();
                if natural.values.len() == self.row.columns() {
                    // 窄窗口保有可用的真实列内容宽，超出部分属于表整体水平范围。
                    // 不能把很多列强夹到padding以内，然后以零宽格丢失内容。
                    let body_width =
                        super::width(scene, owner.configuration.padding)? as f64 / 64.0;
                    let available = body_width.max(natural.values.len() as f64 * 42.0);
                    let mut resolved =
                        native_markdown_table_column_widths(&natural.values, available);
                    // 真实列框先统一到原目录26.6单位，封存sum与后续绘图完全一致。
                    for width in &mut resolved {
                        *width = (*width * 64.0).round() / 64.0;
                    }
                    self.widths = resolved.into();
                    *widths = Some(TableWidths {
                        header: self.row.coordinate().0,
                        widths: self.widths.clone(),
                    });
                    self.cells = self
                        .widths
                        .iter()
                        .enumerate()
                        .map(|(column, width)| Cell::new(column, *width))
                        .collect();
                    self.natural = None;
                } else {
                    // 前一格的 Source/目录已经真实退休后才创建下一格，测宽不并存整行输出。
                    natural.active = Cell::new(natural.values.len(), MAXIMUM_WIDTH);
                }
                return Ok(Advance::Progress);
            }
            return Ok(result);
        }
        if let Some(column) = self
            .cells
            .iter()
            .position(|cell| cell.preparation.is_some())
        {
            // 所有格共享原 Request 唯一 CellInline 槽；一列尚在语义/显示准备时，
            // 不能轮转到另一列启动第二份准备，否则真实原发布会被判为 PublicationSuperseded。
            // 先把这一列的原准备交付为显示源，再恢复下方字形轮转；不创建第二 Parser 或 owner。
            return self.cells[column].advance(
                owner,
                source,
                &self.row,
                false,
                semantics,
                &mut self.work,
            );
        }
        if self.natural.is_none() && self.cells.iter().all(|cell| cell.complete) {
            // 原 Find 可能刚请求已完成格；仍先给该原目标找真实文字行，不能只返回整row高框。
            if let Some(step) = self.advance_source_target(owner, source, scene, semantics)? {
                return Ok(step);
            }
            return Ok(Advance::Complete);
        }
        if let Some(step) = self.advance_source_target(owner, source, scene, semantics)? {
            // 远端目标可超过当前viewport。只排到它真实首标量所在行，不排完匹配尾部。
            return Ok(step);
        }
        // 已实测的模式字形可能位于超高格中部，原屏内锚下方还缺真实可见前缀。
        // 在途 Grid 先于普通行目标执行，因此必须在这里续同一格，不能反复发布旧 clip 的短前缀。
        let mode_bottom = owner.source_target.borrow().as_ref().and_then(|target| {
            if !target.pending()
                || target.source != source.authority()
                || !target.matches(owner, scene)
                || !owner.mode_retains_measured_glyph(target)
            {
                return None; // 普通 Find 或已失效模式不扩大本次实际可见前缀。
            }
            owner.mode_viewport_required_bottom(target, self.top, scene.viewport.size.height)
        });
        let required_bottom = (scene.virtual_viewport.document_origin.y
            + scene.viewport.size.height)
            .max(mode_bottom.unwrap_or(0.0));
        let needed = (required_bottom - self.top - VERTICAL).max(0.0);
        // 先为所有真正可见列准备同一垂直视口；离屏列最多再保一个实际作业。
        for _ in 0..self.cells.len() {
            let column = self.current;
            self.current = (self.current + 1) % self.cells.len();
            if self.cells[column].complete
                || self.cells[column].pending.is_none()
                    && self.cells[column].output.is_some()
                    && self.cells[column].height >= needed
            {
                // 完成的短格只需画其剩余背景；达到视口底部的长格保原续体等下一次滚动。
                continue;
            }
            if !self.column_visible(column, scene, owner.configuration.padding) {
                // 当前横向裁剪外没有可见墨迹请求，不为它创建原生窗口或像素缓冲。
                continue;
            }
            return self.cells[column].advance(
                owner,
                source,
                &self.row,
                false,
                semantics,
                &mut self.work,
            );
        }
        let target_follows = word_target
            .is_some_and(|target| target.row_start >= self.source_range().end)
            || owner.source_target.borrow().as_ref().is_some_and(|target| {
                target.pending()
                    && target.source == source.authority()
                    && target.matches(owner, scene)
                    && target.range.location >= self.source_range().end
            });
        // 原目标尚在后续结构行时，旧视口已满并不表示这次导航已就绪。
        // 当前行必须先逐量子取得真实最终高度并交回唯一正文 Composer，才能继续下一行；
        // 提前返回 Visible 会重复发布旧首屏，并拿它为远端 Find 签首字符可见证明。
        // 已封存恢复行无需再次提交，原目标在后方也仍可交还这条有限恢复作业。
        if self.sealed_height.is_some()
            || !target_follows && self.height() + self.top >= required_bottom
        {
            // 这一帧只签实际已测前缀。尚未结束的行保持Core暂停，不能生成假的LineReady。
            return Ok(Advance::Visible);
        }
        // 可见列都已结束却尚不能确定行尾时，串行测下一真实离屏列，禁止凭空猜 row max。
        for column in 0..self.cells.len() {
            if !self.cells[column].complete {
                // 需要知道真实行尾时继续原列作业；该列的实际内容结束前不推进后续正文。
                return self.cells[column].advance(
                    owner,
                    source,
                    &self.row,
                    false,
                    semantics,
                    &mut self.work,
                );
            }
        }
        Ok(Advance::Complete)
    }

    /// 横向范围来自真实列宽之和；裁剪不把其他列的空内容冒充当前列。
    fn column_visible(
        &self,
        column: usize,
        scene: &NativeMarkdownPagedSceneRequest,
        padding: f64,
    ) -> bool {
        let left = scene.virtual_viewport.layout_origin_x
            + padding
            + self.widths[..column].iter().sum::<f64>()
            - scene.virtual_viewport.document_origin.x;
        left < scene.viewport.origin.x + scene.viewport.size.width
            && left + self.widths[column] > scene.viewport.origin.x
    }

    /// 外层 None 表示没有真实格显示覆盖；Some(None) 是原 Parser 已证明该位置没有链接。
    /// 两者必须区分，未知格不能吞掉首次点击，已覆盖格也不应被送回普通 RootLeaf 重解释。
    pub(super) fn link_at(
        &self,
        owner: &Owner,
        index: usize,
    ) -> std::result::Result<
        Option<Option<omni_markdown_core::MarkdownSourceLink>>,
        omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError,
    > {
        let document = owner
            .document
            .borrow()
            .as_ref()
            .expect("格链接持原Body文档")
            .clone();
        let document = document.borrow();
        for cell in &self.cells {
            if let Some(display) = &cell.display {
                // raw 票仍须与活文档匹配；编辑后的旧格不能仅凭数值相交继续打开链接。
                let range = display
                    .canonical_raw_cell()
                    .canonical_range_for(&document)?;
                if range.contains(&index) {
                    // 目标保原 Parser 的解码、引用定义和 label 语义；不从画面 URL 猜链接。
                    return display
                        .link_at_for(&document, index)
                        .map(|link| Some(link.cloned()));
                }
            }
        }
        Ok(None)
    }

    /// 返回原表格物理行范围，包含真正 EOL；不得以可见格文本长度替代表格来源坐标。
    pub(super) fn source_range(&self) -> Range<usize> {
        self.row.source_range()
    }

    /// 可见性只认真实高度前缀；未知行尾不能借估计高度提前排出后继正文。
    pub(super) fn visible(&self, scene: &NativeMarkdownPagedSceneRequest) -> bool {
        self.top < scene.virtual_viewport.document_origin.y + scene.viewport.size.height
            && self.top + self.height() > scene.virtual_viewport.document_origin.y
    }

    /// 多行共用调用者这一轮的元数据余量；已经扫描好的窗口不会重读并饿死后面的行。
    pub(super) fn scan_windows(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        budget: &mut MarkdownLineComposeBudget,
        work: &mut Work,
    ) -> Result<bool> {
        let before_text = request.virtual_viewport.document_origin.y + request.viewport.size.height
            <= self.top + VERTICAL;
        for column in 0..self.cells.len() {
            if !self.column_visible(column, request, owner.configuration.padding) {
                // 横向离屏列不构造 Native 窗口；仍保真实列位置供后继水平视口使用。
                continue;
            }
            let cell = &mut self.cells[column];
            if cell.empty.is_some() {
                // 真空格没有字形目录，背景和 canonical 点击票已足够，不创建空 Composer。
                continue;
            }
            if before_text && cell.display.is_some() {
                // 当前窗口只含行顶内边距，真实文字尚在视口之外；保原格票供点击，
                // 不因没有文字交集而要求该格完成，也不构造裁剪为空的 Native 文字窗口。
                cell.scan = None;
                continue;
            }
            if cell.output.is_none() {
                // 非空格尚无真实 LineEnd 时整帧保持 Pending，不能冒充 Empty 或短格空白。
                return Ok(false);
            }
            if !cell.scan_window(owner, request, self.top, budget, work)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 所有目录窗口先真实完成，再构造Native格；前格不能反复耗尽后格的预算。
    pub(super) fn prepare(
        &mut self,
        owner: &Owner,
        key: &NativeMarkdownPagedSceneKey,
        current: &dyn Fn() -> bool,
    ) -> Result<GridRowPublication> {
        let row_height = self.height();
        let request = &key.request;
        let before_text = request.virtual_viewport.document_origin.y + request.viewport.size.height
            <= self.top + VERTICAL;
        // Source 采用向下增长的文档 y；Native frame 在这一视口内转换为自底向上的坐标。
        let row_bottom = request.viewport.size.height + request.virtual_viewport.document_origin.y
            - self.top
            - row_height;
        let row_left = request.virtual_viewport.layout_origin_x + owner.configuration.padding
            - request.virtual_viewport.document_origin.x;
        let frame = NSRect::new(
            NSPoint::new(row_left, row_bottom),
            NSSize::new(self.widths.iter().sum(), row_height),
        );
        let palette = owner.fonts.borrow().clone();
        let mut cells = Vec::new();
        let mut x = row_left;
        for (column, width) in self.widths.iter().copied().enumerate() {
            // 每格完整 frame 共用真实行高，文字框另扣原 padding；列间边界不会累积猜测误差。
            let cell_frame =
                NSRect::new(NSPoint::new(x, row_bottom), NSSize::new(width, row_height));
            // 原实测行框扣固定内边距，后继文字窗口再扣已经分页越过的首行高度。
            // 原行尚未完成时它只代表当前真实前缀，不授整格高度或 EOF。
            let content_frame = NSRect::new(
                NSPoint::new(
                    cell_frame.origin.x + HORIZONTAL,
                    cell_frame.origin.y + VERTICAL,
                ),
                NSSize::new(width - HORIZONTAL * 2.0, row_height - VERTICAL * 2.0),
            );
            x += width;
            if !self.column_visible(column, request, owner.configuration.padding) {
                continue;
            }
            let cell = &mut self.cells[column];
            let content = if let Some(empty) = &cell.empty {
                // 只有原 raw/display 双空的真实分支使用 Empty，不持格级字体或匿名映射 FD。
                GridCellContent::Empty(empty.clone())
            } else if before_text {
                // 只发布原行顶 padding 的来源语义；真实 raw.start 可供原格点击，
                // 文字框/链接仍须等视口进入内容区后从原显示映射与实际行目录取得。
                let source = cell
                    .display
                    .as_ref()
                    .expect("行顶内边距发布前已取得真实格显示票")
                    .canonical_raw_cell()
                    .clone();
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_cell_before_text column={column} row_top={} text_top={} viewport_bottom={} actual_cell_complete={}",
                    self.top,
                    self.top + VERTICAL,
                    request.virtual_viewport.document_origin.y + request.viewport.size.height,
                    cell.complete,
                );
                GridCellContent::BeforeText(source)
            } else {
                let scan = cell.scan.take().expect("所有可见格已完成真实窗口扫描");
                if scan.handles.is_empty() {
                    // 只有已完整测过的短格能证明当前区域在正文下方，仍保原 raw 票用于点击。
                    assert!(cell.complete, "只有真实完整短格可证明当前视口无文字");
                    let source = cell
                        .display
                        .as_ref()
                        .expect("已完成格保原显示来源")
                        .canonical_raw_cell()
                        .clone();
                    cells.push(GridCellPublication {
                        frame: cell_frame,
                        content_frame,
                        content: GridCellContent::OutsideText(source),
                    });
                    continue;
                }
                // 首行 top 来自同一输出目录扫描，不能把可见后缀错误画在格的第一行位置。
                let window_frame = NSRect::new(
                    content_frame.origin,
                    NSSize::new(
                        content_frame.size.width,
                        content_frame.size.height - scan.first_top,
                    ),
                );
                // 窗口只借这次 Source/output/palette；同一格不再建立第二 Body 或输入所有权图。
                let window = NativeMarkdownComposedTableCell::prepare(
                    cell.output.as_ref().expect("可见格持原prefix").clone(),
                    key.clone(),
                    cell.display.as_ref().expect("原显示源仍存活").clone(),
                    palette.clone(),
                    scan.handles,
                    window_frame,
                    current,
                )
                .map_err(scene_error)?;
                GridCellContent::Composed(window)
            };
            cells.push(GridCellPublication {
                frame: cell_frame,
                content_frame,
                content,
            });
        }
        Ok(GridRowPublication {
            row: self.row.clone(),
            frame,
            complete: self.sealed_height.is_some() || self.complete(),
            cells: cells.into_boxed_slice(),
        })
    }

    /// 最终提交使用原注册 Font 作为目录身份，真实表格高度不从这个 Font 的普通行槽推测。
    pub(super) fn response(
        &self,
        owner: &Owner,
    ) -> Result<omni_markdown_core::MarkdownLineTableRowResponse> {
        let fonts = owner.fonts.borrow();
        // 列宽已在 solver 后量化；目录同时封存每列宽和总宽，重建不得再次运行不同求解。
        let widths = self
            .widths
            .iter()
            .map(|width| (width * 64.0).round() as i64)
            .collect::<Vec<_>>();
        Ok(self
            .request
            .as_ref()
            .expect("只有在途原行可归还最终几何")
            .clone()
            .respond_registered(
                fonts[0].registered(),
                (self.height() * 64.0).round() as i64,
                widths.iter().sum(),
                &widths,
            )?)
    }
}

impl Cell {
    /// 可见格和唯一选词恢复格观察同一真实资源；收据不持源、FD或历史目录强引用。
    fn resource_receipt(&self) -> Result<super::super::NativeMarkdownPagedCellResourceReceipts> {
        Ok(super::super::NativeMarkdownPagedCellResourceReceipts {
            display: self
                .display
                .as_ref()
                .map(|display| display.resource_observer())
                .or_else(|| {
                    self.preparation
                        .as_ref()
                        .and_then(MarkdownTableCellDisplayPreparation::resource_observer)
                }),
            writer: self
                .composer
                .as_ref()
                .map(MarkdownLineComposer::resource_observer)
                .transpose()?,
            reader: self
                .output
                .as_ref()
                .map(|output| output.borrow().resource_observer()),
            worker: self
                .composer
                .as_ref()
                .map(MarkdownLineComposer::worker_resource_observer),
        })
    }

    /// 空字段只代表真实作业尚未开始，不提供默认依赖、默认文本或另一个初始化图。
    fn new(column: usize, width: f64) -> Self {
        Self {
            column,
            preparation: None,
            display: None,
            empty: None,
            composer: None,
            cancellation: MarkdownLineComposeCancellation::new(),
            output: None,
            pending: None,
            height: 0.0,
            lines: 0,
            complete: false,
            width,
            natural_line: 0.0,
            natural_maximum: 0.0,
            scan: None,
            target: None,
        }
    }

    /// 一个回合只做原 4KiB 语义/转换、原 Composer 量子或有限原生 part 度量之一。
    fn advance(
        &mut self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
        row: &omni_markdown_core::VerifiedMarkdownLayoutTableRow,
        natural: bool,
        semantics: &mut super::super::semantics::RootLeaf,
        work: &mut Work,
    ) -> Result<Advance> {
        if self.display.is_none() && self.empty.is_none() {
            let document = owner
                .document
                .borrow()
                .as_ref()
                .expect("格准备持原Body文档")
                .clone();
            let document = document.borrow();
            if self.preparation.is_none() {
                // 这里仅取得同 sealed row 的准备票；真正语义扫描仍由原 Request 配额控制。
                self.preparation = semantics.prepare_cell(&document, source, row, self.column)?;
                return Ok(Advance::Progress);
            }
            let preparation = self.preparation.as_mut().expect("原格准备已安装");
            let before = semantics.current_work();
            let step = semantics.advance_cell(preparation, &document)?;
            // 返回量包含 CellInline；扣掉原 Request 同轮已累计量后，才把显示转换计入外层。
            let measured = match &step {
                MarkdownTableCellDisplayPreparationStep::Progress { work }
                | MarkdownTableCellDisplayPreparationStep::Ready { work } => *work,
            };
            work.display_step(measured, before, semantics.current_work())?;
            match step {
                MarkdownTableCellDisplayPreparationStep::Progress { .. } => {
                    // 转换未结束时没有 display authority，不授默认样式，也不启动 Composer。
                    return Ok(Advance::Progress);
                }
                MarkdownTableCellDisplayPreparationStep::Ready { .. } => {
                    // 只有原 ready 工厂能取显示源；转换源仍以映射/分页目录持有，非整格 String。
                    let display = preparation
                        .take_ready_for(&document)?
                        .expect("Ready持原显示源");
                    if display.source().utf16_len() == 0
                        && display
                            .canonical_raw_cell()
                            .canonical_range_for(&document)?
                            .is_empty()
                    {
                        // 隐藏语法导致的 display 空不等于 raw 空，不能误删非空格的编辑坐标。
                        self.empty = Some(display.canonical_raw_cell().clone());
                        self.complete = true;
                    } else {
                        self.display = Some(Rc::new(display));
                    }
                    self.preparation = None;
                    return Ok(Advance::Progress);
                }
            }
        }
        if self.complete {
            return Ok(Advance::Complete);
        }
        if self.composer.is_none() {
            let display = self.display.as_ref().expect("非空格持原显示源");
            let content = if natural {
                // 首轮自然宽测量沿实际 advance，达到原 cap 即可停止，不为巨格 shape 到 EOF。
                self.width
            } else {
                native_markdown_table_content_width(self.width).ok_or(ViewError::InvalidGeometry)?
            };
            // 所有列共用 Body catalog，当前格只持自己取消句柄；没有逐格 FontWorker 或 Input。
            self.composer = Some(MarkdownLineComposer::new_with_frontier(
                display.source().clone(),
                &display.authority(),
                owner.catalog.clone(),
                (content * 64.0).round() as i64,
                self.cancellation.clone(),
            )?);
            return Ok(Advance::Progress);
        }
        if let Some(mut metrics) = self.pending.take() {
            let mut budget = LINE_METRIC_BUDGET;
            let output = self.output.as_ref().expect("待度量行持原prefix Reader");
            if !metrics.advance(output, owner, &mut budget, work)? {
                // 实际 part 尚未度量完整，保留续体；半行高度绝不能进入真实行前缀。
                self.pending = Some(metrics);
                return Ok(Advance::Progress);
            }
            // 只有完整度量可增长前缀，旧窗口扫描必须废弃，避免复用过期 bottom 和 Reader。
            self.height += metrics.height()?;
            self.lines += 1;
            self.scan = None;
            return Ok(Advance::Progress);
        }
        let composer = self.composer.as_mut().expect("活格持原Composer");
        let before = composer.frontier_work()?;
        // 每轮固定读取/标量/目录预算；这里不按原始格长放大配额或先读取整段。
        let step = composer.step(MarkdownLineComposeBudget {
            bytes: 4096,
            scalars: 256,
            records: 256,
            metadata_units: 256,
        })?;
        work.composer_step(&step, before, composer.frontier_work()?)?;
        match step.answer {
            MarkdownLineComposeAnswer::Yielded => Ok(Advance::Progress),
            MarkdownLineComposeAnswer::NeedStyle(request) => Ok(Advance::Style {
                column: self.column,
                request,
            }),
            MarkdownLineComposeAnswer::NeedsControl(request) => Ok(Advance::Control {
                column: self.column,
                request,
            }),
            MarkdownLineComposeAnswer::LineReady(facts) => {
                if let Some(target) = self.target.as_mut() {
                    target.reset_scan();
                }
                if natural {
                    // soft wrap 的 advance 属于同一物理行；只有真实 mandatory 才重置自然行宽。
                    self.natural_line += facts.x_advance_26_6 as f64 / 64.0;
                    self.natural_maximum = self.natural_maximum.max(self.natural_line);
                    if facts.kind == omni_markdown_core::MarkdownComposedLineKind::Mandatory {
                        self.natural_line = 0.0;
                    }
                } else {
                    // 新 prefix 替换旧 Reader；Scene 如仍借旧窗口，由自己的真实租约决定释放时机。
                    drop(self.output.take());
                    let output = Rc::new(RefCell::new(
                        self.composer
                            .as_mut()
                            .expect("LineReady仍属原Composer")
                            .publish_ready_prefix()?
                            .expect("完整LineEnd可发布真实prefix"),
                    ));
                    let handle = output.borrow_mut().last_line()?.expect("LineReady持原末行");
                    if handle.facts() != &facts || facts.ordinal != self.lines {
                        // 事实与序号不一致会造成错误行高和文字重叠，必须向上传递真实失败。
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        eprintln!(
                            "native_cell_unsupported phase=line_identity column={} expected_line={} actual_line={} original_facts_match={}",
                            self.column,
                            self.lines,
                            facts.ordinal,
                            handle.facts() == &facts,
                        );
                        return Err(Error::Unsupported);
                    }
                    self.pending = Some(LineMetrics::new(
                        handle,
                        omni_markdown_core::MarkdownPresentationMode::InlinePreview,
                        false,
                    ));
                    self.output = Some(output);
                }
                Ok(Advance::Progress)
            }
            MarkdownLineComposeAnswer::Complete => {
                if let Some(target) = self.target.as_mut() {
                    target.reset_scan();
                }
                self.complete = true;
                if natural {
                    // 自然宽测量产物不参与绘制，完成即取消并归还其源、Reader 和目录资源。
                    self.release_layout();
                } else {
                    // Complete 才取得最终输出；普通前缀不能冒充整格 EOF 或最终 row max。
                    drop(self.output.take());
                    self.output = Some(Rc::new(RefCell::new(
                        self.composer
                            .take()
                            .expect("完成时持原Composer")
                            .into_output()?,
                    )));
                }
                Ok(Advance::Progress)
            }
            answer => {
                // 只在原合成诊断记录实际未接入的 Core 回答；不输出格正文或链接，也不改失败后果。
                #[cfg(feature = "native-markdown-runtime-probe")]
                match answer {
                    MarkdownLineComposeAnswer::NeedsResolution(reason) => eprintln!(
                        "native_cell_unsupported phase=resolution column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsFrontier(reason) => eprintln!(
                        "native_cell_unsupported phase=frontier column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsTabPlacement(reason) => eprintln!(
                        "native_cell_unsupported phase=tab column={} reason={reason:?}",
                        self.column,
                    ),
                    MarkdownLineComposeAnswer::NeedsTableRow(_) => eprintln!(
                        "native_cell_unsupported phase=nested_table_row column={}",
                        self.column,
                    ),
                    _ => unreachable!("其余真实 Core 回答已在上方处理"),
                }
                #[cfg(not(feature = "native-markdown-runtime-probe"))]
                let _ = answer;
                Err(Error::Unsupported) // 未解决的 Unicode/Tab/Frontier 不得截成假 EOF。
            }
        }
    }

    /// 自然宽达到 cap 或真实退场时立即取消并释放该格的所有布局/来源；不触碰 canonical 内容。
    fn release_layout(&mut self) {
        self.cancellation.cancel();
        self.pending = None;
        self.scan = None;
        if let Some(target) = self.target.as_mut() {
            target.reset_scan();
        }
        drop(self.output.take());
        drop(self.composer.take());
        drop(self.display.take());
        drop(self.preparation.take());
    }

    /// 从原末行逆扫到真实视口顶部；当前窗口外字形只留在原分页目录，绝不展开整格行数组。
    fn scan_window(
        &mut self,
        owner: &Owner,
        request: &NativeMarkdownPagedSceneRequest,
        row_top: f64,
        budget: &mut MarkdownLineComposeBudget,
        work: &mut Work,
    ) -> Result<bool> {
        if self
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.request.same(request))
        {
            // 滚动/尺寸/选择 epoch 改变后旧游标不再代表当前视口，不保留过期可见窗口。
            self.scan = None;
        }
        let output = self.output.as_ref().expect("扫描只接受真实已封存prefix");
        if self.scan.is_none() {
            // 从真实末行开始分页逆扫；handles 只容纳交集窗口，不复制所有历史行。
            let cursor = output.borrow_mut().last_line()?;
            self.scan = Some(CellScan {
                request: request.clone(),
                cursor,
                metrics: None,
                bottom: self.height,
                first_top: 0.0,
                handles: VecDeque::new(),
                ready: false,
            });
        }
        let align = {
            let original = owner
                .document
                .borrow()
                .as_ref()
                .expect("原显示格沿父Doc")
                .clone();
            let document = original.borrow();
            self.display
                .as_ref()
                .expect("可见格持原显示Source")
                .canonical_raw_cell()
                .alignment_for(&document)?
                != omni_markdown_core::MarkdownTableAlignment::Left
        };
        let scan = self.scan.as_mut().expect("原扫描已安装");
        if scan.ready {
            return Ok(true);
        }
        let top = (request.virtual_viewport.document_origin.y - row_top - VERTICAL).max(0.0);
        let bottom = request.virtual_viewport.document_origin.y + request.viewport.size.height
            - row_top
            - VERTICAL;
        for _ in 0..METADATA_PER_TURN {
            if budget.metadata_units == 0 || budget.records < 7 {
                // 余量属于所有可见行共享；保原续体，让后继回合继续而非重扫首格。
                return Ok(false);
            }
            if scan.metrics.is_none() {
                let Some(handle) = scan.cursor.take() else {
                    break;
                };
                scan.metrics = Some(LineMetrics::new(
                    handle,
                    omni_markdown_core::MarkdownPresentationMode::InlinePreview,
                    align,
                ));
            }
            let mut metrics = scan.metrics.take().expect("本回合持原行度量");
            if !metrics.advance(output, owner, budget, work)? {
                // 混合字体 part 可能跨回合，未完成前不累减 bottom 或发布该行几何。
                scan.metrics = Some(metrics);
                return Ok(false);
            }
            let height = metrics.height()?;
            let line_top = scan.bottom - height;
            let handle = metrics.into_measured()?;
            let previous = output.borrow_mut().previous_line(handle.handle())?;
            if scan.bottom >= top && line_top <= bottom {
                if scan.handles.len() == MAX_WINDOW_LINES {
                    // 当前窗口超过原有限句柄容量时真实报错，不截断渲染当成完成帧。
                    return Err(Error::Unsupported);
                }
                scan.first_top = line_top;
                scan.handles.push_front(handle);
            }
            scan.bottom = line_top;
            if line_top <= top {
                // 已跨过真实视口顶部，早期行只留在分页目录，不继续扫描到格首。
                scan.cursor = None;
                break;
            }
            scan.cursor = previous;
        }
        scan.ready = scan.cursor.is_none() && scan.metrics.is_none();
        if scan.ready && scan.handles.is_empty() && !self.complete {
            // 未完成格的空 prefix 无法证明此处没有文字；禁止发布 OutsideText 隐藏缺失内容。
            #[cfg(feature = "native-markdown-runtime-probe")]
            eprintln!(
                "native_cell_unsupported phase=uncovered_window column={} row_top={row_top} text_window_top={top} text_window_bottom={bottom} actual_prefix_height={} actual_prefix_lines={}",
                self.column, self.height, self.lines,
            );
            return Err(Error::Unsupported);
        }
        Ok(scan.ready)
    }
}
impl Drop for Cell {
    /// 退出时取消仍持有的原组合任务；Reader/Source 由字段的实际最后所有者自然释放。
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Work {
    /// 同一行尾查询交回真实Source/字素目录工作；它没有形状输入，不伪装成已整形字节。
    pub(super) fn alignment_step(
        &mut self,
        step: &omni_markdown_core::MarkdownComposedLineTrailingWhitespaceStep,
    ) -> Result<()> {
        for (total, value) in [
            (&mut self.bytes, step.bytes),
            (&mut self.scalars, step.scalars),
            (&mut self.metadata, step.metadata_units),
        ] {
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_bytes = self.maximum_bytes.max(step.bytes);
        self.maximum_scalars = self.maximum_scalars.max(step.scalars);
        self.maximum_metadata = self.maximum_metadata.max(step.metadata_units);
        Ok(())
    }

    /// 一个显示量子扣去原Request同次已记的CellInline差量，只把真实转换工作交外层累进。
    fn display_step(
        &mut self,
        measured: SemanticWork,
        before: SemanticWork,
        after: SemanticWork,
    ) -> Result<()> {
        let remainder = |measured: usize, before: usize, after: usize| {
            // 实测累计倒退或重复量超过本轮工作会使资源验收失真，作为真实计量失败返回。
            after
                .checked_sub(before)
                .and_then(|owned| measured.checked_sub(owned))
                .ok_or(Error::CounterExhausted)
        };
        let display = SemanticWork {
            source_bytes: remainder(
                measured.source_bytes,
                before.source_bytes,
                after.source_bytes,
            )?,
            copied_source_bytes: remainder(
                measured.copied_source_bytes,
                before.copied_source_bytes,
                after.copied_source_bytes,
            )?,
            metadata_bytes: remainder(
                measured.metadata_bytes,
                before.metadata_bytes,
                after.metadata_bytes,
            )?,
            utf8_validation_bytes: remainder(
                measured.utf8_validation_bytes,
                before.utf8_validation_bytes,
                after.utf8_validation_bytes,
            )?,
            source_tree_steps: remainder(
                measured.source_tree_steps,
                before.source_tree_steps,
                after.source_tree_steps,
            )?,
            structural_transitions: remainder(
                measured.structural_transitions,
                before.structural_transitions,
                after.structural_transitions,
            )?,
            allocated_source_capacity: remainder(
                measured.allocated_source_capacity,
                before.allocated_source_capacity,
                after.allocated_source_capacity,
            )?,
            normalizer_input_bytes: remainder(
                measured.normalizer_input_bytes,
                before.normalizer_input_bytes,
                after.normalizer_input_bytes,
            )?,
            parser_input_bytes: remainder(
                measured.parser_input_bytes,
                before.parser_input_bytes,
                after.parser_input_bytes,
            )?,
        };
        add_semantic_work(&mut self.display, display)
    }

    /// 保留原Composer实际step读量/最大量子与前沿累计差值，不用字符长度推算处理成本。
    fn composer_step(
        &mut self,
        step: &omni_markdown_core::MarkdownLineComposeStep,
        before: omni_markdown_core::MarkdownLineFrontierWork,
        after: omni_markdown_core::MarkdownLineFrontierWork,
    ) -> Result<()> {
        let frontier = after
            .source_bytes
            .checked_sub(before.source_bytes)
            .ok_or(Error::CounterExhausted)?;
        let shaped = after
            .input_bytes
            .checked_sub(before.input_bytes)
            .ok_or(Error::CounterExhausted)?;
        for (total, value) in [
            (&mut self.bytes, step.bytes),
            (&mut self.scalars, step.scalars),
            (&mut self.metadata, step.metadata_units),
            (&mut self.frontier_source_bytes, frontier),
            (&mut self.shaped_input_bytes, shaped),
        ] {
            // 仅累计原 Composer 返回事实，溢出不得饱和为看似满足上限的小数值。
            *total = total.checked_add(value).ok_or(Error::CounterExhausted)?;
        }
        self.maximum_bytes = self.maximum_bytes.max(step.bytes);
        self.maximum_scalars = self.maximum_scalars.max(step.scalars);
        self.maximum_metadata = self.maximum_metadata.max(step.metadata_units);
        Ok(())
    }
}
