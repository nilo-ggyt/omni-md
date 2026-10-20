//! 同一冻结源的分页字形run：逐标量输入、真实字体和全局UTF16 cluster，正文不物化。
//!
//! 一个job只描述已经完成字体、脚本和方向itemization的run。它不猜混合字体边界，
//! 不把完整run当成一条软行，也不因字形成功而声称Native编辑或整产品内存已经验收。

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use harfrust::{Buffer, BufferFlags, ClusterLevel, ShapeOptions};
use thiserror::Error;

use crate::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutFont,
    MarkdownLayoutFontAuthority, MarkdownLayoutFontResourceObserver, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver,
};

mod cancellation;
#[cfg(feature = "paged-shaping-frontier")]
mod closed_item;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use closed_item::PendingClosedShaping;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) mod frontier;
mod input;
mod options;
mod readonly;
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) use readonly::MarkdownSpoolSliceOrigin;
#[cfg(test)]
mod tests;

pub use cancellation::MarkdownShapingCancellation;
pub use readonly::{
    MARKDOWN_SHAPED_SPOOL_STEP, MarkdownShapedSpool, MarkdownShapedSpoolAuthority,
    MarkdownShapedSpoolBuilder, MarkdownShapedSpoolError, MarkdownShapedSpoolIoError,
    MarkdownShapedSpoolIoOperation, MarkdownShapedSpoolResourceObserver,
    MarkdownShapedSpoolResources, MarkdownShapedSpoolStatus, MarkdownShapedSpoolStep,
};

pub use options::{
    MARKDOWN_SHAPING_MAX_FEATURES, MarkdownShapingDirection, MarkdownShapingFeature,
    MarkdownShapingOptions,
};

