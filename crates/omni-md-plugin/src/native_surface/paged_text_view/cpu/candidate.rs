//! 候选只保原弱输入权限和一个可续绘图会话；主队列闭包不延长它的生命期。

use super::*;
use crate::native_surface::viewport_pixels::{
    CertifiedMaskPairs, PreparedTileEncodingPlan, WritableTile,
};

/// 与同步SceneUse相同的原事实，但不能把Input或delegate强边保存到后续主队列轮次。
/// 克隆仍只有弱身份和固定元数据，供本次advance消费Candidate之后核原票的真实换代。
#[derive(Clone)]
pub(super) struct Ticket {
    input: Weak<NativeMarkdownPagedInput>,
    delegate: Weak<dyn NativeMarkdownPagedTextViewDelegate>,
    state: NativeMarkdownPagedInputState,
    epoch: u64,
    geometry_epoch: u64,
}
impl Ticket {
    /// 同步获取和异步候选共享原事实；只保存弱身份，绝不保存后继 Scene 或延长正文资源寿命。
    pub(super) fn new(origin: &Origin) -> Self {
        Self {
            input: Rc::downgrade(&origin.input),
            delegate: Rc::downgrade(&origin.delegate),
            state: origin.state,
            epoch: origin.epoch,
            geometry_epoch: origin.geometry_epoch,
        }
    }
    pub(super) fn current(
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
            interpreted: false, // CPU候选只核绘图票，不能认领原IME解释回调的输入收据。
        };
        origin.current(view) && origin.delegate.scene_current(key) && origin.current(view)
    }

    /// 只有客户、完整输入或几何确已换代，旧CPU票的撤权才是正常退休。
    /// current中的bool还可能来自借用失败、Viewport故障或当前图层关系错误，不能直接
    /// 把!current当作成功；这里不领取后继Scene，也不让清理后的空serial代替身份事实。
    pub(super) fn superseded(&self, view: &NativeMarkdownPagedTextView) -> Result<bool> {
        let ivars = view.ivars();
        if ivars.geometry_failed.get() {
            return Ok(false); // 几何计数失败不是一次合法布局换代，原Revoked必须继续交给失败边界。
        }
        if ivars.closed.get() || ivars.epoch != self.epoch {
            return Ok(true);
        }
        let same_input = ivars
            .input
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedTextViewError::Busy)?
            .as_ref()
            .is_some_and(|input| Weak::ptr_eq(&self.input, &Rc::downgrade(input)));
        let same_delegate = ivars
            .delegate
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedTextViewError::Busy)?
            .as_ref()
            .is_some_and(|delegate| Weak::ptr_eq(&self.delegate, delegate));
        if !same_input || !same_delegate {
            return Ok(true); // 只有原槽真的退出/换客户才退休；没有延长旧Input或delegate的强生命期。
        }
        let input = self
            .input
            .upgrade()
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let state = input.state()?; // Borrowed/StateChanged/Source等真实输入错误不能被bool撤权吞掉。
        Ok(state != self.state || ivars.geometry_epoch.get() != self.geometry_epoch)
    }
}

pub(super) struct Candidate {
    pub serial: u64,
    pub key: NativeMarkdownPagedSceneKey,
    pub geometry: Geometry,
    pub(super) ticket: Ticket,
    pub(super) scene: Rc<dyn NativeMarkdownPagedTextScene>,
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
        surface: &CpuSurface,
        reusable: std::rc::Weak<TileFrame>,
    ) -> Result<Self> {
        let bitmap =
            match TiledBitmap::new(geometry.width, geometry.height, Arc::clone(&surface.pixels)) {
                Err(crate::native_surface::viewport_pixels::PixelError::BudgetExceeded)
                    if surface.clear_calibrations() =>
                {
                    // 此构造到预算拒绝为止没有Native、mmap或绘图副作用。先实际归还可选表页，
                    // 再核同一个候选的原预检，不能因优化常驻页缩小用户原本可绘制的视口。
                    if surface.pixels.observer().snapshot().release_failed {
                        return Err(NativeMarkdownPagedTextViewError::Raster(
                            "预算预检前校准纯颜色表未能实际归还".into(),
                        ));
                    }
                    TiledBitmap::new(geometry.width, geometry.height, Arc::clone(&surface.pixels))?
                }
                other => other?,
            };
        surface.calibrations.borrow_mut().begin_frame(); // 纯Rust额度转移，不查询新Scene或续原权限。
        Ok(Self {
            serial,
            key: scene.key.clone(),
            geometry,
            ticket: Ticket::new(&scene.origin),
            scene: Rc::clone(&scene.scene),
            bitmap,
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
            let mut calibrations = std::mem::take(&mut *surface.calibrations.borrow_mut());
            let pairs = calibrations.pairs(&request, Arc::clone(&surface.pixels), &|| {
                self.validate(surface, view)
            });
            let current = self.validate(surface, view);
            if pairs.is_ok() && current.is_ok() {
                // Native可关闭或撤销原候选；只有原serial/Source/几何全部仍成立才归还纯表。
                // 取出期间Surface槽为空，重入取消不会借用冲突；失权局部值直接drop，不复活旧页。
                drop(surface.calibrations.replace(calibrations));
            } else {
                drop(calibrations);
            }
            current?; // 真实Source/Font首错仍高于校准拒绝或原生撤权。
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
        // 这里只借原活帧的不可变图片作精确内容比较；宽度改变不继承其位置、Scene 或输入票。
        let prior = surface.reusable_frame(view, &self.reusable, &current);
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
