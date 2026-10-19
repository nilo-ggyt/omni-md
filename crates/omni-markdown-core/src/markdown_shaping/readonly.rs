//! 原Ready任务的只读交付：只顺序复制固定glyph元数据，不复制正文或移交可变shaper数组。
//!
//! 这里只证明同一个不可变输出的来源；Native仍须核自己的活动文档、代次和输入权限。
//! worker完成seal后归还原Job，主线程只持唯一匿名File及必要的原Source/Font共享强边。

use super::{
    MarkdownShapedGlyph, MarkdownShapingCancellation, MarkdownShapingDirection,
    MarkdownShapingError, MarkdownShapingJob, MarkdownShapingStatus,
};
use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutFont, MarkdownLayoutFontAuthority,
    MarkdownLayoutFontResourceObserver, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver, MarkdownOriginalBackingObserver,
};
use std::{
    ops::Range,
    sync::{Arc, Weak},
};
use thiserror::Error;

mod store;
#[cfg(test)]
mod tests;
use store::Store;
pub use store::{MarkdownShapedSpoolIoError, MarkdownShapedSpoolIoOperation};

/// 单次构建只消费一个固定批次；不在一次主线程调用中隐式扫描任意长run。
pub const MARKDOWN_SHAPED_SPOOL_STEP: usize = 256;
const RECORD: usize = 32;

