//! 分页UBA元数据：按有限piece读取属性，所有与段长线性增长的中间状态存入匿名文件。
//!
//! X/W/N/I与保留格式字符的规则移植自unicode-bidi 0.3.18；不调用其整段BidiInfo构造。
//! 本模块不产生字形、caret或Native对象，不能单独证明编辑器已经完成视口排版。
//!
//! Copyright 2015 The Servo Project Developers.
//! Algorithm portions adapted from unicode-bidi 0.3.18, <https://github.com/servo/unicode-bidi>.
//! SPDX-License-Identifier: MIT OR Apache-2.0
//! 原上游MIT许可与归属随本目录LICENSE-MIT保留；改动仅将存储和游标替换为分页实现。

use std::ops::Range;
use std::sync::Arc;

use thiserror::Error;
use unicode_bidi::{BidiClass, BidiDataSource};

use crate::{MarkdownDocumentBufferError, MarkdownTextSnapshot};

mod cancellation;
pub use cancellation::MarkdownBidiCancellation;
mod explicit;
mod implicit;
mod line;
mod resource;
pub use resource::{MarkdownBidiResourceObserver, MarkdownBidiResourceSnapshot};
mod prepare;
mod sequences;
mod source;
mod store;
// 文档叶聚合与UBA只使用这一份Unicode 18发布增量；生成文件和许可保持原位置。
use crate::unicode_bidi_data as unicode_data;
mod uniform;
pub use prepare::{
    MarkdownBidiPrepareBudget, MarkdownBidiPrepareJob, MarkdownBidiPrepareStatus,
    MarkdownBidiPrepareStep,
};

use store::{NONE, Store, Table, read_u32, write_u32};
use unicode_data::UnicodeBidiData;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BidiBaseDirection {
    Auto,
    LeftToRight,
    RightToLeft,
}

