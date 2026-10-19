//! 测量只读真实字形来源的封闭借用；重排另由认证producer提供，不把存储目录冒充原Job。
use super::*;

/// 外部crate不能实现此接口。crate内也只允许原Job或实际Store认证Directory的适配器实现。
pub(crate) mod sealed {
    pub trait Sealed {}
}
#[derive(Clone)]
pub(crate) enum GlyphProviderOrigin {
    Job(Weak<()>),
    #[cfg(feature = "paged-line-compose")]
    Stored {
        store: Weak<()>,
        origin: u64,
    },
}
impl PartialEq for GlyphProviderOrigin {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Job(left), Self::Job(right)) => Weak::ptr_eq(left, right),
            #[cfg(feature = "paged-line-compose")]
            (
                Self::Stored {
                    store: left,
                    origin: a,
                },
                Self::Stored {
                    store: right,
                    origin: b,
                },
            ) => *a == *b && Weak::ptr_eq(left, right),
            #[cfg(feature = "paged-line-compose")]
            _ => false, // 相同文本/范围不能让Job身份和Store签入身份互相代替。
        }
    }
}
impl Eq for GlyphProviderOrigin {}

/// 由require成功后返回的实际事实；构造这些字段本身不能替代Source/Font/EOF门。
pub(crate) struct GlyphProviderFacts {
    pub origin: GlyphProviderOrigin,
    pub source_utf16: Range<usize>,
    pub glyphs: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
}
pub(crate) trait GlyphProvider: sealed::Sealed {
    /// 只读构造时已绑定的opaque身份，不推进IO或先访问外来Font；原Source错误仍由Measure先查。
    fn origin(&self) -> GlyphProviderOrigin;
    /// 每次重新检查原Source健康、实际Font权限、取消和完整EOF；不返回后台借用或强owner。
    fn require(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<GlyphProviderFacts>;
    /// 返回原逻辑cluster；未知依赖仍Pending，完整范围内部缺口必须失败。
    fn cluster(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer>;
    /// 只由真实Store适配器从最新完整LineEnd读取；Job或普通范围没有已提交行权限。
    #[cfg(feature = "paged-line-compose")]
    fn committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        _selected: &SelectedMeasureSelection,
    ) -> Result<CommittedLineStart> {
        self.require(source, font)?;
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    }
    /// 消费下一行票之前再核同一最新LineEnd，不能把旧票用于回退重开或另一个Store。
    #[cfg(feature = "paged-line-compose")]
    fn require_committed_line_start(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        _committed: &CommittedLineStart,
    ) -> Result<()> {
        self.require(source, font)?;
        Err(MarkdownSoftLineMeasureError::InvalidRequest)
    }
    /// 仅真实frontier child或Store已签入同origin目录可以提供；此末端不是当前glyphReady范围。
    #[cfg(feature = "paged-shaping-frontier")]
    fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        self.require(source, font)?;
        Ok(None) // 旧Job和没有原producer证书的目录不能从公开范围补签能力。
    }
}

/// 重排能力只由实际原Job或已认证Store目录提供，不能用公开Range/Choice自行实现。
/// 子范围来自原selector；实现必须保留原语言、features、字体和全局邻接context，
/// BOT/EOT只在对应原item端点继承，真实软行不会获得新的物理段落标记。
#[cfg(feature = "paged-soft-line-resolve")]
pub(crate) trait ResolutionProvider: GlyphProvider {
    fn shaping_cancellation(&self) -> crate::MarkdownShapingCancellation;
    fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownShapingJob, crate::MarkdownSoftLineResolveError>;
}

