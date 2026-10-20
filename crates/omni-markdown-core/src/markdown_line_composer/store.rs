//! 唯一分页输出的受信producer；原job逐批签入后才发布私有item目录，不存每项强owner。
//!
//! glyph和cluster均是派生元数据。正文只借同一Source，临时原job弱票在完整写入后归还；
//! 历史读取只接受本Store签发的opaque origin，不接受公开Range、字体slot或相同文字重建。

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use super::{
    MarkdownLineComposeCancellation, MarkdownLineComposeError as Error, MarkdownLineComposeStyle,
    MarkdownLineFontCatalog, MarkdownLineFontRegistration, MarkdownLineItem, MarkdownLineItemFacts,
    font::FontKey, item::Result,
};
use crate::{
    MarkdownGlyphCluster, MarkdownGlyphClusterAnswer, MarkdownGlyphClusterIndex,
    MarkdownGlyphClusterStatus, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownShapedGlyph, MarkdownShapingDirection,
    markdown_layout_index::{
        LayoutIndexError, LayoutIndexObserver, LayoutIndexReader, PagedLayoutIndex,
    },
};

const STEP: usize = 256;
const NONE: u64 = u64::MAX;
const GLYPH: u8 = 1;
const CLUSTER: u8 = 2;
const ITEM: u8 = 3;
const STYLE: u8 = 4;
const FEATURE: u8 = 5;
const LANGUAGE: u8 = 6;
const ITEM_END: u8 = 7;
mod control;
mod forest;
mod graphemes;
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
mod table_row;
use graphemes::{GraphemeCapture, GraphemeDirectory};
mod line;
pub(super) mod output;
mod provider;
#[cfg(feature = "paged-shaping-frontier")]
mod publish;
pub(super) mod resolved;
mod selected;
mod skip;
use output::{MarkdownComposedLineFacts, MarkdownComposedLineKind, MarkdownComposedLines};
use skip::LineAncestor;

/// 目录整数只能在本模块中建立；它本身不代表原Source或Font仍有消费权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ItemKey {
    record: usize,
    origin: u64,
}

/// 只在当前查询/行组合借用期保留这一张弱票，不形成按item数增长的Weak集合。
pub(super) struct StoredItemProof {
    store: Weak<()>,
    key: ItemKey,
    // 当前producer的临时原Job回查票；历史目录回读不保留N个控制块，也不从record重建它。
    #[cfg(test)]
    job: Option<Weak<()>>,
}

/// 一次实际定位同时取得的原票与完整目录；调用方只在本次借用内消费，不留作后继缓存。
/// proof-only 客户直接移走原票；行组合则复用已解码的三条不可变记录，仍逐次核 Source/字体。
pub(super) struct LocatedItem {
    pub(super) proof: StoredItemProof,
    directory: Directory,
}

#[derive(Clone)]
pub(super) struct StoredItemFacts {
    pub paragraph_inset_26_6: i32,
    #[cfg(test)]
    pub facts: MarkdownLineItemFacts,
    #[cfg(test)]
    pub glyphs: usize,
    #[cfg(test)]
    pub clusters: usize,
    #[cfg(test)]
    pub x_advance_26_6: i64,
    #[cfg(test)]
    pub y_advance_26_6: i64,
    pub registration: MarkdownLineFontRegistration,
}

struct Directory {
    paragraph_inset_26_6: i32,
    facts: MarkdownLineItemFacts,
    presentation: crate::MarkdownPresentationStyles,
    font: FontKey,
    glyph_start: usize,
    glyph_count: usize,
    cluster_start: usize,
    cluster_count: usize,
    previous: Option<usize>,
    origin: u64,
    style_start: usize,
    style_rows: usize,
    features: usize,
    language_bytes: usize,
    x_advance: i64,
    y_advance: i64,
    kind: super::MarkdownComposedLinePartKind,
    tab_interval: Option<i64>,
    // 只来自原frontier ReadyJob的封闭词证书；该端点不是本目录glyphReady范围。
    frontier_word_end: Option<usize>,
    /// 只来自原 ItemScan 的两端 UBA 分段许可；它们不是编辑、断行或字素边界。
    shaping_seams: [Option<(u8, u8)>; 2],
}

enum Phase {
    Index,
    Glyphs,
    Clusters,
    Style,
    Commit,
    Forest,
}

/// 移交只替换这一唯一IO owner；无File clone、第二FD或sealed数据重写路径。
enum Rows {
    Writing(PagedLayoutIndex<64>),
    #[cfg(test)]
    Reading(LayoutIndexReader<64>),
}
impl Rows {
    fn len(&self) -> usize {
        match self {
            Self::Writing(value) => value.len(),
            #[cfg(test)]
            Self::Reading(value) => value.len(),
        }
    }
    fn append(&mut self, row: [u8; 64]) -> std::result::Result<(), LayoutIndexError> {
        match self {
            Self::Writing(value) => value.append(row),
            #[cfg(test)]
            Self::Reading(_) => Err(LayoutIndexError::Poisoned),
        }
    }
    fn read(&mut self, record: usize) -> std::result::Result<[u8; 64], LayoutIndexError> {
        match self {
            Self::Writing(value) => value.read(record),
            #[cfg(test)]
            Self::Reading(value) => value.read(record),
        }
    }
    fn read_batch(
        &mut self,
        range: Range<usize>,
        output: &mut [[u8; 64]],
    ) -> std::result::Result<(), LayoutIndexError> {
        match self {
            #[cfg(test)]
            Self::Reading(value) => value.read_batch(range, output),
            Self::Writing(value) => value.read_batch(range, output),
        }
    }
    #[cfg(test)]
    fn close(self) -> std::result::Result<(), LayoutIndexError> {
        match self {
            Self::Writing(mut value) => value.close(),
            #[cfg(test)]
            Self::Reading(value) => value.close(),
        }
    }
}

struct Capture {
    paragraph_inset_26_6: i32,
    // 只在实际producer工作期持原job弱身份；写完即Drop，不把控制块写成内存历史表。
    job: Weak<()>,
    facts: MarkdownLineItemFacts,
    registration: MarkdownLineFontRegistration,
    style: MarkdownLineComposeStyle,
    index: super::worker_resources::ClusterResourceOwner,
    phase: Phase,
    count: usize,
    consumed: usize,
    glyph_start: usize,
    cluster_start: usize,
    cluster_count: usize,
    source_cursor: usize,
    style_start: usize,
    style_rows: usize,
    x_advance: i64,
    y_advance: i64,
    tree: Option<forest::Build>,
    frontier_word_end: Option<usize>,
    /// 捕获期间每批都与原 Item 的封闭票复核，提交后才写入同一私有目录。
    shaping_seams: [Option<(u8, u8)>; 2],
}

