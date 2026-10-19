//! 候选只保原弱输入权限和一个可续绘图会话；主队列闭包不延长它的生命期。

use super::*;
use crate::native_surface::viewport_pixels::{
    CertifiedMaskPairs, PreparedTileEncodingPlan, WritableTile,
};

/// 与同步SceneUse相同的原事实，但不能把Input或delegate强边保存到后续主队列轮次。
struct Ticket {
    input: Weak<NativeMarkdownPagedInput>,
    delegate: Weak<dyn NativeMarkdownPagedTextViewDelegate>,
    state: NativeMarkdownPagedInputState,
    epoch: u64,
    geometry_epoch: u64,
}
impl Ticket {
    fn new(scene: &SceneUse) -> Self {
        Self {
            input: Rc::downgrade(&scene.origin.input),
            delegate: Rc::downgrade(&scene.origin.delegate),
            state: scene.origin.state,
            epoch: scene.origin.epoch,
            geometry_epoch: scene.origin.geometry_epoch,
        }
    }
    fn current(
        &self,
        view: &NativeMarkdownPagedTextView,
        key: &NativeMarkdownPagedSceneKey,
    ) -> bool {
        let Some(input) = self.input.upgrade() else {
            return false;
        };
        let Some(delegate) = self.delegate.upgrade() else {
            return false;
        };
        let origin = Origin {
            input,
            delegate,
            state: self.state,
            epoch: self.epoch,
            geometry_epoch: self.geometry_epoch,
        };
        origin.current(view) && origin.delegate.scene_current(key) && origin.current(view)
    }
}

pub(super) struct Candidate {
    pub serial: u64,
    pub key: NativeMarkdownPagedSceneKey,
    pub geometry: Geometry,
    ticket: Ticket,
    scene: Rc<dyn NativeMarkdownPagedTextScene>,
    session: Option<Box<dyn NativeMarkdownPagedScenePaintSession>>,
    writable: Option<WritableTile>,
    staging: Box<Staging>,
    bitmap: TiledBitmap,
    work: NativeMarkdownPagedScenePaintWork,
    reusable: std::rc::Weak<TileFrame>,
}