#[derive(Debug, Error)]
pub enum MarkdownShapedSpoolError {
    #[error("只读字形不属于原任务输出")]
    OutputMismatch,
    #[error("只读字形Source权限不匹配")]
    SourceMismatch,
    #[error("只读字形Font权限不匹配")]
    FontMismatch,
    #[error("只读字形配额、坐标或记录无效")]
    InvalidRequest,
    #[error("只读字形尚未完成全部写入")]
    Pending,
    #[error("只读字形已取消")]
    Cancelled,
    #[error("只读字形已永久撤权")]
    Poisoned,
    #[error("原字形任务不可用：{0}")]
    Shaping(#[from] MarkdownShapingError),
    #[error("原只读字形Source不可用：{0}")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("只读字形文件操作失败：{0}")]
    Storage(#[from] MarkdownShapedSpoolIoError),
    #[cfg(feature = "paged-safe-glyph-slices")]
    #[error("原安全字形切片已失权：{0}")]
    SafeSlice(#[from] crate::MarkdownSafeGlyphSliceError),
}
type Result<T> = std::result::Result<T, MarkdownShapedSpoolError>;

/// 指向原Job输出nonce；相同文字、字体、范围的新Job不能取得这张票。
#[derive(Clone)]
pub struct MarkdownShapedSpoolAuthority(Weak<()>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownShapedSpoolStatus {
    Building,
    AwaitingSeal,
    Ready,
    Cancelled,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedSpoolStep {
    pub glyphs: usize,
    pub status: MarkdownShapedSpoolStatus,
}

/// 只有固定事实，不保活File、Source、Font或Job；字体底层其他Blob别名须另核自己的VM收据。
#[derive(Clone)]
pub struct MarkdownShapedSpoolResourceObserver {
    file: store::Observer,
    source: MarkdownLayoutTextSourceObserver,
    font: MarkdownLayoutFontResourceObserver,
    backing: Option<MarkdownOriginalBackingObserver>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownShapedSpoolResources {
    pub file_open: bool,
    pub close_succeeded: bool,
    pub cleanup_failed: bool,
    pub first_error: Option<MarkdownShapedSpoolIoError>,
    pub source_payload_released: bool,
    pub font_payload_released: bool,
}
impl MarkdownShapedSpoolResourceObserver {
    #[must_use]
    /// 多个原子字段不是运行中事务快照；worker join/owner close后才核最终资源组合。
    pub fn snapshot(&self) -> MarkdownShapedSpoolResources {
        let file = self.file.snapshot();
        MarkdownShapedSpoolResources {
            file_open: file.file_open,
            close_succeeded: file.close_succeeded,
            cleanup_failed: file.cleanup_failed,
            first_error: file.first_error,
            source_payload_released: self.source.payload_released(),
            font_payload_released: self.font.payload_released(),
        }
    }
    fn failure(&self) -> Result<()> {
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        } // 原Source错误优先于文件/取消；观察者不需要保活原映射来读取首因。
        if let Some(error) = self.file.snapshot().first_error {
            return Err(error.into());
        }
        Ok(())
    }
}

struct Origin {
    nonce: Arc<()>,
    source: MarkdownLayoutTextSource,
    source_authority: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFont,
    font_authority: MarkdownLayoutFontAuthority,
    cancellation: MarkdownShapingCancellation,
    #[cfg(feature = "paged-safe-glyph-slices")]
    measure_cancellation: Option<crate::MarkdownSoftLineMeasureCancellation>,
}
impl Origin {
    fn permits(
        &self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<()> {
        self.source.health()?;
        // 原输出也服从seal之后收到的同一个请求位，不能靠新reader绕过取消。
        if self.cancellation.requested() {
            return Err(MarkdownShapedSpoolError::Cancelled);
        }
        #[cfg(feature = "paged-safe-glyph-slices")]
        if self
            .measure_cancellation
            .as_ref()
            .is_some_and(|value| value.is_cancelled())
        {
            return Err(MarkdownShapedSpoolError::Cancelled);
        } // 封口只交接数据owner，不洗掉签发此行时原测量的明确取消请求。
        // 弱票只比较原allocation；文字/范围/字体相同不能代替原job nonce。
        if !Weak::ptr_eq(&authority.0, &Arc::downgrade(&self.nonce)) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        // 同一冻结Source强边是读取资格，活动文档的较新代次仍由Native调用方另核。
        if !self.source.permits(source) || !self.source.permits(&self.source_authority) {
            return Err(MarkdownShapedSpoolError::SourceMismatch);
        }
        // 保留精确Font payload身份；PSName或轴值比较不能授予另一次resolve的权限。
        if !self.font.permits(font) || !self.font.permits(&self.font_authority) {
            return Err(MarkdownShapedSpoolError::FontMismatch);
        }
        Ok(())
    }
}
struct Payload {
    file: Store,
    origin: Origin,
}

/// Builder只在原worker逐批推进；只有完整seal后的另一具体类型才能读glyph。
pub struct MarkdownShapedSpoolBuilder {
    payload: Option<Payload>,
    resources: MarkdownShapedSpoolResourceObserver,
    authority: MarkdownShapedSpoolAuthority,
    source_range: Range<usize>,
    count: usize,
    job_count: usize,
    glyph_start: usize,
    #[cfg(feature = "paged-safe-glyph-slices")]
    safe_slice: Option<crate::MarkdownSafeGlyphSlice>,
    consumed: usize,
    generation: u64,
    direction: MarkdownShapingDirection,
    points: f32,
    status: MarkdownShapedSpoolStatus,
}

impl MarkdownShapedSpoolBuilder {
    /// 先核原Ready/EOF再创建唯一FD；此构造不推进shape、不复制正文、不签Native几何。
    pub fn new(
        job: &mut MarkdownShapingJob,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        let profile = job.profile()?;
        if job.status() != MarkdownShapingStatus::Ready {
            return Err(MarkdownShapedSpoolError::Pending); // 未Ready时不创建匿名FD或借用部分Buffer。
        }
        job.glyph(source, font, profile.glyph_count)?;
        let value = job
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        let file = Store::new()?;
        let resources = MarkdownShapedSpoolResourceObserver {
            file: file.observer(),
            source: value.source.resource_observer(),
            font: value.font.resource_observer(),
            backing: value.source.original_backing_observer(),
        };
        let authority = MarkdownShapedSpoolAuthority(job.index_identity());
        Ok(Self {
            payload: Some(Payload {
                file,
                origin: Origin {
                    nonce: Arc::clone(&job.identity),
                    source: value.source.clone(),
                    source_authority: source.clone(),
                    font: value.font.clone(),
                    font_authority: font.clone(),
                    cancellation: job.cancellation(),
                    #[cfg(feature = "paged-safe-glyph-slices")]
                    measure_cancellation: None,
                },
            }),
            resources,
            authority,
            source_range: job.source_range(),
            count: profile.glyph_count,
            job_count: profile.glyph_count,
            glyph_start: 0,
            #[cfg(feature = "paged-safe-glyph-slices")]
            safe_slice: None,
            consumed: 0,
            generation: profile.source_generation,
            direction: job.direction(),
            points: job.points(),
            status: if profile.glyph_count == 0 {
                MarkdownShapedSpoolStatus::AwaitingSeal
            } else {
                MarkdownShapedSpoolStatus::Building
            },
        })
    }

    /// 消费原工厂签发的安全子范围；不接受公开Range，不新建shaper或复制原剩余run。
    #[cfg(feature = "paged-safe-glyph-slices")]
    pub fn from_safe_slice(
        job: &mut MarkdownShapingJob,
        mut slice: crate::MarkdownSafeGlyphSlice,
    ) -> Result<Self> {
        slice.require(job)?; // 在创建FD前核实际原owner，未Ready/撤权的候选不产生输出资源。
        let facts = slice.facts();
        let mut builder = Self::new(job, &slice.source, &slice.font)?;
        builder.source_range = facts.source_utf16;
        builder.glyph_start = facts.visual_glyphs.start;
        builder.count = facts.visual_glyphs.len();
        builder.status = if builder.count == 0 {
            MarkdownShapedSpoolStatus::AwaitingSeal
        } else {
            MarkdownShapedSpoolStatus::Building
        };
        builder
            .payload
            .as_mut()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?
            .origin
            .measure_cancellation = Some(slice.cancellation());
        builder.safe_slice = Some(slice);
        if let Err(error) = builder.guard(job) {
            return builder.fail(error);
        }
        Ok(builder)
    }

    fn guard(&mut self, job: &mut MarkdownShapingJob) -> Result<()> {
        self.resources.failure()?;
        #[cfg(feature = "paged-safe-glyph-slices")]
        if let Some(slice) = self.safe_slice.as_mut() {
            slice.require(job)?;
        }
        let value = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        value.origin.permits(
            &self.authority,
            &value.origin.source_authority,
            &value.origin.font_authority,
        )?;
        // Builder虽可由Rust移动，继续写入始终需要原Job，不能拿另一个Ready结果续接。
        if !Arc::ptr_eq(&value.origin.nonce, &job.identity) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        job.glyph(
            &value.origin.source_authority,
            &value.origin.font_authority,
            self.job_count,
        )?;
        let profile = job.profile()?;
        // profile只是原job聚合；任一事实改变都不能与已经写入的前缀拼成完整文件。
        if profile.glyph_count != self.job_count || profile.source_generation != self.generation {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: MarkdownShapedSpoolError) -> Result<T> {
        self.status = if matches!(error, MarkdownShapedSpoolError::Cancelled) {
            MarkdownShapedSpoolStatus::Cancelled
        } else {
            MarkdownShapedSpoolStatus::Poisoned
        };
        drop(self.payload.take()); // 先结束借用再真实close和交还必要Source/Font强边。
        if let Err(cause) = self.resources.failure() {
            self.status = MarkdownShapedSpoolStatus::Poisoned;
            return Err(cause); // Drop期间的真实首IO也不能被干净取消遮掉。
        }
        Err(error)
    }

    /// 明确的原worker预算：失败销毁候选，实际FD归还收据仍可由独立observer核验。
    pub fn step(
        &mut self,
        job: &mut MarkdownShapingJob,
        budget: usize,
    ) -> Result<MarkdownShapedSpoolStep> {
        let result = (|| {
            self.guard(job)?;
            if budget == 0 || budget > MARKDOWN_SHAPED_SPOOL_STEP {
                return Err(MarkdownShapedSpoolError::InvalidRequest);
            }
            let count = budget.min(self.count - self.consumed);
            let mut bytes = [0u8; RECORD * MARKDOWN_SHAPED_SPOOL_STEP];
            for offset in 0..count {
                let value = self
                    .payload
                    .as_ref()
                    .ok_or(MarkdownShapedSpoolError::Poisoned)?;
                let glyph = job
                    .glyph(
                        &value.origin.source_authority,
                        &value.origin.font_authority,
                        self.glyph_start + self.consumed + offset,
                    )?
                    .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
                if !self.source_range.contains(&glyph.source_utf16_cluster) {
                    return Err(MarkdownShapedSpoolError::OutputMismatch);
                } // 子范围仍用全局源cluster；RTL只选择原视觉区间，绝不重编号或吞掉越界记录。
                bytes[offset * RECORD..(offset + 1) * RECORD].copy_from_slice(&encode(glyph)?);
            } // 栈上只存本批≤256个真实7字段，任意失败都不把前缀写成完整批次。
            self.guard(job)?;
            self.payload
                .as_mut()
                .ok_or(MarkdownShapedSpoolError::Poisoned)?
                .file
                .append(&bytes[..count * RECORD])?;
            self.guard(job)?;
            self.consumed += count;
            if self.consumed == self.count {
                self.status = MarkdownShapedSpoolStatus::AwaitingSeal;
            }
            Ok(MarkdownShapedSpoolStep {
                glyphs: count,
                status: self.status,
            })
        })();
        match result {
            Ok(step) => Ok(step),
            Err(error) => self.fail(error),
        }
    }

    /// 消费完整writer；调用方随后必须归还原Job并join worker，不能把seal当成Job资源已释放。
    pub fn seal(mut self, job: &mut MarkdownShapingJob) -> Result<MarkdownShapedSpool> {
        let result = (|| {
            self.guard(job)?;
            if self.status != MarkdownShapedSpoolStatus::AwaitingSeal || self.consumed != self.count
            {
                return Err(MarkdownShapedSpoolError::Pending);
            }
            let bytes = self
                .count
                .checked_mul(RECORD)
                .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
            self.payload
                .as_mut()
                .ok_or(MarkdownShapedSpoolError::Poisoned)?
                .file
                .seal(bytes)?;
            self.guard(job)?; // 同步IO返回后仍核原Job/token；撤权不签发readonly owner。
            Ok(())
        })();
        if let Err(error) = result {
            return self.fail(error);
        }
        Ok(MarkdownShapedSpool {
            payload: self.payload.take(),
            #[cfg(feature = "paged-safe-glyph-slices")]
            slice_identity: Arc::new(()),
            resources: self.resources.clone(),
            authority: self.authority.clone(),
            source_range: self.source_range.clone(),
            count: self.count,
            generation: self.generation,
            direction: self.direction,
            points: self.points,
            status: MarkdownShapedSpoolStatus::Ready,
        })
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapedSpoolResourceObserver {
        self.resources.clone()
    }
}

/// 只有唯一File owner与immutable Source/Font强边；没有Rc、裸指针或unsafe Send实现。
pub struct MarkdownShapedSpool {
    #[cfg(feature = "paged-safe-glyph-slices")]
    slice_identity: Arc<()>,
    payload: Option<Payload>,
    resources: MarkdownShapedSpoolResourceObserver,
    authority: MarkdownShapedSpoolAuthority,
    source_range: Range<usize>,
    count: usize,
    generation: u64,
    direction: MarkdownShapingDirection,
    points: f32,
    status: MarkdownShapedSpoolStatus,
}
impl MarkdownShapedSpool {
    #[must_use]
    pub fn authority(&self) -> MarkdownShapedSpoolAuthority {
        self.authority.clone()
    }
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.count
    }
    #[must_use]
    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }
    #[must_use]
    pub fn source_generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub fn direction(&self) -> MarkdownShapingDirection {
        self.direction
    }
    #[must_use]
    pub fn points(&self) -> f32 {
        self.points
    }
    #[must_use]
    pub fn status(&self) -> MarkdownShapedSpoolStatus {
        self.status
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownShapedSpoolResourceObserver {
        self.resources.clone()
    }

    pub fn glyph(
        &mut self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        index: usize,
    ) -> Result<Option<MarkdownShapedGlyph>> {
        if index >= self.count {
            self.read_batch(authority, source, font, self.count..self.count, &mut [])?;
            return Ok(None); // 已sealed真实EOF同样经过完整原票，绝不把Building前缀当EOF。
        }
        let mut output = [empty_glyph(); 1];
        self.read_batch(authority, source, font, index..index + 1, &mut output)?;
        Ok(Some(output[0]))
    }

    /// 一个实际pread批次最多256glyph；没有常驻页缓存或完整run的Vec。
    ///
    /// # Errors
    /// 任何失败都撤销整个reader；`output`只是调用方候选缓冲，Err时必须全部弃用。
    /// 即使pread或最后原权限核验失败时已经写过候选，也绝不把部分值签成Ready。
    pub fn read_batch(
        &mut self,
        authority: &MarkdownShapedSpoolAuthority,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<usize> {
        let result = (|| {
            self.resources.failure()?;
            let value = self.payload.as_ref().ok_or_else(|| {
                if self.status == MarkdownShapedSpoolStatus::Cancelled {
                    MarkdownShapedSpoolError::Cancelled
                } else {
                    MarkdownShapedSpoolError::Poisoned
                }
            })?;
            value.origin.permits(authority, source, font)?;
            if range.start > range.end
                || range.end > self.count
                || range.len() != output.len()
                || output.len() > MARKDOWN_SHAPED_SPOOL_STEP
            {
                return Err(MarkdownShapedSpoolError::InvalidRequest); // 在任何record读取前拒绝大批次/裁剪请求。
            }
            let mut bytes = [0u8; RECORD * MARKDOWN_SHAPED_SPOOL_STEP];
            let used = range.len() * RECORD;
            if used != 0 {
                let offset = range
                    .start
                    .checked_mul(RECORD)
                    .ok_or(MarkdownShapedSpoolError::InvalidRequest)?;
                value.file.read_into(offset, &mut bytes[..used])?;
            } // 真实空批次不制造glyph或IO，但末尾原权限门仍适用于canonical EOF。
            for record in bytes[..used].as_chunks::<RECORD>().0 {
                let glyph = decode(*record)?;
                if !self.source_range.contains(&glyph.source_utf16_cluster) {
                    return Err(MarkdownShapedSpoolError::InvalidRequest);
                }
            } // 完整预检本批后再触碰output，损坏尾record不能发布已知前缀。
            self.resources.failure()?;
            value.origin.permits(authority, source, font)?;
            for (target, record) in output.iter_mut().zip(bytes[..used].as_chunks::<RECORD>().0) {
                *target = decode(*record)?;
            }
            value.origin.permits(authority, source, font)?; // 返回前仍是原票；Err时调用方丢弃整批候选。
            Ok(output.len())
        })();
        match result {
            Ok(count) => Ok(count),
            Err(error) => {
                self.status = if matches!(error, MarkdownShapedSpoolError::Cancelled) {
                    MarkdownShapedSpoolStatus::Cancelled
                } else {
                    MarkdownShapedSpoolStatus::Poisoned
                };
                drop(self.payload.take());
                if let Err(cause) = self.resources.failure() {
                    self.status = MarkdownShapedSpoolStatus::Poisoned;
                    return Err(cause);
                }
                Err(error)
            }
        }
    }

    /// 只归还本owner资源，不表示功能成功；底层共享Source/Font映射由各自真实收据核验。
    pub fn close(mut self) -> Result<()> {
        drop(self.payload.take());
        self.resources.failure() // 只有实际close返回成功才由observer报告close_succeeded。
    }
}

fn empty_glyph() -> MarkdownShapedGlyph {
    MarkdownShapedGlyph {
        glyph_id: 0,
        source_utf16_cluster: 0,
        flags: 0,
        x_advance: 0,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
    }
}

fn encode(glyph: MarkdownShapedGlyph) -> Result<[u8; RECORD]> {
    let mut bytes = [0; RECORD];
    bytes[..4].copy_from_slice(&glyph.glyph_id.to_le_bytes());
    bytes[4..8].copy_from_slice(&glyph.flags.to_le_bytes());
    bytes[8..16].copy_from_slice(
        &u64::try_from(glyph.source_utf16_cluster)
            .map_err(|_| MarkdownShapedSpoolError::InvalidRequest)?
            .to_le_bytes(),
    );
    for (chunk, value) in bytes[16..].as_chunks_mut::<4>().0.iter_mut().zip([
        glyph.x_advance,
        glyph.y_advance,
        glyph.x_offset,
        glyph.y_offset,
    ]) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}
fn decode(bytes: [u8; RECORD]) -> Result<MarkdownShapedGlyph> {
    let u32_at = |range: Range<usize>| {
        u32::from_le_bytes(bytes[range].try_into().expect("固定4B record字段"))
    };
    let i32_at = |range: Range<usize>| {
        i32::from_le_bytes(bytes[range].try_into().expect("固定4B record字段"))
    };
    Ok(MarkdownShapedGlyph {
        glyph_id: u32_at(0..4),
        flags: u32_at(4..8),
        source_utf16_cluster: usize::try_from(u64::from_le_bytes(
            bytes[8..16].try_into().expect("固定8B cluster"),
        ))
        .map_err(|_| MarkdownShapedSpoolError::InvalidRequest)?,
        x_advance: i32_at(16..20),
        y_advance: i32_at(20..24),
        x_offset: i32_at(24..28),
        y_offset: i32_at(28..32),
    })
}

/// 具体sealed文件的纯弱身份；原Job的多个spool不能互相替代此票。
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MarkdownShapedSpoolInstance(Weak<()>);
#[cfg(feature = "paged-safe-glyph-slices")]
impl MarkdownShapedSpool {
    pub(crate) fn instance_identity(&self) -> MarkdownShapedSpoolInstance {
        MarkdownShapedSpoolInstance(Arc::downgrade(&self.slice_identity))
    }
    pub(crate) fn require_instance(
        &mut self,
        instance: &MarkdownShapedSpoolInstance,
    ) -> Result<()> {
        self.resources.failure()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        payload.origin.source.health()?;
        if !Weak::ptr_eq(&instance.0, &Arc::downgrade(&self.slice_identity)) {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        let source = payload.origin.source_authority.clone();
        let font = payload.origin.font_authority.clone();
        let authority = self.authority.clone();
        self.read_batch(&authority, &source, &font, self.count..self.count, &mut [])?;
        Ok(()) // 返回弱身份不是Ready；实际使用始终重核原Source/取消/FD首错及原reader状态。
    }
}

/// 历史item收据只保弱权限和固定事实；具体spool nonce不能由同Job的另一输出替代。
#[cfg(feature = "paged-safe-glyph-slices")]
#[derive(Clone)]
pub(crate) struct MarkdownSpoolSliceOrigin {
    pub instance: MarkdownShapedSpoolInstance,
    pub authority: MarkdownShapedSpoolAuthority,
    pub source: MarkdownLayoutTextAuthority,
    pub font: MarkdownLayoutFontAuthority,
    pub range: Range<usize>,
    pub count: usize,
    pub generation: u64,
    pub direction: MarkdownShapingDirection,
    pub points: u32,
}
#[cfg(feature = "paged-safe-glyph-slices")]
impl MarkdownShapedSpool {
    pub(crate) fn safe_slice_origin(
        &mut self,
        source: &MarkdownLayoutTextAuthority,
    ) -> Result<MarkdownSpoolSliceOrigin> {
        self.resources.failure()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        let font = payload.origin.font_authority.clone();
        let authority = self.authority.clone();
        self.read_batch(&authority, source, &font, self.count..self.count, &mut [])?;
        Ok(MarkdownSpoolSliceOrigin {
            instance: self.instance_identity(),
            authority,
            source: source.clone(),
            font,
            range: self.source_range.clone(),
            count: self.count,
            generation: self.generation,
            direction: self.direction,
            points: self.points.to_bits(),
        }) // EOF仍先经过实际Source/Font/文件首错误门，不能从旧裸metadata构造新授权。
    }
    pub(crate) fn require_safe_slice_origin(
        &mut self,
        origin: &MarkdownSpoolSliceOrigin,
    ) -> Result<()> {
        self.resources.failure()?;
        if let Some(payload) = &self.payload {
            payload.origin.source.health()?;
        }
        self.require_instance(&origin.instance)?;
        if origin.range != self.source_range
            || origin.count != self.count
            || origin.generation != self.generation
            || origin.direction != self.direction
            || origin.points != self.points.to_bits()
        {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        self.read_batch(
            &origin.authority,
            &origin.source,
            &origin.font,
            self.count..self.count,
            &mut [],
        )?;
        Ok(()) // 三种弱权限均重核；同名字体、同Job第二spool或同文字Source没有替代资格。
    }
    pub(crate) fn require_safe_slice_job(
        &mut self,
        origin: &MarkdownSpoolSliceOrigin,
        job: &mut MarkdownShapingJob,
    ) -> Result<()> {
        self.require_safe_slice_origin(origin)?;
        let payload = self
            .payload
            .as_ref()
            .ok_or(MarkdownShapedSpoolError::Poisoned)?;
        if !Weak::ptr_eq(
            &Arc::downgrade(&payload.origin.nonce),
            &job.index_identity(),
        ) || origin.range != job.source_range()
        {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        job.glyph(&origin.source, &origin.font, origin.count)?;
        if job.profile()?.glyph_count != origin.count {
            return Err(MarkdownShapedSpoolError::OutputMismatch);
        }
        Ok(()) // 必须在旧Job仍活且真实NeedNextItem时记录，不能日后只凭sameSource补签历史。
    }
}
