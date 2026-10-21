//! A candidate owns its original weak input authority and resumable paint session; queued closures do
//! not retain it.

use super::*;
use crate::native_surface::viewport_pixels::{
    CertifiedMaskPairs, PreparedTileEncodingPlan, WritableTile,
};

/// Carry the original synchronous SceneUse facts without retaining Input or delegate across main-queue
/// turns.
/// Clones keep weak identities and finite metadata so advance can distinguish actual supersession after
/// consuming the candidate.
#[derive(Clone)]
pub(in super::super) struct Ticket {
    input: Weak<NativeMarkdownPagedInput>,
    delegate: Weak<dyn NativeMarkdownPagedTextViewDelegate>,
    state: NativeMarkdownPagedInputState,
    epoch: u64,
    geometry_epoch: u64,
}
impl Ticket {
    /// Synchronous acquisition and asynchronous candidates share original facts using weak identities,
    /// without successor Scenes or extended document ownership.
    pub(in super::super) fn new(origin: &Origin) -> Self {
        Self {
            input: Rc::downgrade(&origin.input),
            delegate: Rc::downgrade(&origin.delegate),
            state: origin.state,
            epoch: origin.epoch,
            geometry_epoch: origin.geometry_epoch,
        }
    }
    pub(in super::super) fn current(
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
            interpreted: false, // A CPU paint ticket cannot claim the input receipt belonging to an original IME interpretation callback.
        };
        origin.current(view) && origin.delegate.scene_current(key) && origin.current(view)
    }

    /// Treat old CPU revocation as normal retirement only after actual client, complete Input, or
    /// geometry supersession.
    /// A false current result may also mean borrow failure, viewport failure, or a broken layer
    /// relationship; do not equate
    /// !current with success, acquire a successor Scene, or let a cleared serial replace actual
    /// identity facts.
    pub(in super::super) fn superseded(&self, view: &NativeMarkdownPagedTextView) -> Result<bool> {
        let ivars = view.ivars();
        if ivars.geometry_failed.get() {
            return Ok(false); // Geometry-counter failure is not valid layout supersession; preserve Revoked for the owning failure boundary.
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
            return Ok(true); // Retire only when the original slot actually exits or changes client, without extending old Input/delegate ownership.
        }
        let input = self
            .input
            .upgrade()
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let state = input.state()?; // Boolean revocation must not swallow actual Borrowed, StateChanged, or Source errors.
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

/// Each candidate has one bounded staging slot, independent of text, line count, or main-queue yields.
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
        self.scene.validate_ready(&current)?; // Check the original Source error before reporting Native geometry rejection.
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
                    // Budget rejection at this constructor has no Native, mmap, or paint effects.
                    // Return optional calibration pages first,
                    // then repeat this same candidate preflight so optional optimization pages cannot
                    // reduce the drawable viewport.
                    if surface.pixels.observer().snapshot().release_failed {
                        return Err(NativeMarkdownPagedTextViewError::Raster(
                            "预算预检前校准纯颜色表未能实际归还".into(),
                        ));
                    }
                    TiledBitmap::new(geometry.width, geometry.height, Arc::clone(&surface.pixels))?
                }
                other => other?,
            };
        surface.calibrations.borrow_mut().begin_frame(); // Transfer pure Rust frame quota without querying a new Scene or renewing authority.
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
        self.scene.validate_ready(&current)?; // The original Source error precedes cancellation; never acquire the current newer Scene.
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
    /// Resume the same tile/session with a bounded work allowance; pending work never exposes a partial
    /// frame.
    pub(super) fn advance(
        mut self,
        surface: &CpuSurface,
        view: &NativeMarkdownPagedTextView,
    ) -> Result<Advance> {
        self.validate(surface, view)?;
        if let Some(prepared) = self.staging.prepared.take() {
            return self.finish_prepared(surface, view, prepared);
        } // Bounded Native calibration gets the next main-queue turn instead of adding synchronous work to the final glyph batch.
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
        } // Create at most one tile per turn; resume the original session cursor after budget exhaustion without repainting its first glyph.
        let session = self
            .session
            .take()
            .ok_or(NativeMarkdownPagedTextViewError::InvalidGeometry)?;
        let current = || self.current(surface, view);
        let step = session.step(NativeMarkdownPagedScenePaintBudget::default(), &current);
        self.scene.validate_ready(&current)?; // Actual Source/font failure takes priority even when Native reports revocation.
        let step = step?;
        if step.work.glyphs > 256 || step.work.metadata_records > 32 || step.work.batches > 256 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        } // A producer cannot evade a main-queue turn budget by splitting work into additional actual batches.
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
                drop(completion); // Return the completion claim; seal WritableTile only after finish receives the actual final-context callback.
                self.validate(surface, view)?;
                let prepared = self
                    .bitmap
                    .prepare_tile(writable)
                    .and_then(|prepared| self.bitmap.plan_encoding(prepared));
                self.validate(surface, view)?; // Final-context, pure-scan, or read-only conversion errors cannot overwrite the original Source failure.
                self.staging.prepared = Some(prepared?);
                Ok(Advance::Pending(self))
            }
        }
    }

    /// Certify one prepared region per turn, then seal exact original pixels before complete-frame
    /// publication.
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
            // Move finite calibration facts out of the RefCell before Native/current reentry, leaving
            // no live container borrow.
            let mut calibrations = std::mem::take(&mut *surface.calibrations.borrow_mut());
            let pairs = calibrations.pairs(&request, Arc::clone(&surface.pixels), &|| {
                self.validate(surface, view)
            });
            let current = self.validate(surface, view);
            if pairs.is_ok() && current.is_ok() {
                // Native may close or revoke this candidate; restore pure tables only while original
                // serial, Source, and geometry remain valid.
                // The surface slot stays empty during reentry; cancellation can proceed without borrow
                // conflicts, and revoked local tables drop without revival.
                drop(surface.calibrations.replace(calibrations));
            } else {
                drop(calibrations);
            }
            current?; // Actual Source/font failure precedes calibration rejection or Native revocation.
            pairs?
        } else {
            None // Solid, transparent, and original neutral-A8 tiles need no two-color calibration graph; retain their existing representation.
        };
        self.staging.certificates[index] = pairs;
        self.staging.next_region += 1;
        if self.staging.next_region < count {
            self.staging.prepared = Some(prepared);
            self.validate(surface, view)?;
            return Ok(Advance::Pending(self)); // The original context completed once; process at most one subregion Native request in each turn.
        }
        let current = || self.current(surface, view);
        // Borrow immutable images from the live frame for exact content comparison only; changed width
        // inherits no position, Scene, or input ticket.
        let prior = surface.reusable_frame(view, &self.reusable, &current);
        self.validate(surface, view)?; // Reject broken old Native relationships instead of turning their failure into an optimization cache miss.
        let prior = prior?;
        let certificates = self.staging.certificates.each_ref().map(Option::as_ref);
        let sealed =
            self.bitmap
                .finish_encoding(prepared, &certificates[..count], prior.as_deref());
        drop(prior); // Release the prior frame immediately after byte comparison, before another main-queue turn or publication transaction.
        self.staging.certificates = Default::default();
        self.staging.next_region = 0;
        self.validate(surface, view)?;
        sealed?;
        if self.bitmap.next_tile().is_some() {
            Ok(Advance::Pending(self)) // Keep the candidate unpublished until every tile completes.
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
// Accumulate actual work with checked arithmetic so overflow cannot manufacture a completion receipt.
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