impl BidiBaseDirection {
    fn level(self) -> Option<u8> {
        match self {
            Self::Auto => None,
            Self::LeftToRight => Some(0),
            Self::RightToLeft => Some(1),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BidiMemoryBudget {
    pages: u16,
}

impl BidiMemoryBudget {
    pub fn new(cache_bytes: usize) -> Result<Self, BidiError> {
        if !(4 * store::PAGE_BYTES..=1024 * 1024).contains(&cache_bytes)
            || !cache_bytes.is_multiple_of(store::PAGE_BYTES)
        {
            return Err(BidiError::InvalidBudget); // 预算不是提示值；不能静默增大或退成全表Vec。
        }
        Ok(Self {
            pages: (cache_bytes / store::PAGE_BYTES) as u16,
        })
    }
}

impl Default for BidiMemoryBudget {
    fn default() -> Self {
        Self { pages: 64 }
    }
}

#[derive(Debug, Error)]
pub enum BidiError {
    #[error("双向文本分析已取消")]
    Cancelled,
    #[error("双向文本分析不属于原布局源")]
    SourceMismatch,
    #[error("双向文本分析范围无效")]
    InvalidRange,
    #[error("双向文本分析端点不是完整标量边界")]
    InvalidScalarBoundary,
    #[error("双向文本分析缓存预算无效")]
    InvalidBudget,
    #[error("双向文本分析坐标溢出")]
    CoordinateOverflow,
    #[error("双向文本元数据不完整")]
    CorruptMetadata,
    #[error("匿名双向文本元数据已因存储失败撤销")]
    PoisonedMetadata,
    #[error("无法读取冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名双向文本元数据")]
    Storage(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiRun {
    pub source_utf16_start: usize,
    pub source_utf16_end: usize,
    pub level: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiStorageProfile {
    pub cache_bytes: usize,
    pub page_loads: u64,
    pub source_borrow_max_bytes: usize,
    pub utf16_units: usize,
    pub unicode_version: (u64, u64, u64),
}

/// 只有所有段和所有规则完成后才能构造；逻辑内容不可变，&mut查询仅用于有界IO页缓存。
pub struct MarkdownBidiParagraph {
    // 保留原immutable资源，供同一分析值生命周期使用；查询本身只读取派生元数据，不再读取正文。
    _snapshot: MarkdownTextSnapshot,
    // 新布局路径保留同一个capsule身份；旧Snapshot API不伪造布局权限。
    #[cfg(feature = "paged-layout-breaks")]
    layout_source: Option<crate::MarkdownLayoutTextSource>,
    source: Range<usize>,
    resolution: Resolution,
    units: u32,
    paragraph_count: Option<u32>,
    source_borrow_max: usize,
}

/// 只借用原完整Uniform结果的范围事实；不授予GCB端点、活动文档或后继Source权限。
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) struct UniformControlFreeSpan<'a> {
    _paragraph: &'a MarkdownBidiParagraph,
    range: Range<usize>,
    paragraph_range: Range<usize>,
    content_end: usize,
}

#[cfg(feature = "paged-shaping-frontier")]
impl UniformControlFreeSpan<'_> {
    pub(crate) fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// 原完整Uniform只认证一个真实分析段；返回其全范围，不把本次子range冒充新段。
    /// 此借用已核同Source和取消，仍不证明标量/GCB端点或分析范围外的段归属。
    pub(crate) fn paragraph_range(&self) -> Range<usize> {
        self.paragraph_range.clone()
    }

    /// 原同源有界B查询已经验证的正文末端；分隔符仍留给原GCB/Script真实消费。
    pub(crate) fn content_end(&self) -> usize {
        self.content_end
    }

    pub(crate) fn level(&self) -> u8 {
        0 // 只有原UniformZero且完整证明无六控制的结果可构造本借用值。
    }
}

/// 只有完整证书才能省略分页表；未证明的输入始终保留原X/W/N/I存储和算法。
enum Resolution {
    UniformZero(uniform::UniformResolution),
    Paged(Store),
}

impl Resolution {
    fn healthy(&self) -> Result<(), BidiError> {
        match self {
            Self::UniformZero(cancellation) => cancellation.check(),
            Self::Paged(store) => store.healthy(),
        } // Uniform没有文件或页，但取消仍永久撤销已完成的分析值。
    }

    #[cfg(test)]
    fn paged_mut(&mut self) -> &mut Store {
        match self {
            Self::Paged(store) => store,
            Self::UniformZero(_) => panic!("此故障/资源测试必须使用真实复杂UBA输入"),
        }
    }
}

impl MarkdownBidiParagraph {
    /// 仅用于原协调器选择是否有新端点工作；不读取来源或签发任何范围事实。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn uses_prepared_paragraphs(&self) -> bool {
        matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
    }

    /// 原有限oracle仍核真实段数；未建立总目录的表示沿公开范围查询逐段验证，不伪造总数。
    #[cfg(test)]
    fn actual_paragraph_count(&mut self) -> usize {
        if let Some(count) = self.paragraph_count {
            return count as usize;
        }
        let mut position = self.source.start;
        let mut count = 0;
        while position < self.source.end {
            let paragraph = self.paragraph_range_at_utf16(position).unwrap().unwrap();
            assert_eq!(paragraph.start, position);
            assert!(paragraph.end > position);
            position = paragraph.end;
            count += 1;
        }
        assert_eq!(position, self.source.end);
        count
    }

    /// 只取得实际FD及固定缓存页的原子收据；不会延长分析、Source或Native对象的生命周期。
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        match &self.resolution {
            Resolution::UniformZero(_) => MarkdownBidiResourceObserver::unallocated(),
            Resolution::Paged(store) => store.resource_observer(),
        }
    }

    pub fn analyze(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
    ) -> Result<Self, BidiError> {
        Self::analyze_cancellable(
            snapshot,
            source,
            base,
            budget,
            &MarkdownBidiCancellation::new(),
        )
    }

    fn analyze_cancellable(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::analyze_cancellable_observed(snapshot, source, base, budget, cancellation, None)
    }

    fn analyze_cancellable_observed(
        snapshot: MarkdownTextSnapshot,
        source: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Option<&Arc<resource::ResourceState>>,
    ) -> Result<Self, BidiError> {
        prepare::MarkdownBidiPrepareJob::snapshot_range(
            snapshot,
            source,
            base,
            budget,
            cancellation,
            resources,
        )?
        .run()
    }

    /// 使用断行、字素与字形阶段的同一个冻结源，不由相同代次或相同正文推断身份。
    ///
    /// UBA仍一次同步完成整段规则，工作状态位于匿名分页元数据中；调用者不能把它放到
    /// 主线程后宣称有限响应时间。本接口只解决来源和永久I/O状态一致，不代表Native验收。
    #[cfg(feature = "paged-layout-breaks")]
    pub fn analyze_layout_source(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
    ) -> Result<Self, BidiError> {
        Self::analyze_layout_source_cancellable(
            source,
            authority,
            paragraph,
            base,
            budget,
            &MarkdownBidiCancellation::new(),
        )
    }

    /// 使用原冻结源和原票据同步计算；Native可把本纯Core入口放入自己的后台任务。
    /// 每次固定元数据访问检查同一取消位，取消后原对象的Ready查询同样永久拒绝。
    #[cfg(feature = "paged-layout-breaks")]
    pub fn analyze_layout_source_cancellable(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::analyze_layout_source_cancellable_observed_inner(
            source,
            authority,
            paragraph,
            base,
            budget,
            cancellation,
            None,
        )
    }

    /// 供行生产者在失败返回时同样取得本次实际FD/页的固定收据；不签发任何布局或源权限。
    #[cfg(all(test, feature = "paged-line-compose"))]
    pub(crate) fn analyze_layout_source_cancellable_observed(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
    ) -> (Result<Self, BidiError>, MarkdownBidiResourceObserver) {
        let resources = resource::ResourceState::new();
        let observer = resources.observe();
        let result = Self::analyze_layout_source_cancellable_observed_inner(
            source,
            authority,
            paragraph,
            base,
            budget,
            cancellation,
            Some(&resources),
        );
        (result, observer) // 返回前已结束所有局部失败补偿；observer不持File、Source或规则计算状态。
    }

    #[cfg(feature = "paged-layout-breaks")]
    fn analyze_layout_source_cancellable_observed_inner(
        source: crate::MarkdownLayoutTextSource,
        authority: &crate::MarkdownLayoutTextAuthority,
        paragraph: Range<usize>,
        base: BidiBaseDirection,
        budget: BidiMemoryBudget,
        cancellation: &MarkdownBidiCancellation,
        resources: Option<&Arc<resource::ResourceState>>,
    ) -> Result<Self, BidiError> {
        source.health()?;
        if !source.permits(authority) {
            return Err(BidiError::SourceMismatch); // 不以同文本、同generation或新票据替代原source身份。
        }
        cancellation.check()?;
        let mut analyzed = Self::analyze_cancellable_observed(
            source.snapshot_for_bidi(),
            paragraph,
            base,
            budget,
            cancellation,
            resources,
        )?;
        source.health()?;
        analyzed.resolution.healthy()?;
        analyzed.layout_source = Some(source);
        Ok(analyzed)
    }

    /// Native发布几何前核对同一源票；本方法不授予活动文档或较新代次的消费权限。
    #[cfg(feature = "paged-layout-breaks")]
    pub fn require_layout_source(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<(), BidiError> {
        self._snapshot.original_backing_health()?; // 真实源首错先于外来票、取消或已完成缓存。
        let result = (|| {
            let source = self
                .layout_source
                .as_ref()
                .ok_or(BidiError::SourceMismatch)?;
            source.health()?;
            if !source.permits(authority) {
                // 不同capture即使文本与generation完全相同，也不能混入同一次几何发布。
                return Err(BidiError::SourceMismatch);
            }
            self.resolution.healthy()
        })();
        self._snapshot.original_backing_health()?;
        if let Some(source) = &self.layout_source {
            source.health()?;
        }
        result?; // 外来票/取消失败同样先复核原Source，不把同时确认的errno洗为较弱原因。
        self.resolution.healthy()
    }

    /// 仅完整原Source的UniformZero才能把视觉前后等同逻辑前后；子段level0不授此权限。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn whole_source_uniform_zero(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
    ) -> Result<bool, BidiError> {
        self.require_layout_source(authority)?;
        let source = self
            .layout_source
            .as_ref()
            .ok_or(BidiError::SourceMismatch)?;
        let proved = self.source == (0..source.utf16_len())
            && matches!(self.resolution, Resolution::UniformZero(_));
        self.require_layout_source(authority)?; // 已完成值仍经过原Source首错、同capture和取消后置门。
        Ok(proved)
    }

    /// 在原分析范围内消费完整Uniform的“无六种内联控制且level0”事实，不重新借正文。
    /// 数字范围可含低代理项；它不是可编辑端点证明，分项交付仍须检查原GCB边界。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn uniform_control_free_span(
        &self,
        authority: &crate::MarkdownLayoutTextAuthority,
        range: Range<usize>,
    ) -> Result<Option<UniformControlFreeSpan<'_>>, BidiError> {
        self.require_layout_source(authority)?;
        let valid = range.start <= range.end
            && range.start >= self.source.start
            && range.end <= self.source.end;
        let paragraph_range = match &self.resolution {
            Resolution::UniformZero(certificate) if certificate.control_free() => {
                Some((self.source.clone(), self.source.end))
            }
            Resolution::UniformZero(certificate)
                if certificate.paragraphs && certificate.interior_controls_absent =>
            {
                certificate.paragraph.control_free(&range)
            }
            _ => None, // Paged及未证明的六种内联控制保持原查询；level0不授予GCB或字体权限。
        };
        self.require_layout_source(authority)?;
        if !valid {
            return Err(BidiError::InvalidRange);
        } // 数字范围失败也先完成后置Source/取消门，不为端点校验借正文。
        Ok(
            paragraph_range.map(|(paragraph_range, content_end)| UniformControlFreeSpan {
                _paragraph: self,
                range,
                paragraph_range,
                content_end,
            }),
        ) // 借用原Paragraph，不Clone权限/Source，不以generation相等转让证书。
    }

    /// Paged已经有全段表；多段Uniform只按原预算准备实际当前B范围，不把源全部展开为层级记录。
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn prepare_uniform_paragraph_range(
        &mut self,
        authority: &crate::MarkdownLayoutTextAuthority,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<uniform::ParagraphPrepareStep, BidiError> {
        self.require_layout_source(authority)?;
        if budget.bytes == 0 || budget.scalars == 0 || budget.metadata_ops == 0 {
            return Err(BidiError::InvalidBudget);
        }
        if position < self.source.start || position > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        let result = match &mut self.resolution {
            Resolution::UniformZero(certificate)
                if certificate.paragraphs && position < self.source.end =>
            {
                certificate
                    .paragraph
                    .step(&self._snapshot, position, budget)
            }
            _ => Ok(uniform::ParagraphPrepareStep::ready()),
        };
        if let Ok(step) = &result {
            self.source_borrow_max = self.source_borrow_max.max(step.maximum_borrow);
        }
        self.require_layout_source(authority)?; // 元数据剪枝也保原Source/取消；真实lease失败先于候选结果。
        result
    }

    /// 原同步公开查询也只复用同一有限叶算法；生产ItemStart先prepare，不在其量子内隐式扫描叶。
    #[cfg(feature = "paged-shaping-frontier")]
    fn uniform_paragraph_at(&mut self, position: usize) -> Result<Option<Range<usize>>, BidiError> {
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        if position < self.source.start || position > self.source.end {
            return Err(BidiError::InvalidRange);
        }
        if position == self.source.end {
            return Ok(None);
        }
        let Resolution::UniformZero(certificate) = &mut self.resolution else {
            unreachable!()
        };
        if let Some(range) = certificate.paragraph.range_covering(position) {
            if certificate.paragraph.range_at(position).is_none()
                && !self._snapshot.utf16_boundaries_are_scalar()?
            {
                self._snapshot
                    .byte_offset_for_utf16(position)
                    .map_err(scalar_endpoint_error)?;
            } // 公共同步API仍拒绝代理项内部；范围事实不能替代scalar边界。
            self._snapshot.original_backing_health()?;
            self.resolution.healthy()?;
            return Ok(Some(range));
        }
        while certificate.paragraph.range_at(position).is_none() {
            let step = certificate.paragraph.step(
                &self._snapshot,
                position,
                MarkdownBidiPrepareBudget {
                    bytes: 4096,
                    scalars: 256,
                    metadata_ops: 256,
                },
            )?;
            self.source_borrow_max = self.source_borrow_max.max(step.maximum_borrow);
        } // 同步API原本允许完成有限元数据查询；没有新增整段缓冲、Reader或另一份段规则。
        let range = certificate.paragraph.range_at(position);
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(range)
    }

    pub fn source_range(&self) -> Range<usize> {
        self.source.clone()
    }

    /// 二分原已完成的段表，不为每个字体/脚本item重新读取整个段落。
    /// 返回该分析值内的真实UBA段；分析范围本身若经裁剪，调用者仍须证明其外侧正文边界。
    /// 真EOF返回None，低代理项/分析范围以外的位置失败；两者同样核原源与取消状态。
    pub fn paragraph_range_at_utf16(
        &mut self,
        offset: usize,
    ) -> Result<Option<Range<usize>>, BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
        {
            return self.uniform_paragraph_at(offset);
        }
        let relative = self.checked_range(offset..offset)?.start;
        if relative == self.units {
            return Ok(None);
        } // EOF没有字符，不制造末尾空段或把前一段范围套到后继item。
        let found = match &mut self.resolution {
            Resolution::UniformZero(_) => self.source.clone(),
            // 完整Uniform证书只允许唯一段；字符不为空时原分析范围就是其精确段范围。
            Resolution::Paged(store) => {
                let mut low = 0;
                let mut high = self.paragraph_count.expect("实际分页段表已经计算总数");
                let paragraph = loop {
                    if low >= high {
                        return Err(BidiError::CorruptMetadata);
                    } // 原分析覆盖每个字符；找不到所属段不能退为“未知”或重扫正文补救。
                    let mid = low + (high - low) / 2;
                    let paragraph = Paragraph::read(store, mid)?;
                    if paragraph.start >= paragraph.end || paragraph.end > self.units {
                        return Err(BidiError::CorruptMetadata);
                    } // 磁盘记录必须是真实非空子范围，不能让损坏坐标驱动二分或溢出加法。
                    if relative < paragraph.start {
                        high = mid;
                    } else if relative >= paragraph.end {
                        low = mid + 1;
                    } else {
                        break paragraph;
                    } // 只命中包含原字符的半开段，分隔符沿capture已有归属，不另猜CRLF等边界。
                };
                self.source.start + paragraph.start as usize
                    ..self.source.start + paragraph.end as usize
            }
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?; // 元数据IO后的同一源/取消复核，不能发布撤权前找到的候选。
        Ok(Some(found))
    }

    /// 读取原UBA段的真实base，不把首个字符的resolved level当段方向。
    /// LS后的空视觉行仍属于该段；EOF返回None，真正新空段由调用者按原段策略处理。
    pub fn paragraph_base_level_at_utf16(
        &mut self,
        offset: usize,
    ) -> Result<Option<u8>, BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
        {
            return self
                .uniform_paragraph_at(offset)
                .map(|range| range.map(|_| 0));
        }
        let relative = self.checked_range(offset..offset)?.start;
        if relative == self.units {
            return Ok(None);
        }
        let base = match &mut self.resolution {
            Resolution::UniformZero(_) => 0, // 完整证书只认证单个base=0段，不以字形方向猜测。
            Resolution::Paged(store) => {
                let (mut low, mut high) =
                    (0, self.paragraph_count.expect("实际分页段表已经计算总数"));
                loop {
                    if low >= high {
                        return Err(BidiError::CorruptMetadata);
                    }
                    let middle = low + (high - low) / 2;
                    let paragraph = Paragraph::read(store, middle)?;
                    if paragraph.start >= paragraph.end
                        || paragraph.end > self.units
                        || paragraph.base > 1
                    {
                        return Err(BidiError::CorruptMetadata);
                    }
                    if relative < paragraph.start {
                        high = middle;
                    } else if relative >= paragraph.end {
                        low = middle + 1;
                    } else {
                        break paragraph.base;
                    }
                }
            }
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(Some(base))
    }

    pub fn level_at_utf16(&mut self, index: usize) -> Result<u8, BidiError> {
        self._snapshot.original_backing_health()?; // 已有level缓存也不能越过原源的永久读取失败。
        self.resolution.healthy()?;
        let relative = index
            .checked_sub(self.source.start)
            .ok_or(BidiError::InvalidRange)?;
        if relative >= self.units as usize {
            return Err(BidiError::InvalidRange); // EOF没有字符level；低代理项沿真实duplicate record读取。
        }
        let level = match &mut self.resolution {
            // level是每UTF16单位事实；低代理项仍与head共享0，不把它当成run合法端点。
            Resolution::UniformZero(_) => 0,
            Resolution::Paged(store) => Text::read(store, relative as u32)?.level,
        };
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()?;
        Ok(level)
    }

    /// 顺序交付逻辑run的Copy候选；调用方只能在本次返回Ok后提交这一批事实。
    ///
    /// 中途IO失败可能发生在若干次回调之后。返回Err时必须丢弃本次全部候选，
    /// 不能在回调里直接写入显示状态、发送Native动作或把先前片段标记为完整覆盖。
    pub fn visit_logical_runs(
        &mut self,
        range: Range<usize>,
        mut visit: impl FnMut(MarkdownBidiRun),
    ) -> Result<(), BidiError> {
        let relative = self.checked_range(range)?;
        let store = match &mut self.resolution {
            Resolution::UniformZero(cancellation) => {
                if !relative.is_empty() {
                    // 单段证书保证整个请求连续且为0；回调后仍须核原源和取消，不能提前发布候选。
                    visit(MarkdownBidiRun {
                        source_utf16_start: self.source.start + relative.start as usize,
                        source_utf16_end: self.source.start + relative.end as usize,
                        level: 0,
                    });
                }
                self._snapshot.original_backing_health()?;
                return cancellation.check();
            }
            Resolution::Paged(store) => store,
        };
        let mut at = relative.start;
        while at < relative.end {
            let level = Text::read(store, at)?.level;
            let from = at;
            while at < relative.end && Text::read(store, at)?.level == level {
                at += Text::read(store, at)?.scalar_len()?;
            }
            visit(MarkdownBidiRun {
                source_utf16_start: self.source.start + from as usize,
                source_utf16_end: self.source.start + at as usize,
                level,
            }); // 页借用已经结束；回调只得到有限Copy事实，不拿到snapshot或缓存buffer。
        }
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()
    }

    /// 调用方必须提交实际断好的单条软行；这里只应用L1/L2，不猜字体宽度或划分视觉行。
    ///
    /// 回调交付的仍是候选。若后继IO失败而返回Err，调用方必须丢弃本次所有候选；
    /// 只有完整Ok才授予本条行的覆盖，不得在回调期间直接发布Native几何或绘制状态。
    pub fn visit_visual_runs(
        &mut self,
        actual_soft_line: Range<usize>,
        mut visit: impl FnMut(MarkdownBidiRun),
    ) -> Result<(), BidiError> {
        #[cfg(feature = "paged-shaping-frontier")]
        if !actual_soft_line.is_empty()
            && matches!(&self.resolution, Resolution::UniformZero(certificate) if certificate.paragraphs)
            && self
                .uniform_paragraph_at(actual_soft_line.start)?
                .is_none_or(|paragraph| actual_soft_line.end > paragraph.end)
        {
            return Err(BidiError::InvalidRange); // 单条真实L1行不能越过另一个B段；CRLF也不合并。
        }
        let relative = self.checked_range(actual_soft_line)?;
        match &mut self.resolution {
            Resolution::UniformZero(_) => {
                if !relative.is_empty() {
                    // 证书只允许无B或唯一尾B，故任何合法子范围都不会跨第二个UBA段。
                    visit(MarkdownBidiRun {
                        source_utf16_start: self.source.start + relative.start as usize,
                        source_utf16_end: self.source.start + relative.end as usize,
                        level: 0,
                    });
                }
            }
            Resolution::Paged(store) => {
                line::visit(
                    store,
                    self.paragraph_count.expect("实际分页段表已经计算总数"),
                    self.source.start,
                    relative,
                    visit,
                )?;
            }
        }
        self._snapshot.original_backing_health()?;
        self.resolution.healthy()
    }

    pub fn storage_profile(&self) -> MarkdownBidiStorageProfile {
        let (cache_bytes, page_loads) = match &self.resolution {
            Resolution::UniformZero(_) => (0, 0),
            Resolution::Paged(store) => (store.cache_bytes(), store.page_loads),
        }; // 资源事实按真实表示报告；Uniform不能冒充配置过但实际不存在的1MiB缓存。
        MarkdownBidiStorageProfile {
            cache_bytes,
            page_loads,
            source_borrow_max_bytes: self.source_borrow_max,
            utf16_units: self.units as usize,
            unicode_version: unicode_data::UNICODE_VERSION,
        }
    }

    fn checked_range(&mut self, range: Range<usize>) -> Result<Range<u32>, BidiError> {
        self._snapshot.original_backing_health()?; // 空行与索引命中同样保留底层失败原因。
        self.resolution.healthy()?; // 空查询同样不能把已撤销的整个分析值恢复成Ok。
        if range.start > range.end || range.start < self.source.start || range.end > self.source.end
        {
            return Err(BidiError::InvalidRange);
        }
        let start = (range.start - self.source.start) as u32;
        let end = (range.end - self.source.start) as u32;
        match &mut self.resolution {
            Resolution::UniformZero(_) => {
                #[cfg(feature = "paged-shaping-frontier")]
                let endpoints_are_scalar = self._snapshot.utf16_boundaries_are_scalar()?;
                #[cfg(not(feature = "paged-shaping-frontier"))]
                let endpoints_are_scalar = false;
                for position in [start, end] {
                    if endpoints_are_scalar {
                        continue;
                    } // 同root无代理对的精确聚合已证明整数端点。
                    // 冻结树的端点转换只借用命中有限叶，不为Uniform建立代理项镜像。
                    self._snapshot
                        .byte_offset_for_utf16(self.source.start + position as usize)
                        .map_err(scalar_endpoint_error)?;
                }
                self._snapshot.original_backing_health()?;
                self.resolution.healthy()?;
            }
            Resolution::Paged(store) => {
                for position in [start, end] {
                    if position < self.units && Text::read(store, position)?.continuation() {
                        return Err(BidiError::InvalidScalarBoundary); // run不能拆开代理对，即使level_at允许观察低项。
                    }
                }
            }
        }
        Ok(start..end)
    }
}