/// 只从原 Item 的强类型分段票提取固定元数据；外部范围或相同数值不能登记一个内部边界。
fn item_shaping_seams(
    item: &MarkdownLineItem,
    authority: &MarkdownLayoutTextAuthority,
) -> Result<[Option<(u8, u8)>; 2]> {
    if &item.authority != authority {
        return Err(Error::SourceMismatch);
    }
    let mut seams = [None; 2];
    let facts = item.facts();
    for (index, proof) in item.shaping_seams().iter().enumerate() {
        let Some(proof) = proof else {
            continue; // 普通端点仍由真实 GCB 目录拥有，不能从未登记推导出标量分段许可。
        };
        let (_, utf16) = proof.boundary(authority)?;
        let levels = proof.levels();
        let expected = if index == 0 {
            facts.source_utf16.start
        } else {
            facts.source_utf16.end
        };
        if utf16 != expected
            || levels.0 == levels.1
            || levels.0 > 126
            || levels.1 > 126
            || (if index == 0 { levels.1 } else { levels.0 }) != facts.level
        {
            return Err(Error::InconsistentCoverage);
        }
        seams[index] = Some(levels);
    }
    Ok(seams)
}

/// 只有一页16KiB输出缓存及一个当前capture；任何失败都撤销整个尚未封口的候选。
pub(super) struct GlyphStore {
    index: Option<Rows>,
    identity: Arc<()>,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    fonts: MarkdownLineFontCatalog,
    cancellation: MarkdownLineComposeCancellation,
    resources: Option<super::worker_resources::ComposerResourceScope>,
    capture: Option<Capture>,
    control_capture: Option<control::ControlCapture>,
    last_item: Option<usize>,
    source_end: usize,
    origins: u64,
    items: usize,
    observer: LayoutIndexObserver,
    graphemes: Option<GraphemeCapture>,
    folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table_skip: Option<crate::markdown_layout_source::table_rows::TableSpan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    table_line: Option<table_row::TableLineCapture>,
    // 只随真实LineEnd提交；后继GCB预取不能扩张旧输出的目录/EOF。
    published_graphemes: Option<GraphemeDirectory>,
    line_end: Option<usize>,
    line_source_end: usize,
    line_ancestors: [Option<LineAncestor>; usize::BITS as usize],
    lines: usize,
    forest: [Option<forest::Root>; 64],
}

