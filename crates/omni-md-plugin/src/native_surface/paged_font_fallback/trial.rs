//! 一个原试次的持久worker状态；只有输入和逐字形检查可分量子，shape仍是原同步完整pass。
use omni_markdown_core::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownShapingBudget, MarkdownShapingJob,
    MarkdownShapingProfile, MarkdownShapingResourceObserver,
};

use super::*;

/// 每步最多检查256条真实输出；上限不改变完整GCB或最终缺字结论。
pub const NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS: usize = 256;

/// 三类工作独立计费；零预算只让对应阶段等待，仍检查原Source和取消位。
#[derive(Clone, Copy, Debug)]
pub struct NativeMarkdownFontTrialBudget {
    pub input: MarkdownShapingBudget,
    pub glyphs: usize,
    /// 每步至多调用一次原同步shape；1不代表vendor pass能在一个时间量子内返回。
    pub shape_calls: usize,
}
impl Default for NativeMarkdownFontTrialBudget {
    fn default() -> Self {
        Self {
            input: MarkdownShapingBudget::default(),
            glyphs: NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS,
            shape_calls: 1,
        }
    }
}

/// 只有Finished允许消费完整结果；输入结束或shape返回不能提前代表已检查全部glyph。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownFontTrialStatus {
    Ingesting,
    AwaitingShape,
    CheckingGlyphs,
    Finished,
}

/// Core失败时不返回该输入步已读计数；显式保留有限上界，不能把未知工作伪报成零。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownFontTrialInputProgress {
    NotAttempted,
    Counted {
        bytes: usize,
        scalars: usize,
    },
    Failed {
        byte_limit: usize,
        scalar_limit: usize,
    },
}

/// 本步只推进一个阶段；glyphs计成功取得的原记录，shape_calls计实际原API调用。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeMarkdownFontTrialStep {
    pub status: NativeMarkdownFontTrialStatus,
    pub input: NativeMarkdownFontTrialInputProgress,
    pub glyphs: usize,
    pub shape_calls: usize,
}

/// 只在原worker内部构造并保留原非Send Job；跨线程传送的仍只有Trial packet和最终结果。
pub struct NativeMarkdownFontTrialWorker {
    #[cfg(feature = "native-paged-compose-probe")]
    probe: Option<omni_markdown_core::MarkdownLineFontProbe>,
    identity: Arc<()>,
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    backing: Option<MarkdownOriginalBackingObserver>,
    job: Option<MarkdownShapingJob>,
    resources: Option<MarkdownShapingResourceObserver>,
    status: NativeMarkdownFontTrialStatus,
    checked: usize,
    facts: NativeMarkdownFontGlyphFacts,
    result: Option<NativeMarkdownFontTrialResult>,
}

impl NativeMarkdownFontTrial {
    /// 消费原packet后才在当前线程构造Job；主线程或构造失败成为同nonce的终态错误。
    /// 失败没有伪造的零资源收据，调用方仍通过into_result取得原错误。
    #[must_use]
    pub fn into_worker(self) -> NativeMarkdownFontTrialWorker {
        let Self {
            #[cfg(feature = "native-paged-compose-probe")]
            probe,
            source,
            source_authority,
            font,
            font_authority,
            range,
            options,
            cancellation,
            identity,
        } = self;
        let mut worker = NativeMarkdownFontTrialWorker {
            #[cfg(feature = "native-paged-compose-probe")]
            probe,
            identity,
            source: source_authority,
            font: font_authority,
            cancellation,
            backing: source.original_backing_observer(),
            job: None,
            resources: None,
            status: NativeMarkdownFontTrialStatus::Ingesting,
            checked: 0,
            facts: NativeMarkdownFontGlyphFacts {
                glyph_count: 0,
                missing_glyphs: 0,
                flags_union: 0,
                input_scalars: 0,
                source_bytes_read: 0,
                source_borrow_max_bytes: 0,
            },
            result: None,
        };
        let created = (|| {
            source.byte_offset_for_utf16(range.start)?;
            #[cfg(feature = "native-paged-compose-probe")]
            if let Some(probe) = &worker.probe {
                probe.validate().map_err(compose::error)?;
            }
            if MainThreadMarker::new().is_some() {
                return Err(NativeMarkdownFontFallbackError::MainThreadShape);
            } // 原Source真实错误优先于线程调度；不让长GCB进入主线程同步算法。
            Ok(MarkdownShapingJob::new_with_cancellation(
                source,
                &worker.source,
                font,
                &worker.font,
                range,
                options,
                worker.cancellation.clone(),
            )?)
        })();
        match created {
            Ok(job) => {
                worker.resources = Some(job.resource_observer());
                worker.job = Some(job);
            }
            Err(error) => worker.finish(Err(error)),
        } // 只有实际构造成功才发布原observer；失败的None保留“未取得收据”事实。
        worker
    }