/// 只有真实代理项内部端点属于边界错误；映射读取/回收失败保留其原始typed cause。
fn scalar_endpoint_error(error: MarkdownDocumentBufferError) -> BidiError {
    match error {
        MarkdownDocumentBufferError::NotUnicodeBoundary => BidiError::InvalidScalarBoundary,
        other => BidiError::Source(other),
    }
}

#[derive(Clone, Copy)]
struct Text {
    byte: u32,
    bracket: u32,
    pair_end: u32,
    pair_start_run: u32,
    pair_end_run: u32,
    original: BidiClass,
    processing: BidiClass,
    level: u8,
    flags: u8,
}

impl Text {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Text, id)?;
        Ok(Self {
            byte: read_u32(&bytes, 0),
            bracket: read_u32(&bytes, 4),
            pair_end: read_u32(&bytes, 8),
            pair_start_run: read_u32(&bytes, 12),
            pair_end_run: read_u32(&bytes, 16),
            original: decode_class(bytes[20])?,
            processing: decode_class(bytes[21])?,
            level: bytes[22],
            flags: bytes[23],
        })
    }

    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        for (offset, value) in [
            (0, self.byte),
            (4, self.bracket),
            (8, self.pair_end),
            (12, self.pair_start_run),
            (16, self.pair_end_run),
        ] {
            write_u32(&mut bytes, offset, value);
        }
        bytes[20] = encode_class(self.original);
        bytes[21] = encode_class(self.processing);
        bytes[22] = self.level;
        bytes[23] = self.flags;
        store.write(Table::Text, id, bytes)
    }

    fn continuation(self) -> bool {
        self.flags & 2 != 0
    }
    fn scalar_len(self) -> Result<u32, BidiError> {
        if self.continuation() {
            Err(BidiError::CorruptMetadata)
        } else {
            Ok(if self.flags & 1 != 0 { 2 } else { 1 })
        }
    }
}