impl GlyphStore {
    pub(super) fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        fonts: MarkdownLineFontCatalog,
        cancellation: MarkdownLineComposeCancellation,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(Error::SourceMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let index = PagedLayoutIndex::new()?;
        let observer = index.resource_observer();
        Ok(Self {
            index: Some(Rows::Writing(index)),
            identity: Arc::new(()),
            source,
            authority: authority.clone(),
            fonts,
            cancellation,
            resources: None,
            capture: None,
            control_capture: None,
            last_item: None,
            source_end: 0,
            origins: 0,
            items: 0,
            observer,
            graphemes: None,
            folds: None,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            tables: None,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            table_skip: None,
            #[cfg(all(
                feature = "s2-table-window-prototype",
                feature = "paged-shaping-frontier"
            ))]
            table_line: None,
            published_graphemes: None,
            line_end: None,
            line_source_end: 0,
            line_ancestors: [None; usize::BITS as usize],
            lines: 0,
            forest: [None; 64],
        })
    }

    pub(super) fn set_resource_scope(
        &mut self,
        resources: super::worker_resources::ComposerResourceScope,
    ) {
        resources.register_writer(output::MarkdownComposedLineResourceObserver::for_source(
            self.observer.clone(),
            &self.source,
        ));
        self.resources = Some(resources);
    }

    /// 只在 composer 构造时分享原有限物理行 proof；输出 reader 不保存第二份范围目录。
    pub(super) fn set_fold_plan(
        &mut self,
        folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    ) {
        self.folds = folds;
    }

    /// 输出与原 Unicode job 共用结构身份；只读查询不会给表格内部原正文补布局权限。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn set_table_plan(
        &mut self,
        plan: Option<crate::markdown_layout_source::table_rows::TablePlan>,
    ) {
        self.tables = plan;
    }

    /// 当前行的迁移范围只来自 sealed row，不接受外部裸 Range。
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub(super) fn set_table_skip(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
    ) {
        self.table_skip = Some(span);
    }

    fn guard(&self) -> Result<()> {
        self.source.health()?; // 已写目录、EOF和页命中仍先核原始IO，不能只相信输出缓存。
        if !self.source.permits(&self.authority) {
            return Err(Error::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let rows = self.index.as_ref().ok_or(Error::Poisoned)?;
        let state = self.observer.snapshot();
        let closed = match rows {
            // 未换页的活writer只有buffer或空状态；真实close收据才代表它退出。
            Rows::Writing(_) => state.owner_closed,
            #[cfg(test)]
            // 封存移交后的读取必须持有真实FD，不能把writer退出事实误用为reader仍存活。
            Rows::Reading(_) => !state.file_open,
        };
        if closed || state.cleanup_failed || state.reader_failed {
            return Err(Error::Poisoned);
        }
        Ok(())
    }

    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        let result = result.and_then(|value| {
            self.guard()?;
            Ok(value)
        });
        if result.is_err() {
            // 先归还实际临时索引和唯一输出文件；旧Proof只能保持失效Weak，无法重启producer。
            drop(self.capture.take());
            drop(self.control_capture.take());
            drop(self.index.take());
            self.source.health()?; // 失败清理可能带来真实映射首错，必须在返回原错误前保留它。
        }
        result // 成功已由上方guard核健康，其后没有IO或资源转换，不再重复遍历同一Source。
    }

    fn require_capture(&mut self, item: &mut MarkdownLineItem) -> Result<()> {
        self.guard()?;
        let capture = self.capture.as_ref().ok_or(Error::InvalidRequest)?;
        let registered = self.fonts.require_registration(&capture.registration)?;
        let profile = item.job.profile()?;
        item.job
            .glyph(&self.authority, &item.font, profile.glyph_count)?;
        if !Weak::ptr_eq(&capture.job, &item.job.index_identity())
            || item.authority != self.authority
            || !item.source.permits(&self.authority)
            || !registered.font().permits(&item.font)
            || profile.source_generation != self.source.generation()
            || profile.glyph_count != capture.count
            || item.facts() != &capture.facts
            || item_shaping_seams(item, &self.authority)? != capture.shaping_seams
        {
            return Err(Error::SourceMismatch);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if item.job.frontier_word_end(&self.authority, &item.font)? != capture.frontier_word_end {
            return Err(Error::SourceMismatch);
        } // 每批/commit前仍由原实际Job复核，不能把已撤销的私有证书写成永久目录事实。
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn begin(&mut self, item: &mut MarkdownLineItem) -> Result<()> {
        self.begin_with_inset(item, 0)
    }

    /// 原捕获和字形记录数不变；缩进只随这一个同 nonce Item 写入既有目录空闲字段。
    pub(super) fn begin_with_inset(
        &mut self,
        item: &mut MarkdownLineItem,
        paragraph_inset_26_6: i32,
    ) -> Result<()> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some()
                || self.control_capture.is_some()
                || item.facts().source_utf16.start != self.source_end
                || !matches!(self.index, Some(Rows::Writing(_)))
                || self.graphemes.as_ref().is_some_and(|capture| {
                    capture.has_buffered_through(item.facts().source_utf16.end)
                })
            {
                return Err(Error::InconsistentCoverage);
            }
            if paragraph_inset_26_6 < 0 {
                return Err(Error::InvalidRequest);
            } // 真正参与行宽运算的封存值不能为负，不能扩大超过原正文列的可用范围。
            let registration = item.registration.clone().ok_or(Error::FontExpired)?;
            let registered = self.fonts.require_registration(&registration)?;
            if item.authority != self.authority || !registered.font().permits(&item.font) {
                return Err(Error::SourceMismatch);
            }
            let profile = item.job.profile()?;
            item.job
                .glyph(&self.authority, &item.font, profile.glyph_count)?;
            #[cfg(feature = "paged-shaping-frontier")]
            let frontier_word_end = item.job.frontier_word_end(&self.authority, &item.font)?;
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let frontier_word_end = None;
            let shaping_seams = item_shaping_seams(item, &self.authority)?;
            let created =
                MarkdownGlyphClusterIndex::new(&mut item.job, &self.authority, &item.font);
            if created.is_err()
                && let Some(resources) = &self.resources
            {
                resources.incomplete();
            } // 构造未交出原收据的失败不能据SourceDrop或后续零计数冒签覆盖完整。
            let index = super::worker_resources::ClusterResourceOwner::new(
                created?,
                self.resources.as_ref(),
            );
            let glyph_start = self.index.as_ref().ok_or(Error::Poisoned)?.len();
            self.capture = Some(Capture {
                paragraph_inset_26_6,
                job: item.job.index_identity(),
                facts: item.facts().clone(),
                registration,
                style: item.replay_style.clone(),
                index,
                phase: Phase::Index,
                count: profile.glyph_count,
                consumed: 0,
                glyph_start,
                cluster_start: 0,
                cluster_count: 0,
                source_cursor: item.facts().source_utf16.start,
                style_start: 0,
                style_rows: 0,
                x_advance: 0,
                y_advance: 0,
                tree: None,
                frontier_word_end,
                shaping_seams,
            });
            self.require_capture(item)
        })();
        self.finish(result)
    }

    /// 每次只写≤256条实际记录；返回Proof只表示完整原item签入，并不表示已选中软行。
    pub(super) fn step(
        &mut self,
        item: &mut MarkdownLineItem,
        records: usize,
    ) -> Result<Option<StoredItemProof>> {
        let result = self.step_inner(item, records);
        self.finish(result)
    }

    fn step_inner(
        &mut self,
        item: &mut MarkdownLineItem,
        records: usize,
    ) -> Result<Option<StoredItemProof>> {
        self.require_capture(item)?;
        if records == 0 {
            return Err(Error::InvalidRequest);
        }
        let limit = records.min(STEP);
        let capture = self.capture.as_mut().ok_or(Error::InvalidRequest)?;
        let file = self.index.as_mut().ok_or(Error::Poisoned)?;
        match capture.phase {
            Phase::Index => {
                let step = capture.index.step(&mut item.job, limit)?;
                if step.status == MarkdownGlyphClusterStatus::Ready {
                    capture.phase = Phase::Glyphs;
                }
            }
            Phase::Glyphs => {
                for _ in 0..limit {
                    if capture.consumed == capture.count {
                        break;
                    }
                    let glyph = item
                        .job
                        .glyph(&self.authority, &item.font, capture.consumed)?
                        .ok_or(Error::InconsistentCoverage)?;
                    file.append(encode_glyph(glyph)?)?;
                    capture.consumed = capture.consumed.checked_add(1).ok_or(Error::Overflow)?;
                }
                if capture.consumed == capture.count {
                    if item
                        .job
                        .glyph(&self.authority, &item.font, capture.count)?
                        .is_some()
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    capture.cluster_start = file.len();
                    capture.phase = Phase::Clusters;
                }
            }
            Phase::Clusters => {
                for _ in 0..limit {
                    if capture.source_cursor == capture.facts.source_utf16.end {
                        break;
                    }
                    // 已签入数量就是原逻辑源顺序的下一ordinal，不再为顺序复制重复二分同一索引。
                    let value = capture
                        .index
                        .at_ordinal(&mut item.job, capture.cluster_count)?
                        .ok_or(Error::InconsistentCoverage)?;
                    if value.source_utf16.start != capture.source_cursor
                        || value.source_utf16.end > capture.facts.source_utf16.end
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    file.append(encode_cluster(&value)?)?;
                    capture.source_cursor = value.source_utf16.end;
                    capture.cluster_count = capture
                        .cluster_count
                        .checked_add(1)
                        .ok_or(Error::Overflow)?;
                    capture.x_advance = capture
                        .x_advance
                        .checked_add(value.x_advance_26_6)
                        .ok_or(Error::Overflow)?;
                    capture.y_advance = capture
                        .y_advance
                        .checked_add(value.y_advance_26_6)
                        .ok_or(Error::Overflow)?;
                }
                if capture.source_cursor == capture.facts.source_utf16.end {
                    // 源范围走到真实末端时，原索引也必须恰好耗尽；不把遗漏记录签入样式目录。
                    if capture
                        .index
                        .at_ordinal(&mut item.job, capture.cluster_count)?
                        .is_some()
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    capture.style_start = file.len();
                    capture.consumed = 0;
                    capture.phase = Phase::Style;
                }
            }
            Phase::Style => {
                // 原样保存有界样式，包含feature全局范围；不能重形时偷偷退回默认feature。
                let language_rows = capture
                    .style
                    .language
                    .as_ref()
                    .map_or(0, |value| value.len().div_ceil(55));
                let total = capture
                    .style
                    .features
                    .len()
                    .checked_add(language_rows)
                    .ok_or(Error::Overflow)?;
                for _ in 0..limit {
                    if capture.consumed == total {
                        break;
                    }
                    let mut row = [0; 64];
                    if capture.consumed < capture.style.features.len() {
                        let feature = capture.style.features[capture.consumed];
                        row[..4].copy_from_slice(&feature.tag);
                        row[4..8].copy_from_slice(&feature.value.to_le_bytes());
                        row[8..12].copy_from_slice(&feature.source_utf16_start.to_le_bytes());
                        row[12..16].copy_from_slice(&feature.source_utf16_end.to_le_bytes());
                        row[56] = FEATURE;
                    } else {
                        // 原语言≤64B，因此最多两条；它们与feature共用本次records，不额外越额。
                        let language = capture
                            .style
                            .language
                            .as_ref()
                            .ok_or(Error::InconsistentCoverage)?
                            .as_bytes();
                        let start = (capture.consumed - capture.style.features.len())
                            .checked_mul(55)
                            .ok_or(Error::Overflow)?;
                        let end = start
                            .checked_add(55)
                            .ok_or(Error::Overflow)?
                            .min(language.len());
                        let bytes = language
                            .get(start..end)
                            .filter(|value| !value.is_empty())
                            .ok_or(Error::InconsistentCoverage)?;
                        row[0] = u8::try_from(bytes.len()).map_err(|_| Error::Overflow)?;
                        row[1..1 + bytes.len()].copy_from_slice(bytes);
                        row[56] = LANGUAGE;
                    }
                    file.append(row)?;
                    capture.consumed += 1;
                }
                if capture.consumed == total {
                    capture.style_rows = file
                        .len()
                        .checked_sub(capture.style_start)
                        .ok_or(Error::Overflow)?;
                    capture.consumed = 0;
                    capture.phase = Phase::Commit;
                }
            }
            Phase::Commit => {
                // 最后再核原Job/profile/font，不让写入期间撤权后的部分文件产生有效origin。
                self.require_capture(item)?;
                let capture = self.capture.as_mut().ok_or(Error::InvalidRequest)?;
                let origin = self.origins.checked_add(1).ok_or(Error::Overflow)?;
                let directory = Directory {
                    paragraph_inset_26_6: capture.paragraph_inset_26_6,
                    facts: capture.facts.clone(),
                    presentation: capture.style.presentation,
                    font: capture.registration.key(),
                    glyph_start: capture.glyph_start,
                    glyph_count: capture.count,
                    cluster_start: capture.cluster_start,
                    cluster_count: capture.cluster_count,
                    previous: self.last_item,
                    origin,
                    style_start: capture.style_start,
                    style_rows: capture.style_rows,
                    features: capture.style.features.len(),
                    language_bytes: capture.style.language.as_ref().map_or(0, String::len),
                    x_advance: capture.x_advance,
                    y_advance: capture.y_advance,
                    kind: super::MarkdownComposedLinePartKind::Glyphs,
                    tab_interval: None,
                    frontier_word_end: capture.frontier_word_end,
                    shaping_seams: capture.shaping_seams,
                };
                let file = self.index.as_mut().ok_or(Error::Poisoned)?;
                let record = capture
                    .style_start
                    .checked_add(capture.style_rows)
                    .ok_or(Error::Overflow)?;
                if file.len()
                    != record
                        .checked_add(capture.consumed)
                        .ok_or(Error::Overflow)?
                {
                    return Err(Error::InconsistentCoverage);
                }
                for row in encode_directory(&directory)?
                    .into_iter()
                    .skip(capture.consumed)
                    .take(limit)
                {
                    file.append(row)?;
                    capture.consumed += 1;
                }
                if capture.consumed != 3 {
                    self.require_capture(item)?;
                    return Ok(None);
                }
                self.require_capture(item)?;
                self.capture.as_mut().ok_or(Error::InvalidRequest)?.tree = Some(
                    forest::Build::new(ItemKey { record, origin }, directory.facts.source_utf16),
                );
                self.capture.as_mut().ok_or(Error::InvalidRequest)?.phase = Phase::Forest;
            }
            Phase::Forest => {
                let capture = self.capture.as_mut().ok_or(Error::InvalidRequest)?;
                let tree = capture.tree.as_mut().ok_or(Error::InvalidRequest)?;
                if !tree.step(
                    self.index.as_mut().ok_or(Error::Poisoned)?,
                    &mut self.forest,
                    limit,
                )? {
                    return Ok(None);
                }
                let key = tree.key();
                self.require_capture(item)?;
                self.last_item = Some(key.record);
                self.origins = key.origin;
                self.items = self.items.checked_add(1).ok_or(Error::Overflow)?;
                self.source_end = self
                    .capture
                    .as_ref()
                    .ok_or(Error::InvalidRequest)?
                    .facts
                    .source_utf16
                    .end;
                drop(self.capture.take()); // 临时原job Weak与cluster FD在签完目录和搜索树后真实归还。
                return Ok(Some(StoredItemProof {
                    store: Arc::downgrade(&self.identity),
                    key,
                    #[cfg(test)]
                    job: Some(item.job.index_identity()),
                }));
            }
        }
        self.require_capture(item)?;
        Ok(None)
    }

    fn directory(&mut self, proof: &StoredItemProof) -> Result<Directory> {
        self.guard()?;
        if !Weak::ptr_eq(&proof.store, &Arc::downgrade(&self.identity)) {
            return Err(Error::SourceMismatch);
        }
        let file = self.index.as_mut().ok_or(Error::Poisoned)?;
        let end = proof.key.record.checked_add(3).ok_or(Error::Overflow)?;
        let mut records = [[0; 64]; 3];
        file.read_batch(proof.key.record..end, &mut records)?;
        // 目录本来就要求完整的三条相邻record；合并同一请求的IO，不扩大语义读取预算。
        let directory = decode_directory(records)?;
        if directory.origin != proof.key.origin
            || directory.facts.source_utf16.end > self.source_end
            || directory.glyph_start.checked_add(directory.glyph_count)
                != Some(directory.cluster_start)
            || directory.cluster_start.checked_add(directory.cluster_count)
                != Some(directory.style_start)
            || directory.style_start.checked_add(directory.style_rows) != Some(proof.key.record)
            || directory
                .previous
                .is_some_and(|record| record >= directory.glyph_start)
            || directory
                .frontier_word_end
                .is_some_and(|end| end > self.source.utf16_len())
        {
            return Err(Error::InconsistentCoverage);
        }
        self.fonts.resolve(directory.font)?; // 过期slot不能给旧glyph新配font；也不以目录整数恢复owner。
        self.guard()?;
        Ok(directory)
    }

    pub(super) fn facts(&mut self, proof: &StoredItemProof) -> Result<StoredItemFacts> {
        let result = (|| {
            let directory = self.directory(proof)?;
            Ok(StoredItemFacts {
                paragraph_inset_26_6: directory.paragraph_inset_26_6,
                #[cfg(test)]
                facts: directory.facts,
                #[cfg(test)]
                glyphs: directory.glyph_count,
                #[cfg(test)]
                clusters: directory.cluster_count,
                #[cfg(test)]
                x_advance_26_6: directory.x_advance,
                #[cfg(test)]
                y_advance_26_6: directory.y_advance,
                registration: self.fonts.resolve(directory.font)?.registration(),
            })
        })();
        self.finish(result)
    }

    /// 只为原已选重排范围领取一个真实Glyphs目录；Tab或跨item范围没有单字体重排权限。
    pub(super) fn resolution_item_at(
        &mut self,
        range: &Range<usize>,
    ) -> Result<Option<StoredItemProof>> {
        let result = (|| {
            let Some(LocatedItem { proof, directory }) = self.item_at(range.start)? else {
                return Ok(None);
            };
            Ok(
                (directory.kind == super::MarkdownComposedLinePartKind::Glyphs
                    && range.start >= directory.facts.source_utf16.start
                    && range.end <= directory.facts.source_utf16.end)
                    .then_some(proof),
            )
        })();
        self.finish(result)
    }

    #[cfg(test)]
    pub(super) fn glyph(
        &mut self,
        proof: &StoredItemProof,
        ordinal: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        let result = (|| {
            let directory = self.directory(proof)?;
            if ordinal > directory.glyph_count {
                return Err(Error::InvalidRequest);
            }
            if ordinal == directory.glyph_count {
                return Ok(None);
            }
            let record = directory
                .glyph_start
                .checked_add(ordinal)
                .ok_or(Error::Overflow)?;
            let glyph = decode_glyph(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)?;
            if !directory
                .facts
                .source_utf16
                .contains(&glyph.source_utf16_cluster)
            {
                return Err(Error::InconsistentCoverage);
            }
            Ok(Some(glyph))
        })();
        self.finish(result)
    }

    #[cfg(test)]
    pub(super) fn cluster(
        &mut self,
        proof: &StoredItemProof,
        offset: usize,
    ) -> Result<Option<MarkdownGlyphCluster>> {
        let directory = match self.directory(proof) {
            Ok(directory) => directory,
            Err(error) => return self.finish(Err(error)), // 外来票或实际目录IO失败立即撤候选，不进入子范围查询。
        };
        self.cluster_in_directory(&directory, offset)
            .map(|located| located.map(|(_, cluster)| cluster))
    }

    /// 仅复用本Store在本次同步调用已验证的目录；不跨让出保存，不重新读同三条不可变record。
    fn cluster_in_directory(
        &mut self,
        directory: &Directory,
        offset: usize,
    ) -> Result<Option<(usize, MarkdownGlyphCluster)>> {
        let result = (|| {
            self.guard()?;
            self.fonts.resolve(directory.font)?;
            self.guard()?; // 目录字节不可变不代表Source或字体仍有效，EOF与同范围查询也先核原票。
            if offset < directory.facts.source_utf16.start
                || offset > directory.facts.source_utf16.end
            {
                return Err(Error::InvalidRequest);
            }
            if offset == directory.facts.source_utf16.end {
                return Ok(None);
            }
            // 真实目录已给出首末 ordinal；查询恰在 item 两端时不必二分相同记录。
            // 这里只选择读取位置，结果仍来自原64B记录，并逐项解码、核覆盖与 glyph 范围。
            let endpoint = if offset == directory.facts.source_utf16.start {
                Some(0)
            } else if offset.checked_add(1) == Some(directory.facts.source_utf16.end) {
                Some(
                    directory
                        .cluster_count
                        .checked_sub(1)
                        .ok_or(Error::InconsistentCoverage)?,
                )
            } else {
                None
            };
            if let Some(ordinal) = endpoint {
                self.guard()?;
                let value = self.cluster_record_in_directory(directory, ordinal)?;
                if !value.source_utf16.contains(&offset) {
                    return Err(Error::InconsistentCoverage);
                }
                return Ok(Some((ordinal, value)));
            }
            let (mut low, mut high) = (0, directory.cluster_count);
            while low < high {
                self.guard()?; // 每次真实页读取可取消，二分至多usize::BITS，不按整项线性扫描。
                let middle = low + (high - low) / 2;
                let value = self.cluster_record_in_directory(directory, middle)?;
                if offset < value.source_utf16.start {
                    high = middle;
                } else if offset >= value.source_utf16.end {
                    low = middle + 1;
                } else {
                    return Ok(Some((middle, value)));
                }
            }
            Err(Error::InconsistentCoverage) // 完整签入目录内部无覆盖是损坏，不是Pending或零宽空白。
        })();
        self.finish(result)
    }

    /// 调用方已核本次Source/字体；只读原目录中确实需要的一条记录，不预取后继内容。
    fn cluster_record_in_directory(
        &mut self,
        directory: &Directory,
        ordinal: usize,
    ) -> Result<MarkdownGlyphCluster> {
        if ordinal >= directory.cluster_count {
            return Err(Error::InconsistentCoverage); // 声称存在的相邻cluster必须确实属于完整目录。
        }
        let record = directory
            .cluster_start
            .checked_add(ordinal)
            .ok_or(Error::Overflow)?;
        let value = decode_cluster(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)?;
        if value.source_utf16.start < directory.facts.source_utf16.start
            || value.source_utf16.end > directory.facts.source_utf16.end
            || value.glyphs.end > directory.glyph_count
        {
            return Err(Error::InconsistentCoverage); // 原文件坏记录不能变成有效坐标或安全断点。
        }
        Ok(value)
    }

    pub(super) fn style(&mut self, proof: &StoredItemProof) -> Result<MarkdownLineComposeStyle> {
        let result = (|| {
            let directory = self.directory(proof)?;
            if directory.kind != super::MarkdownComposedLinePartKind::Glyphs {
                return Err(Error::InvalidRequest);
            }
            let expected_rows = directory
                .features
                .checked_add(directory.language_bytes.div_ceil(55))
                .ok_or(Error::Overflow)?;
            if directory.style_rows != expected_rows
                || directory.features > 64
                || directory.language_bytes > 64
            {
                return Err(Error::InconsistentCoverage);
            }
            let mut features = Vec::with_capacity(directory.features); // 只有已验证≤64条样式，不是正文或逐glyph缓存。
            for ordinal in 0..directory.features {
                self.guard()?;
                let record = directory
                    .style_start
                    .checked_add(ordinal)
                    .ok_or(Error::Overflow)?;
                let row = self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?;
                if row[56] != FEATURE
                    || row[16..56]
                        .iter()
                        .chain(row[57..].iter())
                        .any(|byte| *byte != 0)
                {
                    return Err(Error::InconsistentCoverage);
                }
                features.push(crate::MarkdownShapingFeature {
                    tag: row[..4].try_into().expect("固定4B字段"),
                    value: u32::from_le_bytes(row[4..8].try_into().expect("固定4B字段")),
                    source_utf16_start: u32::from_le_bytes(
                        row[8..12].try_into().expect("固定4B字段"),
                    ),
                    source_utf16_end: u32::from_le_bytes(
                        row[12..16].try_into().expect("固定4B字段"),
                    ),
                });
            }
            let language = if directory.language_bytes == 0 {
                None
            } else {
                let mut bytes = [0; 64];
                let mut consumed = 0usize;
                for ordinal in directory.features..directory.style_rows {
                    self.guard()?;
                    let row = self.index.as_mut().ok_or(Error::Poisoned)?.read(
                        directory
                            .style_start
                            .checked_add(ordinal)
                            .ok_or(Error::Overflow)?,
                    )?;
                    let count = usize::from(row[0]);
                    if row[56] != LANGUAGE
                        || count == 0
                        || count > 55
                        || consumed
                            .checked_add(count)
                            .is_none_or(|end| end > directory.language_bytes)
                        || row[1 + count..56]
                            .iter()
                            .chain(row[57..].iter())
                            .any(|byte| *byte != 0)
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    bytes[consumed..consumed + count].copy_from_slice(&row[1..1 + count]);
                    consumed += count;
                }
                if consumed != directory.language_bytes {
                    return Err(Error::InconsistentCoverage);
                }
                Some(
                    std::str::from_utf8(&bytes[..consumed])
                        .map_err(|_| Error::InconsistentCoverage)?
                        .to_owned(),
                )
            };
            Ok(MarkdownLineComposeStyle {
                presentation: directory.presentation,
                points: directory.facts.points,
                language,
                features,
            })
        })();
        self.finish(result)
    }

    #[cfg(test)]
    pub(super) fn previous(&mut self, proof: &StoredItemProof) -> Result<Option<StoredItemProof>> {
        let result = (|| {
            let directory = self.directory(proof)?;
            let Some(record) = directory.previous else {
                return Ok(None);
            };
            if record >= proof.key.record {
                return Err(Error::InconsistentCoverage);
            }
            let row = self
                .index
                .as_mut()
                .ok_or(Error::Poisoned)?
                .read(record.checked_add(1).ok_or(Error::Overflow)?)?;
            if row[56] != STYLE {
                return Err(Error::InconsistentCoverage);
            }
            let previous = StoredItemProof {
                store: Arc::downgrade(&self.identity),
                key: ItemKey {
                    record,
                    origin: u64_at(&row, 32),
                },
                #[cfg(test)]
                job: None,
            };
            let facts = self.directory(&previous)?;
            if facts.facts.source_utf16.end != directory.facts.source_utf16.start {
                return Err(Error::InconsistentCoverage);
            }
            Ok(Some(previous))
        })();
        self.finish(result)
    }

    /// 只计唯一最终Store已实际写入的record；临时cluster索引不混充为scene输出。
    pub(super) fn record_count(&self) -> Result<usize> {
        self.guard()?;
        Ok(self.index.as_ref().ok_or(Error::Poisoned)?.len())
    }

    pub(super) fn resource_observer(&self) -> Result<output::MarkdownComposedLineResourceObserver> {
        self.guard()?;
        Ok(output::MarkdownComposedLineResourceObserver::for_source(
            self.observer.clone(),
            &self.source,
        )) // 这是writer本身的实际FD/映射收据，不能拿prefix reader的独立收据替代。
    }

    #[cfg(test)]
    pub(super) fn observer(&self) -> LayoutIndexObserver {
        self.observer.clone()
    }
    pub(super) fn item_at(&mut self, offset: usize) -> Result<Option<LocatedItem>> {
        let result = (|| {
            self.guard()?;
            if offset > self.source_end {
                return Err(Error::InvalidRequest);
            }
            if offset == self.source_end {
                return Ok(None);
            } // 仅已签入前缀的末端，调用者另核真正文档EOF。
            let key = forest::lookup(
                self.index.as_mut().ok_or(Error::Poisoned)?,
                &self.forest,
                offset,
            )?;
            let proof = StoredItemProof {
                store: Arc::downgrade(&self.identity),
                key,
                #[cfg(test)]
                job: None,
            };
            let directory = self.directory(&proof)?;
            if !directory.facts.source_utf16.contains(&offset) {
                return Err(Error::InconsistentCoverage);
            }
            Ok(Some(LocatedItem { proof, directory }))
        })();
        self.finish(result)
    }

    /// item目录封口只证明派生数据不可再写，不表示任何软行、行高或Native几何Ready。
    #[cfg(test)]
    pub(super) fn seal_items(&mut self) -> Result<()> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some() {
                return Err(Error::InvalidRequest);
            }
            let rows = self.index.take().ok_or(Error::Poisoned)?;
            self.index = Some(match rows {
                Rows::Writing(index) => Rows::Reading(index.into_reader()?),
                Rows::Reading(reader) => Rows::Reading(reader),
            });
            Ok(())
        })();
        self.finish(result)
    }

    /// 输出可能已写入候选前缀才失败；调用方只能在完整Ok后发布整个batch。
    #[cfg(test)]
    pub(super) fn glyphs(
        &mut self,
        proof: &StoredItemProof,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<()> {
        let result = (|| {
            let directory = self.directory(proof)?;
            if range.end.checked_sub(range.start) != Some(output.len())
                || output.len() > STEP
                || range.end > directory.glyph_count
            {
                return Err(Error::InvalidRequest);
            }
            let start = directory
                .glyph_start
                .checked_add(range.start)
                .ok_or(Error::Overflow)?;
            let end = directory
                .glyph_start
                .checked_add(range.end)
                .ok_or(Error::Overflow)?;
            let mut rows = [[0; 64]; STEP]; // 固定16KiB栈上候选，没有按item/glyph数增长的分配。
            self.index
                .as_mut()
                .ok_or(Error::Poisoned)?
                .read_batch(start..end, &mut rows[..output.len()])?;
            for (row, slot) in rows.into_iter().zip(output.iter_mut()) {
                let glyph = decode_glyph(row)?;
                if !directory
                    .facts
                    .source_utf16
                    .contains(&glyph.source_utf16_cluster)
                {
                    return Err(Error::InconsistentCoverage);
                }
                *slot = glyph;
            }
            self.directory(proof)?; // Native/worker消费前仍是原Source/Font/catalog，没有以IO成功代替权限。
            Ok(())
        })();
        self.finish(result)
    }

    #[cfg(test)]
    pub(super) fn close(mut self) -> Result<()> {
        drop(self.capture.take());
        let result = match self.index.take() {
            Some(rows) => rows.close().map_err(Error::from),
            None => Err(Error::Poisoned),
        };
        self.source.health()?;
        result
    }
}