    /// 同步兼容入口只驱动同一个状态机；调用方仍须在真实worker执行并join。
    /// 此方法不承诺长GCB的shape可暂停，也没有第二套缺字或候选选择算法。
    pub fn run(self) -> NativeMarkdownFontTrialResult {
        let mut worker = self.into_worker();
        while worker.status() != NativeMarkdownFontTrialStatus::Finished {
            worker.step(NativeMarkdownFontTrialBudget::default());
        } // 默认预算在各阶段都有实际进度；没有后台轮询或自动新建线程。
        worker.into_result()
    }
}

impl NativeMarkdownFontTrialWorker {
    /// 只读阶段不升级权限；Finished仍须消费最终结果并由原fallback重核试次身份。
    #[must_use]
    pub fn status(&self) -> NativeMarkdownFontTrialStatus {
        self.status
    }

    /// 原取消位没有正文、字体或Job强边；请求停止不等于已交还资源。
    #[must_use]
    pub fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }

    /// 原Job的纯原子观察者；构造失败的None不是零资源已通过。
    #[must_use]
    pub fn resource_observer(&self) -> Option<MarkdownShapingResourceObserver> {
        self.resources.clone()
    }

    /// 推进当前单一阶段；超额glyph或shape预算按固定上限消费，不扩大正文覆盖。
    pub fn step(&mut self, budget: NativeMarkdownFontTrialBudget) -> NativeMarkdownFontTrialStep {
        let mut step = NativeMarkdownFontTrialStep {
            status: self.status,
            input: NativeMarkdownFontTrialInputProgress::NotAttempted,
            glyphs: 0,
            shape_calls: 0,
        };
        if self.status == NativeMarkdownFontTrialStatus::Finished {
            self.refresh_terminal_error();
            return step;
        } // 终态不重跑输入或shape；原共享Source后来失败仍不能交出旧成功facts。
        match self.advance(budget, &mut step) {
            Ok(true) => self.finish(Ok(self.facts)),
            Ok(false) => {} // 有效零预算或阶段转换保留同一Job，不签不完整缺字结论。
            Err(error) => self.finish(Err(error)),
        }
        step.status = self.status;
        step
    }

    fn advance(
        &mut self,
        budget: NativeMarkdownFontTrialBudget,
        step: &mut NativeMarkdownFontTrialStep,
    ) -> Result<bool> {
        #[cfg(feature = "native-paged-compose-probe")]
        if let Some(probe) = &self.probe {
            probe.validate().map_err(compose::error)?;
        } // 零预算也核原Source及原Style scope；原响应已消费时不能继续旧字体试次。
        let job = self
            .job
            .as_mut()
            .ok_or(NativeMarkdownFontFallbackError::InvalidState)?;
        job.profile()?; // 每步包括零预算都走原Source/Font/分页/取消门，不以阶段缓存绕过健康。
        match self.status {
            NativeMarkdownFontTrialStatus::Ingesting => {
                if budget.input.bytes == 0 || budget.input.scalars == 0 {
                    return Ok(false);
                }
                // 原Core输入失败不提供部分计数；先登记真实允许上界，成功才替换成准确计数。
                step.input = NativeMarkdownFontTrialInputProgress::Failed {
                    byte_limit: budget
                        .input
                        .bytes
                        .min(budget.input.scalars)
                        .min(MARKDOWN_LAYOUT_SOURCE_BYTES),
                    scalar_limit: budget
                        .input
                        .scalars
                        .min(budget.input.bytes)
                        .min(MARKDOWN_LAYOUT_SOURCE_BYTES),
                };
                let actual = job.step(&self.source, &self.font, budget.input)?;
                step.input = NativeMarkdownFontTrialInputProgress::Counted {
                    bytes: actual.bytes,
                    scalars: actual.scalars,
                };
                if actual.status == omni_markdown_core::MarkdownShapingStatus::AwaitingShape {
                    self.status = NativeMarkdownFontTrialStatus::AwaitingShape;
                }
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::AwaitingShape => {
                if budget.shape_calls == 0 {
                    return Ok(false);
                }
                step.shape_calls = 1;
                job.shape(&self.source, &self.font)?; // 原完整同步vendor pass，不声称时间量子有界。
                let profile = job.profile()?;
                self.facts = facts(profile);
                self.status = NativeMarkdownFontTrialStatus::CheckingGlyphs;
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::CheckingGlyphs => {
                let remaining = self.facts.glyph_count - self.checked;
                let count = budget
                    .glyphs
                    .min(NATIVE_MARKDOWN_FONT_TRIAL_GLYPHS)
                    .min(remaining);
                for _ in 0..count {
                    let glyph = job
                        .glyph(&self.source, &self.font, self.checked)?
                        .ok_or(NativeMarkdownFontFallbackError::InvalidCluster)?;
                    // 只成功读取原记录才增加计数；任何后续失败都整次撤权，不发布已有前缀。
                    step.glyphs += 1;
                    self.checked += 1;
                    self.facts.missing_glyphs = self
                        .facts
                        .missing_glyphs
                        .checked_add(usize::from(glyph.glyph_id == 0))
                        .ok_or(NativeMarkdownFontFallbackError::Overflow)?;
                    self.facts.flags_union |= glyph.flags;
                }
                if self.checked == self.facts.glyph_count {
                    if job.glyph(&self.source, &self.font, self.checked)?.is_some() {
                        return Err(NativeMarkdownFontFallbackError::InvalidCluster);
                    } // 原EOF getter完整认证实际条数；零glyph也不能用猜测跳过此门。
                    return Ok(true);
                }
                Ok(false)
            }
            NativeMarkdownFontTrialStatus::Finished => {
                Err(NativeMarkdownFontFallbackError::InvalidState)
            }
        }
    }

    fn finish(&mut self, outcome: Result<NativeMarkdownFontGlyphFacts>) {
        drop(self.job.take()); // 真正交还分页数组和本Job的Source/Font强边后才读取终态收据。
        let resources = self
            .resources
            .as_ref()
            .map(MarkdownShapingResourceObserver::snapshot);
        let outcome = if matches!(
            &outcome,
            Err(NativeMarkdownFontFallbackError::Source(_))
                | Err(NativeMarkdownFontFallbackError::Shaping(
                    MarkdownShapingError::Source(_)
                ))
        ) {
            outcome // 原Source错误先于分页和取消；最后Drop的新Backing失败还会在下方再核。
        } else if let Some(error) = resources.and_then(|resources| resources.first_error) {
            Err(NativeMarkdownFontFallbackError::GlyphStoreIo(error))
        } else if resources.is_some_and(|resources| {
            resources.files_open != 0
                || resources.mappings_open != 0
                || resources.mapped_bytes != 0
                || resources.cleanup_failed
                || !resources.shaper_access_certified
        }) {
            Err(NativeMarkdownFontFallbackError::GlyphStoresNotReleased)
        } else {
            outcome
        };
        self.result = Some(NativeMarkdownFontTrialResult {
            identity: self.identity.clone(),
            outcome,
            resources,
            backing: self.backing.clone(),
            cancellation: self.cancellation.clone(),
        });
        self.status = NativeMarkdownFontTrialStatus::Finished;
        self.refresh_terminal_error();
    }

    fn refresh_terminal_error(&mut self) {
        #[cfg(feature = "native-paged-compose-probe")]
        let probe_error = self.probe.as_ref().and_then(|probe| probe.validate().err());
        let Some(result) = self.result.as_mut() else {
            return;
        };
        if let Some(errno) = self
            .backing
            .as_ref()
            .and_then(|backing| backing.first_errno())
        {
            result.outcome =
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        #[cfg(feature = "native-paged-compose-probe")]
        if let Some(cause) = probe_error
            && (matches!(
                &cause,
                omni_markdown_core::MarkdownLineComposeError::Source(_)
            ) || result.outcome.is_ok())
        {
            result.outcome = Err(compose::error(cause));
        } // 草稿正文撤权同属原Source失败，必须优先于稍后的取消；弱scope失效不覆盖已有分页首错。
        if result.outcome.is_ok() && self.cancellation.requested() {
            result.outcome = Err(MarkdownShapingError::Cancelled.into());
        } // 终态先核Source/原票，再核取消；没有任何旧成功可以越过真实源健康门。
    }

    /// 消费完整终态；若调用者提前停止，先请求原取消并实际归还Job，不把前缀视为成功。
    pub fn into_result(mut self) -> NativeMarkdownFontTrialResult {
        if self.status != NativeMarkdownFontTrialStatus::Finished {
            self.cancellation.request();
            let outcome = match self.job.as_mut() {
                Some(job) => match job.profile() {
                    Err(error) => Err(error.into()),
                    Ok(_) => Err(MarkdownShapingError::Cancelled.into()),
                },
                None => Err(NativeMarkdownFontFallbackError::InvalidState),
            };
            self.finish(outcome);
        }
        self.refresh_terminal_error();
        self.result.take().expect("终态试次必须保有唯一结果")
    }
}

impl Drop for NativeMarkdownFontTrialWorker {
    fn drop(&mut self) {
        if self.status != NativeMarkdownFontTrialStatus::Finished {
            self.cancellation.request();
        }
        drop(self.job.take()); // 未消费结果的早退也真实释放；观察者独立存活但不保业务资源。
    }
}

fn facts(profile: MarkdownShapingProfile) -> NativeMarkdownFontGlyphFacts {
    NativeMarkdownFontGlyphFacts {
        glyph_count: profile.glyph_count,
        missing_glyphs: 0,
        flags_union: 0,
        input_scalars: profile.input_scalars,
        source_bytes_read: profile.source_bytes_read,
        source_borrow_max_bytes: profile.source_borrow_max_bytes,
    }
}