#[derive(Clone, Copy)]
struct Paragraph {
    start: u32,
    end: u32,
    base: u8,
}

impl Paragraph {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Paragraph, id)?;
        Ok(Self {
            start: read_u32(&bytes, 0),
            end: read_u32(&bytes, 4),
            base: bytes[8],
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        write_u32(&mut bytes, 0, self.start);
        write_u32(&mut bytes, 4, self.end);
        bytes[8] = self.base;
        store.write(Table::Paragraph, id, bytes)
    }
}

#[derive(Clone, Copy)]
struct Run {
    start: u32,
    end: u32,
    previous: u32,
    next: u32,
}

impl Run {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Run, id)?;
        Ok(Self {
            start: read_u32(&bytes, 0),
            end: read_u32(&bytes, 4),
            previous: read_u32(&bytes, 8),
            next: read_u32(&bytes, 12),
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        for (offset, value) in [
            (0, self.start),
            (4, self.end),
            (8, self.previous),
            (12, self.next),
        ] {
            write_u32(&mut bytes, offset, value);
        }
        store.write(Table::Run, id, bytes)
    }
}

#[derive(Clone, Copy)]
struct Sequence {
    first: u32,
    last: u32,
    sos: BidiClass,
    eos: BidiClass,
}

impl Sequence {
    fn read(store: &mut Store, id: u32) -> Result<Self, BidiError> {
        let bytes = store.read(Table::Sequence, id)?;
        Ok(Self {
            first: read_u32(&bytes, 0),
            last: read_u32(&bytes, 4),
            sos: decode_class(bytes[8])?,
            eos: decode_class(bytes[9])?,
        })
    }
    fn write(self, store: &mut Store, id: u32) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        write_u32(&mut bytes, 0, self.first);
        write_u32(&mut bytes, 4, self.last);
        bytes[8] = encode_class(self.sos);
        bytes[9] = encode_class(self.eos);
        store.write(Table::Sequence, id, bytes)
    }
}