fn number(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Overflow)
}
fn small(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::Overflow)
}
fn usize_at(bytes: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(u64::from_le_bytes(
        bytes[offset..offset + 8].try_into().expect("固定8B字段"),
    ))
    .map_err(|_| Error::Overflow)
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("固定8B字段"))
}
fn i64_at(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("固定8B字段"))
}

fn encode_glyph(value: MarkdownShapedGlyph) -> Result<[u8; 64]> {
    let mut row = [0; 64];
    row[..4].copy_from_slice(&value.glyph_id.to_le_bytes());
    row[4..8].copy_from_slice(&value.flags.to_le_bytes());
    row[8..16].copy_from_slice(&number(value.source_utf16_cluster)?.to_le_bytes());
    for (slot, value) in [
        value.x_advance,
        value.y_advance,
        value.x_offset,
        value.y_offset,
    ]
    .into_iter()
    .enumerate()
    {
        row[16 + slot * 4..20 + slot * 4].copy_from_slice(&value.to_le_bytes());
    }
    row[56] = GLYPH;
    Ok(row)
}
fn decode_glyph(row: [u8; 64]) -> Result<MarkdownShapedGlyph> {
    if row[56] != GLYPH
        || row[32..56]
            .iter()
            .chain(row[57..].iter())
            .any(|value| *value != 0)
    {
        return Err(Error::InconsistentCoverage);
    }
    let signed = |at| i32::from_le_bytes(row[at..at + 4].try_into().expect("固定4B字段"));
    Ok(MarkdownShapedGlyph {
        glyph_id: u32::from_le_bytes(row[..4].try_into().expect("固定4B字段")),
        flags: u32::from_le_bytes(row[4..8].try_into().expect("固定4B字段")),
        source_utf16_cluster: usize_at(&row, 8)?,
        x_advance: signed(16),
        y_advance: signed(20),
        x_offset: signed(24),
        y_offset: signed(28),
    })
}
fn encode_cluster(value: &MarkdownGlyphCluster) -> Result<[u8; 64]> {
    let mut row = [0; 64];
    for (slot, value) in [
        value.source_utf16.start,
        value.source_utf16.end,
        value.glyphs.start,
        value.glyphs.end,
        value.missing_glyphs,
    ]
    .into_iter()
    .enumerate()
    {
        row[slot * 4..slot * 4 + 4].copy_from_slice(&small(value)?.to_le_bytes());
    }
    row[20..24].copy_from_slice(&value.flags.to_le_bytes());
    for (slot, value) in [
        value.prefix_x_26_6,
        value.prefix_y_26_6,
        value.x_advance_26_6,
        value.y_advance_26_6,
    ]
    .into_iter()
    .enumerate()
    {
        row[24 + slot * 8..32 + slot * 8].copy_from_slice(&value.to_le_bytes());
    }
    row[56] = CLUSTER;
    Ok(row)
}
fn decode_cluster(row: [u8; 64]) -> Result<MarkdownGlyphCluster> {
    if row[56] != CLUSTER || row[57..].iter().any(|value| *value != 0) {
        return Err(Error::InconsistentCoverage);
    }
    let small = |at| {
        usize::try_from(u32::from_le_bytes(
            row[at..at + 4].try_into().expect("固定4B字段"),
        ))
        .map_err(|_| Error::Overflow)
    };
    let value = MarkdownGlyphCluster {
        source_utf16: small(0)?..small(4)?,
        glyphs: small(8)?..small(12)?,
        missing_glyphs: small(16)?,
        flags: u32::from_le_bytes(row[20..24].try_into().expect("固定4B字段")),
        prefix_x_26_6: i64_at(&row, 24),
        prefix_y_26_6: i64_at(&row, 32),
        x_advance_26_6: i64_at(&row, 40),
        y_advance_26_6: i64_at(&row, 48),
    };
    if value.source_utf16.is_empty()
        || value.glyphs.start > value.glyphs.end
        || value.missing_glyphs > value.glyphs.len()
    {
        return Err(Error::InconsistentCoverage);
    }
    Ok(value)
}
fn encode_directory(value: &Directory) -> Result<[[u8; 64]; 3]> {
    let mut rows = [[0; 64]; 3];
    for (slot, value) in [
        value.facts.source_utf16.start,
        value.facts.source_utf16.end,
        value.glyph_start,
        value.glyph_count,
        value.cluster_start,
        value.cluster_count,
    ]
    .into_iter()
    .enumerate()
    {
        rows[0][slot * 8..slot * 8 + 8].copy_from_slice(&number(value)?.to_le_bytes());
    }
    rows[0][48..56].copy_from_slice(
        &value
            .previous
            .map(number)
            .transpose()?
            .unwrap_or(NONE)
            .to_le_bytes(),
    );
    rows[0][56] = ITEM;
    rows[0][57] = value.facts.level;
    rows[0][58] = match value.facts.direction {
        MarkdownShapingDirection::LeftToRight => 0,
        MarkdownShapingDirection::RightToLeft => 1,
        _ => return Err(Error::InvalidRequest),
    };
    rows[0][59] =
        u8::from(value.facts.beginning_of_text) | (u8::from(value.facts.end_of_text) << 1);
    rows[0][60..].copy_from_slice(&value.facts.script);
    rows[1][..8].copy_from_slice(&value.font.generation.to_le_bytes());
    rows[1][8..10].copy_from_slice(&value.font.slot.to_le_bytes());
    rows[1][12..16].copy_from_slice(&value.facts.points.to_le_bytes());
    rows[1][16..24].copy_from_slice(&number(value.facts.paragraph_utf16.start)?.to_le_bytes());
    rows[1][24..32].copy_from_slice(&number(value.facts.paragraph_utf16.end)?.to_le_bytes());
    rows[1][32..40].copy_from_slice(&value.origin.to_le_bytes());
    rows[1][40..48].copy_from_slice(&number(value.style_start)?.to_le_bytes());
    rows[1][48..52].copy_from_slice(&small(value.style_rows)?.to_le_bytes());
    rows[1][52] = u8::try_from(value.features).map_err(|_| Error::Overflow)?;
    rows[1][53] = u8::try_from(value.language_bytes).map_err(|_| Error::Overflow)?;
    rows[1][54] = value.kind.code();
    rows[1][55] = u8::from(value.tab_interval.is_some());
    rows[1][56] = STYLE;
    rows[2][..8].copy_from_slice(&value.x_advance.to_le_bytes());
    rows[2][8..16].copy_from_slice(&value.y_advance.to_le_bytes());
    rows[2][16..24].copy_from_slice(&value.origin.to_le_bytes());
    rows[2][24..32].copy_from_slice(&value.tab_interval.unwrap_or(0).to_le_bytes());
    rows[2][32..40].copy_from_slice(
        &value
            .frontier_word_end
            .map(number)
            .transpose()?
            .unwrap_or(0)
            .to_le_bytes(),
    );
    // 绘制语义与原item同一固定记录封存；不增加正文、样式表或额外IO记录。
    rows[2][40..44].copy_from_slice(&value.presentation.bits().to_le_bytes());
    rows[2][44..48].copy_from_slice(&value.paragraph_inset_26_6.to_le_bytes());
    // 两个实际 UBA seam 复用既有目录保留位；不扩大记录或新建按 item 增长的内存索引。
    for (index, levels) in value.shaping_seams.iter().enumerate() {
        if let Some((left, right)) = levels {
            rows[2][48] |= 1 << index;
            rows[2][49 + index * 2] = *left;
            rows[2][50 + index * 2] = *right;
        }
    }
    rows[2][56] = ITEM_END;
    Ok(rows)
}
fn decode_directory(rows: [[u8; 64]; 3]) -> Result<Directory> {
    if rows[0][56] != ITEM
        || rows[1][56] != STYLE
        || rows[2][56] != ITEM_END
        || rows[0][59] > 3
        || rows[1][10..12]
            .iter()
            .chain(rows[1][57..].iter())
            .chain(rows[2][53..56].iter())
            .chain(rows[2][57..].iter())
            .any(|value| *value != 0)
    {
        return Err(Error::InconsistentCoverage);
    }
    let direction = match rows[0][58] {
        0 => MarkdownShapingDirection::LeftToRight,
        1 => MarkdownShapingDirection::RightToLeft,
        _ => return Err(Error::InconsistentCoverage),
    };
    let origin = u64_at(&rows[1], 32);
    let facts = MarkdownLineItemFacts {
        source_utf16: usize_at(&rows[0], 0)?..usize_at(&rows[0], 8)?,
        paragraph_utf16: usize_at(&rows[1], 16)?..usize_at(&rows[1], 24)?,
        script: rows[0][60..64].try_into().expect("固定4B字段"),
        level: rows[0][57],
        direction,
        points: f32::from_le_bytes(rows[1][12..16].try_into().expect("固定4B字段")),
        beginning_of_text: rows[0][59] & 1 != 0,
        end_of_text: rows[0][59] & 2 != 0,
    };
    let kind = super::MarkdownComposedLinePartKind::decode(rows[1][54])?;
    if rows[2][48] > 3 {
        return Err(Error::InconsistentCoverage);
    }
    let mut shaping_seams = [None; 2];
    for (index, seam) in shaping_seams.iter_mut().enumerate() {
        let levels = (rows[2][49 + index * 2], rows[2][50 + index * 2]);
        if rows[2][48] & (1 << index) == 0 {
            if levels != (0, 0) {
                return Err(Error::InconsistentCoverage);
            }
            continue; // 无许可的保留位必须仍为空，不能把损坏目录解释成普通端点。
        }
        if kind != super::MarkdownComposedLinePartKind::Glyphs
            || levels.0 == levels.1
            || levels.0 > 126
            || levels.1 > 126
            || (if index == 0 { levels.1 } else { levels.0 }) != facts.level
        {
            return Err(Error::InconsistentCoverage);
        }
        *seam = Some(levels);
    }
    let tab_interval = match (rows[1][55], i64_at(&rows[2], 24)) {
        (0, 0) => None,
        (1, value) if value >= 0 => Some(value),
        _ => return Err(Error::InconsistentCoverage),
    };
    if kind != super::MarkdownComposedLinePartKind::Tab && tab_interval.is_some() {
        return Err(Error::InconsistentCoverage);
    }
    let frontier_word_end = match u64_at(&rows[2], 32) {
        0 => None,
        _ => Some(usize_at(&rows[2], 32)?),
    };
    if frontier_word_end.is_some_and(|end| {
        kind != super::MarkdownComposedLinePartKind::Glyphs
            || end < facts.source_utf16.end
            || end > facts.paragraph_utf16.end
    }) {
        return Err(Error::InconsistentCoverage);
    } // 最后真实短片可等于原词末端；任何consumer仍核同Source/Font/origin，不据数字扩大源覆盖。
    let paragraph_inset_26_6 =
        i32::from_le_bytes(rows[2][44..48].try_into().expect("固定4B缩进字段"));
    if paragraph_inset_26_6 < 0
        || (facts.source_utf16.is_empty()
            && kind != super::MarkdownComposedLinePartKind::ParagraphBreak)
        || facts.source_utf16.start < facts.paragraph_utf16.start
        || facts.source_utf16.end > facts.paragraph_utf16.end
        || !facts.points.is_finite()
        || (facts.points <= 0.0 && kind != super::MarkdownComposedLinePartKind::TableRow)
        || facts.level > 126
        || origin == 0
        || origin != u64_at(&rows[2], 16)
        || rows[1][52] > 64
        || rows[1][53] > 64
        || facts.level & 1 != u8::from(direction == MarkdownShapingDirection::RightToLeft)
        || facts.beginning_of_text != (facts.source_utf16.start == facts.paragraph_utf16.start)
        || facts.end_of_text != (facts.source_utf16.end == facts.paragraph_utf16.end)
    {
        return Err(Error::InconsistentCoverage);
    }
    let previous = if u64_at(&rows[0], 48) == NONE {
        None
    } else {
        Some(usize_at(&rows[0], 48)?)
    };
    Ok(Directory {
        paragraph_inset_26_6,
        facts,
        presentation: crate::MarkdownPresentationStyles::from_bits(u32::from_le_bytes(
            rows[2][40..44].try_into().expect("固定4B样式字段"),
        ))
        .ok_or(Error::InconsistentCoverage)?,
        font: FontKey {
            generation: u64_at(&rows[1], 0),
            slot: u16::from_le_bytes(rows[1][8..10].try_into().expect("固定2B字段")),
        },
        glyph_start: usize_at(&rows[0], 16)?,
        glyph_count: usize_at(&rows[0], 24)?,
        cluster_start: usize_at(&rows[0], 32)?,
        cluster_count: usize_at(&rows[0], 40)?,
        previous,
        origin,
        style_start: usize_at(&rows[1], 40)?,
        style_rows: usize::try_from(u32::from_le_bytes(
            rows[1][48..52].try_into().expect("固定4B字段"),
        ))
        .map_err(|_| Error::Overflow)?,
        features: usize::from(rows[1][52]),
        language_bytes: usize::from(rows[1][53]),
        x_advance: i64_at(&rows[2], 0),
        y_advance: i64_at(&rows[2], 8),
        kind,
        tab_interval,
        frontier_word_end,
        shaping_seams,
    })
}

impl From<LayoutIndexError> for Error {
    fn from(error: LayoutIndexError) -> Self {
        match error {
            LayoutIndexError::InvalidRecord => Self::InconsistentCoverage,
            LayoutIndexError::Overflow => Self::Overflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}

#[cfg(test)]
mod tests;