/// 每个候选只有一个有限准备槽，不随正文、行或主队列让出次数扩容。
#[derive(Default)]
struct Staging {
    prepared: Option<PreparedTileEncodingPlan>,
    certificates: [Option<CertifiedMaskPairs>; 4],
    next_region: usize,
    calibrations: calibration::Calibrations,
}
pub(super) enum Advance {
    Pending(Candidate),
    Complete(Complete),
}
pub(super) struct Complete {
    pub frame: Rc<TileFrame>,
    pub key: NativeMarkdownPagedSceneKey,
    pub geometry: Geometry,
    serial: u64,
    ticket: Ticket,
    pub(super) scene: Rc<dyn NativeMarkdownPagedTextScene>,
}
impl Complete {
    pub(super) fn current(&self, surface: &CpuSurface, view: &NativeMarkdownPagedTextView) -> bool {
        surface.active_serial.get() == Some(self.serial)
            && self.scene.key().same(&self.key)
            && self.ticket.current(view, &self.key)
            && surface.active_serial.get() == Some(self.serial)
    }
    pub(super) fn validate(
        &self,
        surface: &CpuSurface,
        view: &NativeMarkdownPagedTextView,
    ) -> Result<()> {
        let current = || self.current(surface, view);
        self.scene.validate_ready(&current)?;
        let geometry = surface.geometry_for(view, &self.key, &current);
        self.scene.validate_ready(&current)?; // Native几何拒绝也必须先检查原Source首错。
        if geometry? != self.geometry {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }
}
impl Candidate {
    pub(super) fn new(
        serial: u64,
        scene: &SceneUse,
        geometry: Geometry,
        pixels: Arc<NativeMarkdownViewportPixels>,
        reusable: std::rc::Weak<TileFrame>,
    ) -> Result<Self> {
        Ok(Self {
            serial,
            key: scene.key.clone(),
            geometry,
            ticket: Ticket::new(scene),
            scene: Rc::clone(&scene.scene),
            bitmap: TiledBitmap::new(geometry.width, geometry.height, pixels)?,
            writable: None,
            staging: Box::default(),
            session: None,
            work: NativeMarkdownPagedScenePaintWork::default(),
            reusable,
        })
    }
    pub(super) fn current(&self, surface: &CpuSurface, view: &NativeMarkdownPagedTextView) -> bool {
        surface.active_serial.get() == Some(self.serial)
            && self.scene.key().same(&self.key)
            && self.ticket.current(view, &self.key)
            && surface.active_serial.get() == Some(self.serial)
    }
    pub(super) fn validate(
        &self,
        surface: &CpuSurface,
        view: &NativeMarkdownPagedTextView,
    ) -> Result<()> {
        let current = || self.current(surface, view);
        self.scene.validate_ready(&current)?; // 原Source首错先于取消；从不领取当前较新Scene。
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let geometry = surface.geometry_for(view, &self.key, &current);
        self.scene.validate_ready(&current)?;
        if geometry? != self.geometry {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }
    pub(super) fn advance(
        mut self,
        surface: &CpuSurface,
        view: &NativeMarkdownPagedTextView,
    ) -> Result<Advance> {
        self.validate(surface, view)?;
        if let Some(prepared) = self.staging.prepared.take() {
            return self.finish_prepared(surface, view, prepared);
        } // 有限原生校准独占下一次主队列轮次，不与最后glyph批次叠加同步工作。
        if self.writable.is_none() {
            let tile = self
                .bitmap
                .next_tile()
                .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
            let writable = self.bitmap.begin_tile(tile);
            let current = || self.current(surface, view);
            self.scene.validate_ready(&current)?;
            let writable = writable?;
            let transform = tile_transform(self.geometry.bounds, self.geometry.scale, tile)?;
            checked(&current, || {
                CGContext::concat_ctm(Some(writable.context()), transform)
            })?;
            checked(&current, || {
                CGContext::clip_to_rect(Some(writable.context()), self.key.request.viewport)
            })?;
            let session =
                Rc::clone(&self.scene).begin_cpu_paint(writable.context_claim(), &current)?;
            self.scene.validate_ready(&current)?;
            self.writable = Some(writable);
            self.session = Some(session);
            self.work = NativeMarkdownPagedScenePaintWork::default();
        } // 每轮至多创建一个tile；预算耗尽从原session cursor续，不从第一glyph重画。
        let session = self
            .session
            .take()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let current = || self.current(surface, view);
        let step = session.step(NativeMarkdownPagedScenePaintBudget::default(), &current);
        self.scene.validate_ready(&current)?; // 即使Native返回撤权，真实Source/Font首错仍优先。
        let step = step?;
        if step.work.glyphs > 256 || step.work.metadata_records > 32 || step.work.batches > 256 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        } // producer违规不能以拆多个实际批次越过一次主队列的同步预算。
        self.work = add(self.work, step.work)?;
        match step.outcome {
            NativeMarkdownPagedScenePaintOutcome::Pending(session) => {
                self.session = Some(session);
                self.validate(surface, view)?;
                Ok(Advance::Pending(self))
            }
            NativeMarkdownPagedScenePaintOutcome::Complete(completion) => {
                let writable = self
                    .writable
                    .take()
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
                if !completion.permits(&self.key, writable.context())
                    || completion.work() != self.work
                {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                drop(completion); // 归还完成票显式claim；WritableTile的最后context待finish实际callback后才封存。
                self.validate(surface, view)?;
                let prepared = self
                    .bitmap
                    .prepare_tile(writable)
                    .and_then(|prepared| self.bitmap.plan_encoding(prepared));
                self.validate(surface, view)?; // 最后context/纯扫描/只读转换错误不能覆盖原Source首错。
                self.staging.prepared = Some(prepared?);
                Ok(Advance::Pending(self))
            }
        }
    }

    fn finish_prepared(
        mut self,
        surface: &CpuSurface,
        view: &NativeMarkdownPagedTextView,
        prepared: PreparedTileEncodingPlan,
    ) -> Result<Advance> {
        let count = prepared.region_count();
        if count == 0 || count > 4 || self.staging.next_region >= count {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let index = self.staging.next_region;
        let pairs = if let Some(request) = prepared.mask_request(index)? {
            // 把有限纯事实槽移到借用外，Native/current重入期间没有借用Candidate内部容器。
            let mut calibrations = std::mem::take(&mut self.staging.calibrations);
            let pairs = calibrations.pairs(&request, Arc::clone(&surface.pixels), &|| {
                self.validate(surface, view)
            });
            self.staging.calibrations = calibrations;
            self.validate(surface, view)?;
            pairs?
        } else {
            None // 固色、透明或原中性A8不必加载双色校准图，保持旧表示与功能。
        };
        self.staging.certificates[index] = pairs;
        self.staging.next_region += 1;
        if self.staging.next_region < count {
            self.staging.prepared = Some(prepared);
            self.validate(surface, view)?;
            return Ok(Advance::Pending(self)); // 同一个原context只完成过一次；每轮至多处理一个子区Native请求。
        }
        let current = || self.current(surface, view);
        let prior = surface.reusable_frame(view, &self.reusable, self.geometry, &current);
        self.validate(surface, view)?; // 旧Native关系出错仍拒绝，不能被像素优化洗成cache miss。
        let prior = prior?;
        let certificates = self.staging.certificates.each_ref().map(Option::as_ref);
        let sealed =
            self.bitmap
                .finish_encoding(prepared, &certificates[..count], prior.as_deref());
        drop(prior); // 两图的逐byte比较结束就归还旧帧，不跨主队列或发布事务。
        self.staging.certificates = Default::default();
        self.staging.next_region = 0;
        self.validate(surface, view)?;
        sealed?;
        if self.bitmap.next_tile().is_some() {
            Ok(Advance::Pending(self)) // 所有tile完成前仍不展示部分候选。
        } else {
            drop(std::mem::take(&mut self.staging.calibrations)); // 发布前实际归还校准纯表页，不留在Published或idle。
            self.validate(surface, view)?;
            if surface.pixels.observer().snapshot().release_failed {
                return Err(NativeMarkdownPagedTextViewError::Raster(
                    "校准纯颜色表未能实际归还".into(),
                ));
            }
            Ok(Advance::Complete(Complete {
                frame: Rc::new(self.bitmap.finish()?),
                key: self.key,
                geometry: self.geometry,
                serial: self.serial,
                ticket: self.ticket,
                scene: self.scene,
            }))
        }
    }
}
fn add(
    a: NativeMarkdownPagedScenePaintWork,
    b: NativeMarkdownPagedScenePaintWork,
) -> Result<NativeMarkdownPagedScenePaintWork> {
    let sum = |a: usize, b: usize| {
        a.checked_add(b)
            .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)
    };
    Ok(NativeMarkdownPagedScenePaintWork {
        glyphs: sum(a.glyphs, b.glyphs)?,
        batches: sum(a.batches, b.batches)?,
        metadata_records: sum(a.metadata_records, b.metadata_records)?,
    })
}