/// 公开旧API只借原Job和已有索引；构造时不创建第二索引或复制glyph数据。
pub(crate) struct JobGlyphProvider<'a> {
    job: &'a mut MarkdownShapingJob,
    clusters: Option<&'a mut MarkdownGlyphClusterIndex>,
}
impl<'a> JobGlyphProvider<'a> {
    pub(crate) fn new(
        job: &'a mut MarkdownShapingJob,
        clusters: Option<&'a mut MarkdownGlyphClusterIndex>,
    ) -> Self {
        Self { job, clusters }
    }
}
impl sealed::Sealed for JobGlyphProvider<'_> {}
impl GlyphProvider for JobGlyphProvider<'_> {
    fn origin(&self) -> GlyphProviderOrigin {
        GlyphProviderOrigin::Job(self.job.index_identity())
    }
    fn require(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<GlyphProviderFacts> {
        let profile = self.job.profile()?;
        if self.job.glyph(source, font, profile.glyph_count)?.is_some() {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        } // 真EOF getter核Ready和两张原票，不能用profile长度冒充部分Buffer已完成。
        Ok(GlyphProviderFacts {
            origin: self.origin(),
            source_utf16: self.job.source_range(),
            glyphs: profile.glyph_count,
            generation: profile.source_generation,
            direction: self.job.direction(),
        })
    }
    fn cluster(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        self.require(source, font)?;
        let value = self
            .clusters
            .as_mut()
            .ok_or(MarkdownSoftLineMeasureError::InvalidRequest)?
            .containing(self.job, offset)?;
        self.require(source, font)?;
        Ok(value) // 索引真实IO之后仍核同原Job；失败不发布从旧页取出的Copy记录。
    }
    #[cfg(feature = "paged-shaping-frontier")]
    fn frontier_word_end(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Option<usize>> {
        Ok(self.job.frontier_word_end(source, font)?)
    }
}

#[cfg(feature = "paged-soft-line-resolve")]
impl ResolutionProvider for JobGlyphProvider<'_> {
    fn shaping_cancellation(&self) -> crate::MarkdownShapingCancellation {
        self.job.cancellation()
    }
    fn reshape_subrange(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
    ) -> std::result::Result<MarkdownShapingJob, crate::MarkdownSoftLineResolveError> {
        Ok(self.job.reshape_subrange(source, font, range)?)
    } // 真正原Job克隆自己已验证的样式；不重建原output身份或提前读取尚未shape的tail。
}

impl GlyphProviderOrigin {
    pub(super) fn same_provider_family(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Job(_), Self::Job(_)) => true,
            #[cfg(feature = "paged-line-compose")]
            (Self::Stored { store: left, .. }, Self::Stored { store: right, .. }) => {
                Weak::ptr_eq(left, right)
            }
            #[cfg(feature = "paged-line-compose")]
            _ => false,
        } // 每项origin可以变化，但原Store不能被同源的第二个文件或Job替代。
    }
}

/// 唯一原Measure的独占借用就是交接票；不保前项provider，因此不会重叠借同Store。
/// 丢弃未消费令牌只结束借用，原NeedNextItem不变；没有隐式推进、重试或成功收据。
pub(crate) struct PreparedMeasureContinuation<'a> {
    measure: &'a mut MarkdownSoftLineMeasureJob,
}
impl<'a> PreparedMeasureContinuation<'a> {
    pub(super) fn new(measure: &'a mut MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn continue_with(
        self,
        next: &mut dyn GlyphProvider,
        font: &MarkdownLayoutFontAuthority,
        breaks: &mut MarkdownLayoutBreakJob,
    ) -> Result<()> {
        self.measure.continue_prepared(next, font, breaks)
    }
}

/// 只携带整行弱身份与实际撤权/取消标量；没有Source、字体、文件或Job强边。
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MeasuredLineScope {
    pub owner: Weak<()>,
    pub alive: Arc<AtomicBool>,
    pub cancellation: MarkdownSoftLineMeasureCancellation,
}

/// producer本次签入所需的固定事实；真正权限仍是不可构造的permit和实际provider核验。
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MeasuredItemFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub source_utf16: Range<usize>,
    pub measured_utf16: Range<usize>,
    pub glyphs: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
    pub line: MeasuredLineScope,
}

