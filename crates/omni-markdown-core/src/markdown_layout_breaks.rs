//! 同一冻结源的字素与默认断行交集；查询让出配额，不用无界循环寻找下一机会。
//!
//! 这一步只认证断点。宽度选行、混合字体、unsafe-to-break重排、词典与Native几何
//! 仍由各自阶段完成，不能把默认Unicode机会当成已排好的可见软行。

use std::rc::{Rc, Weak};

use thiserror::Error;

use crate::{
    MarkdownGraphemeAnswer as GraphemeAnswer, MarkdownGraphemeBudget, MarkdownGraphemeError,
    MarkdownGraphemeJob, MarkdownGraphemeQuery, MarkdownGraphemeResourceObserver,
    MarkdownGraphemeResourceSnapshot, MarkdownGraphemeUnit, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLineBreakAnswer as LineAnswer, MarkdownLineBreakBoundary,
    MarkdownLineBreakBudget, MarkdownLineBreakError, MarkdownLineBreakJob, MarkdownLineBreakQuery,
    MarkdownLineBreakResourceObserver, MarkdownLineBreakResourceSnapshot, MarkdownLineBreakUnit,
};

#[derive(Debug, Error)]
pub enum MarkdownLayoutBreakError {
    #[error("断点源权限不匹配")]
    SourceMismatch,
    #[error("断点查询不属于原任务")]
    CursorMismatch,
    #[error("断点配额或范围无效")]
    InvalidRequest,
    #[error("断点任务已撤权或取消")]
    Poisoned,
    #[error("字素与断行的同一源坐标不一致")]
    InconsistentCoordinates,
    #[error("断点字素阶段失败")]
    Grapheme(#[from] MarkdownGraphemeError),
    #[error("断点默认断行阶段失败")]
    Line(#[from] MarkdownLineBreakError),
    #[error("断点原冻结源已经不可用")]
    Source(#[from] crate::MarkdownDocumentBufferError),
}
type Result<T> = std::result::Result<T, MarkdownLayoutBreakError>;

/// 一个查询回合至多核验64个候选；更大的调用方预算不能把本API变成整文扫描循环。
pub const MARKDOWN_LAYOUT_BREAK_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakQuery {
    At,
    Previous,
    Next,
}

/// 可克隆的有限查询状态只保弱owner与坐标；不能保活源、索引、字体或Native控件。
#[derive(Clone)]
pub struct MarkdownLayoutBreakCursor {
    owner: Weak<()>,
    unit: MarkdownLayoutBreakUnit,
    query: MarkdownLayoutBreakQuery,
    at: usize,
    candidate: Option<MarkdownLineBreakBoundary>,
    completed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLayoutBreakAnswer {
    /// 需要实际推进同一源的算法前缀；未知区域不能解释为空。
    PendingSource,
    /// 本次候选数已用完；同一cursor保留精确下一步，不重采新的源或查询意图。
    Yielded,
    Known(Option<MarkdownLineBreakBoundary>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLayoutBreakStep {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub ready: bool,
    pub needs_dictionary: bool,
}

#[derive(Clone)]
pub struct MarkdownLayoutBreakResourceObserver {
    grapheme: MarkdownGraphemeResourceObserver,
    line: MarkdownLineBreakResourceObserver,
}
impl MarkdownLayoutBreakResourceObserver {
    #[must_use]
    pub fn snapshots(
        &self,
    ) -> (
        MarkdownGraphemeResourceSnapshot,
        MarkdownLineBreakResourceSnapshot,
    ) {
        (self.grapheme.snapshot(), self.line.snapshot())
    }
}

struct Payload {
    source: MarkdownLayoutTextSource,
    grapheme: MarkdownGraphemeJob,
    line: MarkdownLineBreakJob,
    // 端点票只保弱scope；任意子索引失败、cancel或Drop同时撤销已借出的坐标票。
    #[cfg(feature = "paged-shaping-frontier")]
    endpoint_scope: Rc<()>,
}

/// 两个算法各自拥有一页匿名索引；只有同一个冻结source capsule可以同时授权它们。
pub struct MarkdownLayoutBreakJob {
    owner: Rc<()>,
    payload: Option<Payload>,
    authority: MarkdownLayoutTextAuthority,
    maximum: [usize; 2],
    grapheme_first: bool,
    resources: MarkdownLayoutBreakResourceObserver,
    // 仅实际raw LB查询的未知源留下调度需求；GCB待决和候选配额让出不能冒充正文需求。
    #[cfg(feature = "paged-shaping-frontier")]
    line_source_demand: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    grapheme_source_demand: bool,
}

impl MarkdownLayoutBreakJob {
    /// 原两个算法共享同 Source 的真实段落 proof，各自原游标只跳过明确隐藏 body。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn install_fold_plan(
        &mut self,
        plan: crate::markdown_layout_source::folds::FoldPlan,
    ) -> Result<()> {
        let authority = self.authority.clone();
        self.require_source(&authority)?;
        let result = (|| {
            let payload = self
                .payload
                .as_mut()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            payload.grapheme.install_fold_plan(plan.clone())?;
            payload.line.install_fold_plan(plan)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.poison(error),
        }
    }

    /// 表格流只分享原结构行目录；两个 Unicode 游标仍保各自真实状态和原配额。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(crate) fn install_table_plan(
        &mut self,
        plan: crate::markdown_layout_source::table_rows::TablePlan,
    ) -> Result<()> {
        let authority = self.authority.clone();
        self.require_source(&authority)?;
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.grapheme.install_table_plan(plan.clone())?;
        payload.line.install_table_plan(plan)?;
        Ok(())
    }

    /// 只观察两个原 machine 的真实 canonical 进度，不以需求位置伪造已消费。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(crate) fn table_passed(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        end: usize,
    ) -> Result<bool> {
        self.require_source(authority)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        Ok(payload.grapheme.table_passed(end) && payload.line.table_passed(end))
    }

    /// 迁移者必须明确区分原全源 ordinal 与本次显式可见序列；两者不互相补签。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn grapheme_ordinal_domain(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<crate::MarkdownGraphemeOrdinalDomain> {
        self.require_source(authority)?;
        Ok(self
            .payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .ordinal_domain())
    }

    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Self> {
        if !source.permits(authority) {
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        source.health()?;
        let maximum = [source.byte_len(), source.utf16_len()];
        let grapheme = MarkdownGraphemeJob::new(source.clone())?;
        let line = MarkdownLineBreakJob::new(source.clone())?;
        let resources = MarkdownLayoutBreakResourceObserver {
            grapheme: grapheme.resource_observer(),
            line: line.resource_observer(),
        };
        Ok(Self {
            owner: Rc::new(()),
            payload: Some(Payload {
                source,
                grapheme,
                line,
                #[cfg(feature = "paged-shaping-frontier")]
                endpoint_scope: Rc::new(()),
            }),
            authority: authority.clone(),
            maximum,
            grapheme_first: true,
            resources,
            #[cfg(feature = "paged-shaping-frontier")]
            line_source_demand: false,
            #[cfg(feature = "paged-shaping-frontier")]
            grapheme_source_demand: false,
        })
    }

    /// 消费原Source权限前先核真实健康；同正文或代次的另一次capture不能冒充本任务。
    ///
    /// # Errors
    /// 真实源错误优先于身份不符；任何失败均沿原poison路径释放两个索引和Source强边，
    /// 不重新capture、不恢复旧前缀，也不授予Native布局或输入权限。
    pub fn require_source(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        let result = (|| {
            let payload = self
                .payload
                .as_ref()
                .ok_or(MarkdownLayoutBreakError::Poisoned)?;
            payload.source.health()?;
            if authority != &self.authority || !payload.source.permits(authority) {
                return Err(MarkdownLayoutBreakError::SourceMismatch);
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.poison(error), // 与step/query共享真正的失败关闭边界。
        }
    }

    /// 软行紧急候选复用本任务的原GCB索引；不另建字素job或把未知后继当EOF。
    #[cfg(feature = "paged-soft-line-resolve")]
    pub(crate) fn grapheme_query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<GraphemeAnswer> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .query(authority, MarkdownGraphemeUnit::Utf16, offset, query);
        self.require_source(authority)?; // 子索引IO完成后的同源健康门保留真实错误优先级。
        match result {
            Ok(answer) => {
                #[cfg(feature = "paged-shaping-frontier")]
                if answer == GraphemeAnswer::Pending {
                    self.grapheme_source_demand = true;
                }
                Ok(answer)
            }
            Err(error) => self.poison(error.into()),
        }
    }

    /// 同一Ready GCB表的有限record数，不把当前前缀或另一Source的表交给行输出。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn ready_grapheme_count(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Option<usize>> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .ready_boundary_count(authority);
        self.require_source(authority)?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error.into()),
        }
    }

    /// 一次迁移≤256条实际byte/UTF16坐标；false保持未知，Err丢弃完整batch并撤原owner。
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_ready_graphemes(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [crate::MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        self.require_source(authority)?;
        let result = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .grapheme
            .read_ready_boundaries(authority, range, output);
        self.require_source(authority)?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error.into()),
        }
    }

    fn poison<T>(&mut self, error: MarkdownLayoutBreakError) -> Result<T> {
        if matches!(
            &error,
            MarkdownLayoutBreakError::Grapheme(crate::MarkdownGraphemeError::FoldedRange)
                | MarkdownLayoutBreakError::Line(crate::MarkdownLineBreakError::FoldedRange)
        ) {
            return Err(error);
        } // 明确隐藏是权限域差异，不是索引损坏；保留仍健康的可见 worker。
        drop(self.payload.take()); // 原索引和source强边真实Drop，外部cursor/observer不保活资源。
        Err(error)
    }

    /// 两个算法共用本次总配额，轮换先行方；不能把每个算法各用一份预算说成总预算。
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        let result = self.advance(authority, budget);
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.poison(error),
        }
    }

    fn advance(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLayoutBreakStep> {
        // 子job仍分别核真实原source Weak身份；此处不得重捕文档来替换它们的authority。
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.source.health()?; // 双阶段零工作量或缓存命中同样不能绕过共享源的永久撤权。
        let mut total = MarkdownLayoutBreakStep {
            bytes: 0,
            scalars: 0,
            records: 0,
            ready: false,
            needs_dictionary: false,
        };
        for grapheme in [self.grapheme_first, !self.grapheme_first] {
            if total.bytes == budget.bytes
                || total.scalars == budget.scalars
                || total.records == budget.records
            {
                break; // 次算法没有余额就等下一次真实step，不能零配额私自结算EOF。
            }
            let bytes = budget.bytes - total.bytes;
            let scalars = budget.scalars - total.scalars;
            let records = budget.records - total.records;
            if grapheme {
                let step = payload.grapheme.step(
                    authority,
                    MarkdownGraphemeBudget {
                        bytes,
                        scalars,
                        records,
                    },
                )?;
                total.bytes += step.bytes;
                total.scalars += step.scalars;
                total.records += step.records;
            } else {
                let step = payload.line.step(
                    authority,
                    MarkdownLineBreakBudget {
                        bytes,
                        scalars,
                        records,
                    },
                )?;
                total.bytes += step.bytes;
                total.scalars += step.scalars;
                total.records += step.records;
            }
        }
        self.grapheme_first = !self.grapheme_first;
        total.ready = payload.grapheme.is_ready() && payload.line.is_ready();
        total.needs_dictionary = payload.line.storage_profile().needs_dictionary;
        Ok(total)
    }

    /// 创建一个原任务的只读意图；越界不会被夹到EOF变成另一个有效查询。
    pub fn cursor(
        &self,
        unit: MarkdownLayoutBreakUnit,
        at: usize,
        query: MarkdownLayoutBreakQuery,
    ) -> Result<MarkdownLayoutBreakCursor> {
        if self.payload.is_none() {
            return Err(MarkdownLayoutBreakError::Poisoned);
        }
        self.payload
            .as_ref()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?
            .source
            .health()?;
        let maximum = match unit {
            MarkdownLayoutBreakUnit::Byte => self.maximum[0],
            MarkdownLayoutBreakUnit::Utf16 => self.maximum[1],
        };
        if at > maximum {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        Ok(MarkdownLayoutBreakCursor {
            owner: Rc::downgrade(&self.owner),
            unit,
            query,
            at,
            candidate: None,
            completed: false,
        })
    }

    /// 最多检查给定数量的真实机会；不分配交集Vec，也不复制原索引。
    pub fn query(
        &mut self,
        cursor: &mut MarkdownLayoutBreakCursor,
        candidate_budget: usize,
    ) -> Result<MarkdownLayoutBreakAnswer> {
        if !Weak::ptr_eq(&cursor.owner, &Rc::downgrade(&self.owner)) || cursor.completed {
            return Err(MarkdownLayoutBreakError::CursorMismatch); // 旧/外来意图不能消费当前任务。
        }
        if candidate_budget == 0 {
            return Err(MarkdownLayoutBreakError::InvalidRequest);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        {
            // 每次原查询独立给出阻塞原因；上一查询的等待不能污染后来的GCB或Known结果。
            self.line_source_demand = false;
        }
        let result = self.query_inner(
            cursor,
            candidate_budget.min(MARKDOWN_LAYOUT_BREAK_CANDIDATES),
        );
        match result {
            Ok(answer) => Ok(answer),
            Err(error) => self.poison(error),
        }
    }

    fn query_inner(
        &mut self,
        cursor: &mut MarkdownLayoutBreakCursor,
        candidate_budget: usize,
    ) -> Result<MarkdownLayoutBreakAnswer> {
        let payload = self
            .payload
            .as_mut()
            .ok_or(MarkdownLayoutBreakError::Poisoned)?;
        payload.source.health()?; // raw Line的Known(None)也必须先检查，不能靠之后GCB分支补验证。
        let (line_unit, grapheme_unit) = match cursor.unit {
            MarkdownLayoutBreakUnit::Byte => {
                (MarkdownLineBreakUnit::Byte, MarkdownGraphemeUnit::Byte)
            }
            MarkdownLayoutBreakUnit::Utf16 => {
                (MarkdownLineBreakUnit::Utf16, MarkdownGraphemeUnit::Utf16)
            }
        };
        for _ in 0..candidate_budget {
            let candidate = if let Some(candidate) = cursor.candidate {
                candidate // 字素前缀待决后重试同一个真实机会，不跳过它去找后继。
            } else {
                let query = match cursor.query {
                    MarkdownLayoutBreakQuery::At => MarkdownLineBreakQuery::IsBoundary,
                    MarkdownLayoutBreakQuery::Previous => MarkdownLineBreakQuery::Previous,
                    MarkdownLayoutBreakQuery::Next => MarkdownLineBreakQuery::Next,
                };
                match payload
                    .line
                    .query(&self.authority, line_unit, cursor.at, query)?
                {
                    LineAnswer::Pending => {
                        #[cfg(feature = "paged-shaping-frontier")]
                        {
                            self.line_source_demand = true;
                        }
                        return Ok(MarkdownLayoutBreakAnswer::PendingSource);
                    }
                    LineAnswer::Known(None) => {
                        cursor.completed = true;
                        return Ok(MarkdownLayoutBreakAnswer::Known(None));
                    }
                    LineAnswer::Known(Some(candidate)) => candidate,
                }
            };
            let position = match cursor.unit {
                MarkdownLayoutBreakUnit::Byte => candidate.byte,
                MarkdownLayoutBreakUnit::Utf16 => candidate.utf16,
            };
            match payload.grapheme.query(
                &self.authority,
                grapheme_unit,
                position,
                MarkdownGraphemeQuery::IsBoundary,
            )? {
                GraphemeAnswer::Pending => {
                    cursor.candidate = Some(candidate);
                    #[cfg(feature = "paged-shaping-frontier")]
                    {
                        self.grapheme_source_demand = true;
                    }
                    return Ok(MarkdownLayoutBreakAnswer::PendingSource);
                }
                GraphemeAnswer::Known(Some(boundary)) => {
                    if boundary.byte != candidate.byte || boundary.utf16 != candidate.utf16 {
                        return Err(MarkdownLayoutBreakError::InconsistentCoordinates);
                    }
                    cursor.completed = true;
                    return Ok(MarkdownLayoutBreakAnswer::Known(Some(candidate)));
                }
                GraphemeAnswer::Known(None) => {
                    if cursor.query == MarkdownLayoutBreakQuery::At {
                        cursor.completed = true;
                        return Ok(MarkdownLayoutBreakAnswer::Known(None));
                    }
                    // 原机会切入实际字素，继续严格前驱/后继；有限预算用完保存同一全局位置。
                    cursor.candidate = None;
                    cursor.at = position;
                }
            }
        }
        Ok(MarkdownLayoutBreakAnswer::Yielded)
    }

    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutBreakResourceObserver {
        self.resources.clone()
    }
    pub fn cancel(&mut self) {
        drop(self.payload.take()); // 取消不改外部共享不可变源，只交还本job的两个索引owner。
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use prefix::MarkdownCertifiedGraphemeEndpoint;