fn direction(level: u8) -> BidiClass {
    if level & 1 == 0 {
        BidiClass::L
    } else {
        BidiClass::R
    }
}
fn removed(class: BidiClass) -> bool {
    use BidiClass::*;
    matches!(class, RLE | LRE | RLO | LRO | PDF | BN)
}

// 编码是本模块固定格式，不依赖第三方enum的未承诺discriminant或unsafe转型。
fn encode_class(class: BidiClass) -> u8 {
    use BidiClass::*;
    match class {
        AL => 0,
        AN => 1,
        B => 2,
        BN => 3,
        CS => 4,
        EN => 5,
        ES => 6,
        ET => 7,
        FSI => 8,
        L => 9,
        LRE => 10,
        LRI => 11,
        LRO => 12,
        NSM => 13,
        ON => 14,
        PDF => 15,
        PDI => 16,
        R => 17,
        RLE => 18,
        RLI => 19,
        RLO => 20,
        S => 21,
        WS => 22,
    }
}
fn decode_class(value: u8) -> Result<BidiClass, BidiError> {
    use BidiClass::*;
    Ok(match value {
        0 => AL,
        1 => AN,
        2 => B,
        3 => BN,
        4 => CS,
        5 => EN,
        6 => ES,
        7 => ET,
        8 => FSI,
        9 => L,
        10 => LRE,
        11 => LRI,
        12 => LRO,
        13 => NSM,
        14 => ON,
        15 => PDF,
        16 => PDI,
        17 => R,
        18 => RLE,
        19 => RLI,
        20 => RLO,
        21 => S,
        22 => WS,
        _ => return Err(BidiError::CorruptMetadata),
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod unicode_data_tests;