/// 短暂借住原Measure，不能Clone或落盘恢复；同一Store的旧借用结束后仍可借回核验。
#[cfg(feature = "paged-safe-glyph-slices")]
pub(crate) struct MeasuredItemPermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl<'a> MeasuredItemPermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<MeasuredItemFacts> {
        self.measure.guard_provider(provider)?;
        self.measure.require_next_item()?;
        Ok(MeasuredItemFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            origin: self.measure.origin.clone(),
            source_utf16: self.measure.item.clone(),
            measured_utf16: self.measure.start.max(self.measure.item.start)..self.measure.item.end,
            glyphs: self.measure.glyph_count,
            generation: self.measure.generation,
            direction: self.measure.direction,
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // 事实只描述原真实消费阶段；不签最终行尾、GCB安全或Native绘制Ready。
    }
}

/// 当前provider只是原选择的权限锚点；selected可能落在更早item，不能把current当整行覆盖。
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedMeasureFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub current: GlyphProviderFacts,
    pub selected: SelectedMeasureSelection,
    pub line: MeasuredLineScope,
}

/// 真实自然机会与紧急GCB选择分别保留种类；紧急切片绝不伪造Allowed/Mandatory。
#[cfg(feature = "paged-line-compose")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SelectedMeasureKind {
    Natural(MarkdownLineBreakBoundary),
    #[cfg(feature = "paged-shaping-frontier")]
    Emergency {
        overflowing_grapheme: bool,
    },
}

/// 仅原Measure内部选择可生成这些事实；Store仍须核原整行票和真实历史字形覆盖。
#[cfg(feature = "paged-line-compose")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedMeasureSelection {
    pub source_utf16: Range<usize>,
    pub x_advance_26_6: i64,
    pub boundary_flags: u32,
    pub kind: SelectedMeasureKind,
}

/// 原Store独立读取的固定提交事实；不保文件/正文，具体记录必须由同Store再次核验。
#[cfg(feature = "paged-line-compose")]
#[derive(Clone)]
pub(crate) struct CommittedLineStart {
    pub source_utf16: Range<usize>,
    pub generation: u64,
    pub record: usize,
    pub ordinal: usize,
    pub kind: SelectedMeasureKind,
    pub x_advance_26_6: i64,
}

/// 非Clone单次票。旧Measure析构后可以保留固定事实，但不保Source、Font、Job或FD。
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedLineStart {
    source: MarkdownLayoutTextAuthority,
    family: GlyphProviderOrigin,
    cancellation: MarkdownSoftLineMeasureCancellation,
    committed: CommittedLineStart,
}