/// 两个输入维度独立计费；UTF8残余跨step保留，但不会因此多读超过byte配额。
#[derive(Clone, Copy, Debug)]
pub struct MarkdownShapingBudget {
    pub bytes: usize,
    pub scalars: usize,
}
impl Default for MarkdownShapingBudget {
    fn default() -> Self {
        Self {
            bytes: MARKDOWN_LAYOUT_SOURCE_BYTES,
            scalars: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapingStatus {
    Ingesting,
    AwaitingShape,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingStep {
    pub status: MarkdownShapingStatus,
    pub bytes: usize,
    pub scalars: usize,
}

/// 位置保留shaper的26.6整数，不在Core悄悄改为CoreText另一套advance舍入。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedGlyph {
    pub glyph_id: u32,
    pub source_utf16_cluster: usize,
    pub flags: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

#[derive(Debug, Error)]
pub enum MarkdownShapingError {
    #[error("字形源权限不匹配")]
    SourceMismatch,
    #[error("字形字体权限不匹配")]
    FontMismatch,
    #[error("字形源范围无效或不是标量边界")]
    InvalidRange,
    #[error("字形选项或配额无效")]
    InvalidOptions,
    #[error("字形坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结字形源不是完整UTF8")]
    InvalidUtf8,
    #[error("字形任务尚未完成所需阶段")]
    Pending,
    #[error("字形任务已取消")]
    Cancelled,
    #[error("字形任务已永久撤权")]
    Poisoned,
    #[error("字形分页存储、操作或嵌套预算失败")]
    ShaperStorage,
    #[error("字形算法拒绝当前run：{0}")]
    Shape(#[from] harfrust::ShapeError),
    #[error("无法读取冻结字形源")]
    Source(#[from] MarkdownDocumentBufferError),
}

type Result<T> = std::result::Result<T, MarkdownShapingError>;

/// 输出观察只含计数和真实析构事实；不保活字体、正文、分页映射或历史。
#[derive(Clone)]
pub struct MarkdownShapingResourceObserver {
    paging: harfrust::PagingResourceObserver,
    source: MarkdownLayoutTextSourceObserver,
    font: MarkdownLayoutFontResourceObserver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapingIoOperation {
    CreateFile,
    CloneFile,
    Preallocate,
    Resize,
    Map,
    Sync,
    Unmap,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingIoError {
    pub operation: MarkdownShapingIoOperation,
    pub errno: i32,
}

impl From<harfrust::PagingIoError> for MarkdownShapingIoError {
    fn from(error: harfrust::PagingIoError) -> Self {
        use MarkdownShapingIoOperation as Output;
        use harfrust::PagingOperation as Input;
        let operation = match error.operation {
            Input::CreateFile => Output::CreateFile,
            Input::CloneFile => Output::CloneFile,
            Input::Preallocate => Output::Preallocate,
            Input::Resize => Output::Resize,
            Input::Map => Output::Map,
            Input::Sync => Output::Sync,
            Input::Unmap => Output::Unmap,
            Input::Close => Output::Close,
        }; // 一对一保留实际操作；不靠格式化字符串猜测失败归属。
        Self {
            operation,
            errno: error.errno,
        }
    }
}

/// 同一原job的真实资源事实；虚拟extent不能替代进程RSS或physical footprint。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapingResourceSnapshot {
    pub source_payload_released: bool,
    pub font_payload_released: bool,
    /// 字形元数据固定工作块的实际分配字节；原正文始终归Source独立持有。
    pub work_bytes: usize,
    pub peak_work_bytes: usize,
    pub files_open: usize,
    pub mappings_open: usize,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    pub files_created: u64,
    pub files_closed: u64,
    pub mappings_created: u64,
    pub mappings_released: u64,
    pub first_error: Option<MarkdownShapingIoError>,
    pub cleanup_failed: bool,
    pub shaper_access_certified: bool,
}

impl MarkdownShapingResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownShapingResourceSnapshot {
        let paging = self.paging.snapshot();
        MarkdownShapingResourceSnapshot {
            source_payload_released: self.source.payload_released(),
            font_payload_released: self.font.payload_released(),
            work_bytes: paging.work_bytes,
            peak_work_bytes: paging.peak_work_bytes,
            files_open: paging.files_open,
            mappings_open: paging.mappings_open,
            mapped_bytes: paging.mapped_bytes,
            peak_mapped_bytes: paging.peak_mapped_bytes,
            files_created: paging.files_created,
            files_closed: paging.files_closed,
            mappings_created: paging.mappings_created,
            mappings_released: paging.mappings_released,
            first_error: paging.first_error.map(Into::into),
            cleanup_failed: paging.cleanup_failed,
            shaper_access_certified: paging.bounded_access_certified,
        }
    }

    /// 源/字体payload字段已经交还，且此job全部shaper映射与文件真实关闭，才算已释放。
    ///
    /// 其他job仍可合法共享原字体或Snapshot；此收据不对那些独立owner作虚假归属。
    #[must_use]
    pub fn resources_released(&self) -> bool {
        let snapshot = self.snapshot();
        // 首次业务IO错误仍独立保留；它不抹掉成功归还事实，也不能掩盖cleanup_failed。
        snapshot.source_payload_released
            && snapshot.font_payload_released
            && snapshot.files_open == 0
            && snapshot.mappings_open == 0
            && snapshot.mapped_bytes == 0
            && snapshot.work_bytes == 0
            && !snapshot.cleanup_failed
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownShapingProfile {
    pub source_generation: u64,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    pub input_scalars: usize,
    pub glyph_count: usize,
    /// 此值只证明shaper的分页访问合同；没有包含Native、字体或源mapping的整产品峰值。
    pub shaper_access_certified: bool,
    pub shaper_work_bytes: usize,
    pub shaper_peak_work_bytes: usize,
    pub shaper_mapped_bytes: usize,
    pub shaper_peak_mapped_bytes: usize,
    pub shaper_files_open: usize,
}

struct Payload {
    // 真正的分页数组先Drop，再交还同一个冻结源与字体；观察者不参与任何强边。
    buffer: Buffer,
    source: MarkdownLayoutTextSource,
    font: MarkdownLayoutFont,
    // 只有封闭frontier认证的子任务复用原计划；普通Job继续按原公开路径构造计划。
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_plan: Option<std::sync::Arc<harfrust::ShapePlan>>,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_word: Option<frontier::WordPrefixProof>,
    #[cfg(feature = "paged-shaping-frontier")]
    frontier_view: Option<frontier::local::CertifiedCenterView>,
}

/// 唯一owner按阶段持有真实buffer；失败/取消立即交还全部job资源，不能重采权限后复活。
pub struct MarkdownShapingJob {
    // 索引只持此控制块的Weak；相同源、字体和范围的另一job也不能冒用原glyph结果。
    identity: Arc<()>,
    cancellation: MarkdownShapingCancellation,
    #[cfg(test)]
    cancellation_hook: Option<Arc<dyn Fn(harfrust::CooperativeCheckpoint) + Send + Sync>>,
    payload: Option<Payload>,
    source_authority: MarkdownLayoutTextAuthority,
    font_authority: MarkdownLayoutFontAuthority,
    resources: MarkdownShapingResourceObserver,
    source_utf16: Range<usize>,
    source_bytes: Range<usize>,
    input: input::Input,
    options: options::ValidatedOptions,
    status: MarkdownShapingStatus,
    source_bytes_read: usize,
    source_borrow_max: usize,
    // 固定账本claim最后Drop：原payload/Buffer和此Job其余字段均先真实归还。
    #[cfg(feature = "paged-line-compose")]
    composer_resources: Option<crate::markdown_line_composer::worker_resources::ShapeResourceClaim>,
}

impl MarkdownShapingJob {
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn has_composer_resource_scope(
        &self,
        scope: &crate::markdown_line_composer::worker_resources::ComposerResourceScope,
    ) -> bool {
        self.composer_resources
            .as_ref()
            .is_some_and(|claim| claim.belongs_to(scope))
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn attach_composer_resources(
        &mut self,
        scope: &crate::markdown_line_composer::worker_resources::ComposerResourceScope,
    ) -> Result<()> {
        self.payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .source
            .health()?;
        if self.cancellation.requested() {
            return Err(MarkdownShapingError::Cancelled);
        }
        let resources = self.resources.snapshot();
        if self.composer_resources.is_some()
            || resources.files_created != 0
            || resources.mappings_created != 0
            || resources.peak_work_bytes != 0
        {
            scope.incomplete();
            return Err(MarkdownShapingError::InvalidOptions);
        } // 只在首次glyph工作块或I/O之前安装；后置观察不能冒充内部构造/失败全覆盖。
        self.composer_resources = Some(scope.claim_shape(self.resources.clone()));
        Ok(())
    }
    /// 捕获调用方交来的原source/font身份；验证范围后只读至多40字节的真实邻接上下文。
    ///
    /// 这里不执行字形算法。完整run入列以后，调用方显式执行同步`shape`阶段；该阶段
    /// 可以包含整个run的算法工作量，不能把一次调用包装成“有限主线程耗时”的证明。
    pub fn new(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        source_utf16: Range<usize>,
        options: MarkdownShapingOptions,
    ) -> Result<Self> {
        if !source.permits(source_authority) {
            // 外来源在申请shaper资源前拒绝；相同正文或代次都不能代替payload身份。
            return Err(MarkdownShapingError::SourceMismatch);
        }
        source.health()?; // 原映射已撤权时不能仅凭相同Weak身份继续申请新字形任务。
        if !font.permits(font_authority) {
            // 字体名称/face hash相同不代表同一原字体owner。
            return Err(MarkdownShapingError::FontMismatch);
        }
        if source_utf16.start > source_utf16.end || source_utf16.end > source.utf16_len() {
            return Err(MarkdownShapingError::InvalidRange);
        }
        // cluster以全局UTF16保存；拒绝溢出，不截断成另一个有效位置。
        u32::try_from(source_utf16.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let start = source.byte_offset_for_utf16(source_utf16.start)?;
        let end = source.byte_offset_for_utf16(source_utf16.end)?;
        let options = options::ValidatedOptions::new(options, &font)?;
        Self::assemble(
            source,
            source_authority,
            font,
            font_authority,
            (source_utf16, start..end),
            options,
            MarkdownShapingCancellation::default(),
        )
    }

    /// 原 FontProbe 或分项协调器消费已认证的两套坐标，不为远端字素重走定位。
    /// 这不是公开范围构造口；Source、Font、选项和坐标上界仍按原构造合同验证。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn from_certified_item_input(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>, Option<Range<usize>>),
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(source_authority) {
            return Err(MarkdownShapingError::SourceMismatch);
        }
        if !font.permits(font_authority) {
            return Err(MarkdownShapingError::FontMismatch);
        }
        if ranges.0.start > ranges.0.end
            || ranges.0.end > source.utf16_len()
            || ranges.1.start > ranges.1.end
            || ranges.1.end > source.byte_len()
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        u32::try_from(ranges.0.end).map_err(|_| MarkdownShapingError::CoordinateOverflow)?;
        let options = options::ValidatedOptions::new(options, &font)?;
        let prepared = ranges
            .2
            .map(|bounds| input::prepare_context(&source, ranges.1.clone(), Some(bounds)))
            .transpose()?;
        let mut job = Self::assemble_with_context(
            source,
            source_authority,
            font,
            font_authority,
            (ranges.0, ranges.1, prepared),
            options,
            cancellation,
        )?;
        if job.cancellation.requested() {
            job.guard(source_authority, font_authority)?;
        } // 和原new_with_cancellation相同：真实Source/资源首错优先，并实际归还取消中的候选载荷。
        Ok(job)
    }

    /// 只接收本模块已经验证的样式与原范围；公开构造和真实软行派生共享唯一装配路径。
    fn assemble(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>),
        options: options::ValidatedOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        Self::assemble_with_context(
            source,
            source_authority,
            font,
            font_authority,
            (ranges.0, ranges.1, None),
            options,
            cancellation,
        ) // 公开完整Job及既有前沿仍保持原同步上下文读取合同。
    }

    /// 装配算法只有一份；已收费的闭项上下文与原同步读取仅在输入来源处不同。
    fn assemble_with_context(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        ranges: (Range<usize>, Range<usize>, Option<input::PreparedContext>),
        options: options::ValidatedOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let (source_utf16, source_bytes, prepared) = ranges; // 两套坐标仍来自同一原票，不重新定位。
        let mut buffer = Buffer::new();
        buffer.set_direction(options.direction);
        buffer.set_script(Some(options.script));
        buffer.set_language(options.language.clone());
        buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
        // 两端标记仅来自已itemize的真实文本边界；有限step/chunk从不产生新的BOT/EOT。
        let mut flags = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
        if options.beginning_of_text {
            flags |= BufferFlags::BEGINNING_OF_TEXT;
        }
        if options.end_of_text {
            flags |= BufferFlags::END_OF_TEXT;
        }
        buffer.set_flags(flags);
        let (context_bytes, context_max) = if let Some(prepared) = prepared {
            prepared.install(&mut buffer)
        } else {
            input::install_context(&source, source_bytes.clone(), &mut buffer)?
        }; // 闭项已按前一个Input量子读完，不为安装同一上下文再借Source或重复计费。
        let resources = MarkdownShapingResourceObserver {
            paging: buffer.paging_resource_observer(),
            source: source.resource_observer(),
            font: font.resource_observer(),
        };
        let status = if source_utf16.is_empty() {
            MarkdownShapingStatus::AwaitingShape
        } else {
            MarkdownShapingStatus::Ingesting
        };
        Ok(Self {
            identity: Arc::new(()),
            cancellation,
            #[cfg(test)]
            cancellation_hook: None,
            payload: Some(Payload {
                buffer,
                source,
                font,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_plan: None,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_word: None,
                #[cfg(feature = "paged-shaping-frontier")]
                frontier_view: None,
            }),
            source_authority: source_authority.clone(),
            font_authority: font_authority.clone(),
            resources,
            input: input::Input::new(source_bytes.start, source_utf16.start),
            source_utf16,
            source_bytes,
            options,
            status,
            source_bytes_read: context_bytes,
            source_borrow_max: context_max,
            #[cfg(feature = "paged-line-compose")]
            composer_resources: None,
        })
    }

    /// 仅原Ready任务能为同一item的真实子范围克隆原字体/样式，不接受外部伪造选项。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> Result<Self> {
        let profile = self.profile()?;
        self.glyph(source, font, profile.glyph_count)?; // EOF原语核真实Ready和原权限，不能只读状态。
        let public_range = self.source_range();
        if range.start < public_range.start
            || range.end > public_range.end
            || range.start > range.end
        {
            return Err(MarkdownShapingError::InvalidRange);
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let start = payload.source.byte_offset_for_utf16(range.start)?;
        let end = payload.source.byte_offset_for_utf16(range.end)?;
        let mut options = self.options.clone();
        // 软行分界不是新的段落。仅原item的对应真实端点才能继承BOT/EOT；中部仍保留
        // 同Source的实际邻接上下文，既不把4KiB读窗设为段落，也不改语言/feature全局范围。
        options.beginning_of_text &= range.start == self.source_utf16.start;
        options.end_of_text &= range.end == self.source_utf16.end;
        let source_value = payload.source.clone();
        let font_value = payload.font.clone();
        let cancellation = self.cancellation.clone();
        self.guard(source, font)?;
        let mut child = Self::assemble(
            source_value,
            source,
            font_value,
            font,
            (range, start..end),
            options,
            cancellation,
        )?;
        child.guard(source, font)?; // 构造中的真实源失败或原取消不能发布一个可运行子任务。
        self.guard(source, font)?;
        Ok(child)
    }

    /// 合成控制只在实际深层checkpoint放置确定性barrier；产品不编译此入口或保存闭包。
    #[cfg(all(test, feature = "paged-soft-line-resolve"))]
    pub(crate) fn resolve_test_checkpoint(
        &mut self,
        hook: Arc<dyn Fn(harfrust::CooperativeCheckpoint) + Send + Sync>,
    ) {
        self.cancellation_hook = Some(hook);
    }

    /// 纯Core输入包可先跨线程交付，worker再绑定既有取消请求位构造原任务。
    ///
    /// 不转移已含非Send分页状态的Job；取消位没有正文或字体强边。构造中的真实Source错误
    /// 仍由原new先报告，已经请求取消时沿同一guard归还候选资源，不发布可运行的任务。
    pub fn new_with_cancellation(
        source: MarkdownLayoutTextSource,
        source_authority: &MarkdownLayoutTextAuthority,
        font: MarkdownLayoutFont,
        font_authority: &MarkdownLayoutFontAuthority,
        source_utf16: Range<usize>,
        options: MarkdownShapingOptions,
        cancellation: MarkdownShapingCancellation,
    ) -> Result<Self> {
        let mut job = Self::new(
            source,
            source_authority,
            font,
            font_authority,
            source_utf16,
            options,
        )?;
        job.cancellation = cancellation;
        if job.cancellation.requested() {
            job.guard(source_authority, font_authority)?; // 原IO/Source首因优先，正常取消真实take/drop候选载荷。
        }
        Ok(job) // 未请求取消时仍由后续每次原step/shape/read守同一位，不建立检查与交付之间的复位窗口。
    }

    fn guard(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<()> {
        match self.status {
            MarkdownShapingStatus::Cancelled => return Err(MarkdownShapingError::Cancelled),
            MarkdownShapingStatus::Poisoned => return Err(MarkdownShapingError::Poisoned),
            // 后续每一步都检查原payload；没有在失效后重捕较新source的恢复分支。
            _ => {}
        }
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        if let Err(error) = payload.source.health() {
            // 其他共享reader的真实回收失败也撤销这个job，不能使用旧glyph cache续签Ready。
            return self.poison(error.into());
        }
        if !payload.source.permits(source) || !payload.source.permits(&self.source_authority) {
            return self.poison(MarkdownShapingError::SourceMismatch);
        }
        if !payload.font.permits(font) || !payload.font.permits(&self.font_authority) {
            return self.poison(MarkdownShapingError::FontMismatch);
        }
        if !payload.buffer.allocation_successful() {
            // shape的Ok不包含分页、最大操作数或嵌套预算成功，必须独立拒绝。
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(view) = &payload.frontier_view
            && let Err(error) = view.require(self)
        {
            return self.poison(error);
        } // 支持区不是公开任意裁切入口；具体中心票必须仍属于这个原Job与计划owner。
        if self.cancellation.requested() {
            return self.cancelled(); // Source/分页错误已先检查，不让取消遮掉真实首因。
        }
        Ok(())
    }

    fn poison<T>(&mut self, error: MarkdownShapingError) -> Result<T> {
        self.status = MarkdownShapingStatus::Poisoned;
        // take结束内部可变借用以后Drop，真实归还数组、上下文与本job的source/font强边。
        drop(self.payload.take());
        Err(error)
    }

    /// 推进原run输入；未知尾部保持Ingesting，真实输入范围结束才允许进入AwaitingShape。
    pub fn step(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        budget: MarkdownShapingBudget,
    ) -> Result<MarkdownShapingStep> {
        self.guard(source, font)?;
        if budget.bytes == 0 || budget.scalars == 0 {
            return self.poison(MarkdownShapingError::InvalidOptions);
        }
        if self.status != MarkdownShapingStatus::Ingesting {
            // 输入完毕后的空观察不是第二次shape，也不会重新添加相同字符。
            return Ok(MarkdownShapingStep {
                status: self.status,
                bytes: 0,
                scalars: 0,
            });
        }
        let result = self.step_input(budget);
        match result {
            Ok(step) => Ok(step),
            // 即使此step已在有限buffer中加入前缀，也必须整体撤权，禁止发布部分成功。
            Err(MarkdownShapingError::Cancelled) => self.cancelled(),
            Err(error) => self.poison(error),
        }
    }

    /// 同步执行原run的完整字形算法；IO、实际访问认证和输出元数据分别核验。
    pub fn shape(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<MarkdownShapingStatus> {
        self.guard(source, font)?;
        match self.status {
            MarkdownShapingStatus::Ingesting => return Err(MarkdownShapingError::Pending),
            MarkdownShapingStatus::Ready => return Ok(MarkdownShapingStatus::Ready),
            MarkdownShapingStatus::AwaitingShape => {}
            // guard已经处理这两条退出，保留显式匹配避免未来状态被误放入shape。
            MarkdownShapingStatus::Cancelled => return Err(MarkdownShapingError::Cancelled),
            MarkdownShapingStatus::Poisoned => return Err(MarkdownShapingError::Poisoned),
        }
        let control = self.cooperative_control()?;
        let result = {
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownShapingError::Poisoned)?;
            let shaper = payload
                .font
                .shaper_at_scale(self.options.scale)
                .map_err(|_| MarkdownShapingError::InvalidOptions)?;
            // 冷Font的OnceLock初始化不在取消scope内；其返回后深层原算法逐批观察原请求。
            harfrust::with_cooperative_control(control, || {
                let shape_options = ShapeOptions::new()
                    .point_size(Some(self.options.points))
                    .features(&self.options.features);
                // 元数据证书与执行必须指向同一个已编译计划；不在shape时悄悄重建另一计划。
                #[cfg(feature = "paged-shaping-frontier")]
                let shape_options = shape_options.plan(payload.frontier_plan.as_deref());
                harfrust::shape(&shaper, &mut payload.buffer, shape_options)
            })
        };
        if let Err(error) = result {
            // 原Source与分页的真实错误先于取消：终态桥不可把IO失败改写成干净取消。
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownShapingError::Poisoned)?;
            if let Err(source_error) = payload.source.health() {
                return self.poison(source_error.into());
            }
            if payload.buffer.paging_error().is_some() {
                return self.poison(MarkdownShapingError::ShaperStorage);
            }
            if error == harfrust::ShapeError::Cancelled {
                return self.cancelled();
            }
            return self.poison(error.into());
        }
        let buffer = &self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .buffer;
        if !buffer.allocation_successful() || !buffer.paged_checkpoint() {
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        self.guard(source, font)?; // 真实shape/分页返回后再核原请求，禁止把期间取消发布成Ready。
        // Linux仍可计算真实字形，但其分页驻留合同尚未实测；profile明确保持false。
        self.status = MarkdownShapingStatus::Ready;
        Ok(self.status)
    }

    /// 只交付一条Copy glyph。调用方按真实视口范围查询，不能取得整片或正文借用。
    pub fn glyph(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        index: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        self.guard(source, font)?;
        if self.status != MarkdownShapingStatus::Ready {
            return Err(MarkdownShapingError::Pending);
        }
        let buffer = &self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?
            .buffer;
        #[cfg(feature = "paged-shaping-frontier")]
        let view = self
            .payload
            .as_ref()
            .and_then(|p| p.frontier_view.as_ref())
            .map(|v| v.glyphs.clone());
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let view: Option<Range<usize>> = None;
        let count = view.as_ref().map_or(buffer.len(), Range::len);
        if index >= count {
            return Ok(None); // 实际EOF明确无记录；这不是未知输入前缀的“暂时为空”。
        }
        let actual_index = view.as_ref().map_or(index, |range| range.start + index);
        let Some((info, position)) = buffer.paged_glyph_record(actual_index) else {
            return self.poison(MarkdownShapingError::ShaperStorage);
        };
        if !buffer.allocation_successful() {
            return self.poison(MarkdownShapingError::ShaperStorage);
        }
        let cluster = info.cluster as usize;
        if !self.source_range().contains(&cluster) {
            // cluster必须属于本run原全局范围；glyph0等缺字仍是合法结果，不能裁掉。
            return self.poison(MarkdownShapingError::InvalidRange);
        }
        self.guard(source, font)?; // 原Copy记录取回后仍须原Source/Font/token有效，才允许交付一条glyph。
        Ok(Some(MarkdownShapedGlyph {
            glyph_id: info.glyph_id,
            source_utf16_cluster: cluster,
            flags: info.flags().to_bits() & harfrust::GlyphFlags::ALL.to_bits(),
            x_advance: self.options.quantize(position.x_advance),
            y_advance: self.options.quantize(position.y_advance),
            x_offset: self.options.quantize(position.x_offset),
            y_offset: self.options.quantize(position.y_offset),
        }))
    }

    #[must_use]
    pub fn status(&self) -> MarkdownShapingStatus {
        self.status
    }
    /// 精确标识本次任务，不能保活字形数组、字体或正文；Core派生索引仍须核原job健康。
    pub(crate) fn index_identity(&self) -> Weak<()> {
        Arc::downgrade(&self.identity)
    }
    /// 返回原校验选项的方向，不把尚未完成的结果提升为Ready。
    #[must_use]
    pub fn direction(&self) -> MarkdownShapingDirection {
        match self.options.direction {
            harfrust::Direction::LeftToRight => MarkdownShapingDirection::LeftToRight,
            harfrust::Direction::RightToLeft => MarkdownShapingDirection::RightToLeft,
            harfrust::Direction::TopToBottom => MarkdownShapingDirection::TopToBottom,
            harfrust::Direction::BottomToTop => MarkdownShapingDirection::BottomToTop,
            // 原构造器只接受四种具体方向；Invalid从不进入本job。
            harfrust::Direction::Invalid => unreachable!("字形任务方向已在构造时校验"),
        }
    }
    /// 原校验选项中的真实请求字号；仅提供样式事实，不代表任务Ready或原权限仍有效。
    #[must_use]
    pub fn points(&self) -> f32 {
        self.options.points
    }
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(view) = self.payload.as_ref().and_then(|p| p.frontier_view.as_ref()) {
            return view.source.clone();
        }
        self.source_utf16.clone()
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapingResourceObserver {
        self.resources.clone()
    }

    /// profile来自同一个存活job；失败或取消不会借“零资源”重新签署成功统计。
    pub fn profile(&mut self) -> Result<MarkdownShapingProfile> {
        let source = self.source_authority.clone();
        let font = self.font_authority.clone();
        self.guard(&source, &font)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapingError::Poisoned)?;
        let paging = self.resources.paging.snapshot();
        let glyph_count = if self.status == MarkdownShapingStatus::Ready {
            #[cfg(feature = "paged-shaping-frontier")]
            let ready_count = payload
                .frontier_view
                .as_ref()
                .map_or_else(|| payload.buffer.len(), |view| view.glyphs.len());
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let ready_count = payload.buffer.len();
            ready_count
        } else {
            0 // 尚未shape的Buffer长度是输入scalar数，不能冒充最终glyph数。
        };
        Ok(MarkdownShapingProfile {
            source_generation: payload.source.generation(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            input_scalars: self.input.scalars,
            glyph_count,
            shaper_access_certified: payload.buffer.paged_access_certified()
                && paging.bounded_access_certified,
            shaper_mapped_bytes: paging.mapped_bytes,
            shaper_work_bytes: paging.work_bytes,
            shaper_peak_work_bytes: paging.peak_work_bytes,
            shaper_peak_mapped_bytes: paging.peak_mapped_bytes,
            shaper_files_open: paging.files_open,
        })
    }

    /// 显式取消是终态；立即交还此job实际资源，外部观察者随后检查真实close/unmap结果。
    pub fn cancel(&mut self) {
        self.cancellation.request(); // 同一个请求位同时覆盖尚在运行的深层算法，不创建第二token。
        if matches!(
            self.status,
            MarkdownShapingStatus::Cancelled | MarkdownShapingStatus::Poisoned
        ) {
            return; // 终态保持原来失败事实，不能把Poisoned清成成功的Cancelled。
        }
        let _ = self.cancelled::<()>(); // 原void API保留；cleanup失败仍在Poisoned状态和原资源收据中。
    }
}