#[cfg(feature = "paged-line-compose")]
impl SelectedLineStart {
    pub(super) fn require(
        &self,
        source: &MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        provider: &mut dyn GlyphProvider,
        start: usize,
        cancellation: &MarkdownSoftLineMeasureCancellation,
    ) -> Result<()> {
        source.health()?; // 即使票过期、外来Store或取消，原实际源的首errno仍先报告。
        if authority != &self.source
            || !source.permits(&self.source)
            || source.generation() != self.committed.generation
            || start != self.committed.source_utf16.end
            || !self.cancellation.same_request(cancellation)
        {
            return Err(MarkdownSoftLineMeasureError::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        let facts = provider.require(authority, font)?;
        if !self.family.same_provider_family(&facts.origin)
            || facts.generation != self.committed.generation
        {
            return Err(MarkdownSoftLineMeasureError::JobMismatch);
        }
        // Store须真实读取最新LineEnd的原record、kind、range与ordinal；源相同或
        // 记录地址相同都不够，旧票在后来LineEnd提交后也不能重新打开旧行首。
        let result = provider.require_committed_line_start(authority, font, &self.committed);
        source.health()?;
        result?;
        if self.cancellation.is_cancelled() {
            return Err(MarkdownSoftLineMeasureError::Cancelled);
        }
        Ok(())
    }
}

/// 不可Clone、只在原Measure借用期有效。producer还须以同Store真实历史证明覆盖与字体，
/// 再核实际GCB、cluster边界及L1/L2；本票绝不把公开Range升级为任意拼行许可。
#[cfg(feature = "paged-line-compose")]
pub(crate) struct SelectedMeasurePermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
#[cfg(feature = "paged-line-compose")]
impl<'a> SelectedMeasurePermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<SelectedMeasureFacts> {
        self.measure.guard_provider(provider)?;
        let selected = self.measure.require_selected()?;
        Ok(SelectedMeasureFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            current: GlyphProviderFacts {
                origin: self.measure.origin.clone(),
                source_utf16: self.measure.item.clone(),
                glyphs: self.measure.glyph_count,
                generation: self.measure.generation,
                direction: self.measure.direction,
            },
            selected,
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // 只复制固定元数据；需要读取历史字形时仍借唯一Store，不能保活旧Job或Font。
    }
    /// Store完成真实LineEnd并最终复核本permit后才调用；同一选择只能签出一张下一行票。
    pub(crate) fn next_start(&self, provider: &mut dyn GlyphProvider) -> Result<SelectedLineStart> {
        let selected = self.require(provider)?;
        if self.measure.start_issued.get()
            || !matches!(selected.current.origin, GlyphProviderOrigin::Stored { .. })
        {
            return Err(MarkdownSoftLineMeasureError::InvalidRequest);
        }
        let committed =
            provider.committed_line_start(&selected.source, &selected.font, &selected.selected);
        self.measure.require_live_line()?;
        let committed = committed?;
        if committed.source_utf16 != selected.selected.source_utf16
            || committed.generation != selected.current.generation
            || committed.kind != selected.selected.kind
            || committed.x_advance_26_6 != selected.selected.x_advance_26_6
        {
            return Err(MarkdownSoftLineMeasureError::InconsistentCoverage);
        }
        self.measure.start_issued.set(true);
        Ok(SelectedLineStart {
            source: selected.source,
            family: selected.current.origin,
            cancellation: self.measure.cancellation.clone(),
            committed,
        }) // 未完成/失败写入不给票；丢弃票只归还固定弱元数据，不改变旧immutable字形。
    }
}

/// 临时借住真正选中unsafe/emergency的原Measure；旧公开Choice不具有此构造权限。
/// 每次借回目录仍须核原origin，跨item候选不会因为同Source而重新签给current目录。
#[cfg(feature = "paged-line-compose")]
pub(crate) struct ResolutionMeasurePermit<'a> {
    measure: &'a MarkdownSoftLineMeasureJob,
}
/// 同一原选择的固定事实；没有Source/Font强owner，权限仍须借原permit重新核验。
#[cfg(feature = "paged-line-compose")]
pub(crate) struct ResolutionMeasureFacts {
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub origin: GlyphProviderOrigin,
    pub line: MeasuredLineScope,
}
#[cfg(feature = "paged-line-compose")]
impl<'a> ResolutionMeasurePermit<'a> {
    pub(super) fn new(measure: &'a MarkdownSoftLineMeasureJob) -> Self {
        Self { measure }
    }
    pub(crate) fn require(&self, provider: &mut dyn GlyphProvider) -> Result<()> {
        self.measure.guard_provider(provider)?;
        self.measure.require_resolution()?;
        self.measure.require_live_line()
    }
    pub(crate) fn seed(
        &self,
        provider: &mut dyn GlyphProvider,
    ) -> Result<MarkdownSoftLineResolveSeed> {
        self.require(provider)?;
        self.measure.resolve_seed_provider(provider)
    }
    pub(crate) fn facts(&self, provider: &mut dyn GlyphProvider) -> Result<ResolutionMeasureFacts> {
        self.require(provider)?;
        Ok(ResolutionMeasureFacts {
            source: self.measure.source.clone(),
            font: self.measure.font.clone(),
            origin: self.measure.origin.clone(),
            line: MeasuredLineScope {
                owner: Arc::downgrade(&self.measure.identity),
                alive: Arc::clone(&self.measure.line_alive),
                cancellation: self.measure.cancellation.clone(),
            },
        }) // 单一整行scope不会随已测item数积累Weak集合，也不能恢复另一个测量请求。
    }
    pub(crate) fn measure(&self) -> &MarkdownSoftLineMeasureJob {
        self.measure
    } // 仅供共用selector重新核原nonce，不允许构造另一Measure或改变内部起点。
}
