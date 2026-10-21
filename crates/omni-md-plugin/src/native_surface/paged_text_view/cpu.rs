//! Paint the same Ready Scene into bounded RGBA8 pixels; the window context receives no retained glyph
//! display list.

use super::*;
use crate::native_surface::viewport_pixels::{
    NativeMarkdownViewportPixels, PixelTile, TileFrame, TiledBitmap,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::CGContext;
use objc2_quartz_core::{CALayer, CATransaction};

mod calibration;
pub(super) mod candidate;
mod install;
mod layers;
mod scheduler;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use install::probe::run as probe_installation_rollback;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use layers::probe::run as probe_mask_slots;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use scheduler::probe::nested_delivery as probe_nested_delivery;
use std::sync::Arc;

/// Own this new View actual AppKit backing and pixel ledger, without a strong document, Scene, or View
/// back-reference.
pub(super) struct CpuSurface {
    layer: Retained<CALayer>,
    layers: layers::LayerPool,
    pixels: Arc<NativeMarkdownViewportPixels>,
    paints: Cell<usize>,
    painting: Cell<bool>,
    // Synchronous Native reentry coalesces one repaint request without retaining newer Scene/Input or
    // pixels on the old stack.
    repaint_requested: Cell<bool>,
    published: RefCell<Option<Rc<Published>>>,
    reuses: Cell<usize>,
    candidate: RefCell<Option<candidate::Candidate>>,
    // Retain bounded pure composition tables across this surface frames only, without Scene, Input,
    // provider, or CALayer ownership.
    // Move the entire slot out before Native calibration so close reentry and actual table unmapping
    // cross no RefCell borrow.
    calibrations: RefCell<calibration::Calibrations>,
    active_serial: Cell<Option<u64>>,
    next_serial: Cell<u64>,
    scheduled: Cell<Option<u64>>,
    deferred: Cell<Option<u64>>,
    steps: Cell<usize>,
}

/// One displayed frame weak identity and finite Native publication facts, without Scene, Input, file,
/// or document payload.
struct Published {
    key: NativeMarkdownPagedSceneKey,
    geometry: Geometry,
    publication: layers::Publication,
    frame: RefCell<Option<Rc<TileFrame>>>,
}
#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    bounds: NSRect,
    backing: NSRect,
    scale: f64,
    width: usize,
    height: usize,
}

/// Admit one original paint stack per surface despite Native reentry; nested stacks cannot consume
/// counters or overwrite candidates.
struct PaintUse<'a>(&'a Cell<bool>);
impl<'a> PaintUse<'a> {
    fn enter(slot: &'a Cell<bool>) -> Result<Self> {
        if slot.get() {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        slot.set(true);
        Ok(Self(slot))
    }
}
impl Drop for PaintUse<'_> {
    fn drop(&mut self) {
        self.0.set(false); // Return this synchronous stack admission only, without queueing events, recapturing Scenes, or retrying newer state.
    }
}

/// End the original transaction on failure too, without publishing further contents after revocation.
struct Transaction;
impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

/// Failure after the mount setter cannot claim no commit occurred; revoke only the original attempt and
/// preserve the caller error.
struct PublicationUse<'a> {
    surface: &'a CpuSurface,
    attempt: layers::Attempt,
    armed: bool,
}
impl<'a> PublicationUse<'a> {
    fn new(surface: &'a CpuSurface) -> Result<Self> {
        Ok(Self {
            surface,
            attempt: surface.layers.attempt()?,
            armed: true,
        })
    }
    fn commit(&mut self) {
        self.armed = false;
    }
}
impl Drop for PublicationUse<'_> {
    fn drop(&mut self) {
        if self.armed
            && self
                .surface
                .layers
                .revoke_attempt(&self.attempt, &self.surface.layer)
        {
            let metadata = self.surface.published.borrow_mut().take();
            drop(metadata); // A revoked original picture cannot certify Ready; do not clear successor relationships from another serial.
        }
    }
}

impl CpuSurface {
    pub(super) fn install(view: &NativeMarkdownPagedTextView) -> Result<Rc<Self>> {
        let origin = view.origin()?;
        if origin.step(view, || view.wantsLayer())? || origin.step(view, || view.layer())?.is_some()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry); // The new View must retain its original nil relationship rather than take an external layer.
        }
        // Enable the standard layer-backed path first so AppKit owns actual updateLayer scheduling.
        // Setting a layer before wantsLayer creates layer-hosting; direct updateLayer calls cannot
        // substitute for real scheduling.
        let mut installation = install::Installation::new(view);
        let layer = installation.enable(&origin)?;
        let layers = layers::LayerPool::new(&|| origin.current(view))?;
        installation.mount(layers.mount_claim());
        origin.step(view, || layer.addSublayer(layers.mount()))?;
        let owner = Rc::new(Self {
            layer,
            layers,
            pixels: Arc::default(),
            paints: Cell::new(0),
            painting: Cell::new(false),
            repaint_requested: Cell::new(false),
            published: RefCell::new(None),
            reuses: Cell::new(0),
            candidate: RefCell::new(None),
            calibrations: RefCell::default(),
            active_serial: Cell::new(None),
            next_serial: Cell::new(0),
            scheduled: Cell::new(None),
            deferred: Cell::new(None),
            steps: Cell::new(0),
        });
        origin.step(view, || {
            view.setLayerContentsRedrawPolicy(
                objc2_app_kit::NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
            )
        })?;
        owner.layer_current(view, &|| origin.current(view))?;
        installation.commit(); // No Native callback occurs before publication to the unique CPU slot; the successful owner takes all cleanup responsibility.
        Ok(owner)
    }

    pub(super) fn observer(&self) -> NativeMarkdownViewportPixelObserver {
        self.pixels.observer()
    }
    pub(super) fn track_close(&self, receipt: &NativeMarkdownPagedCloseReceipt) {
        receipt.track_pixels(&self.pixels); // Close original allocation admission; asynchronous providers remain covered by this actual pixel receipt without retaining the surface.
    }
    pub(super) fn layer_weaks(&self) -> Vec<objc2::rc::Weak<AnyObject>> {
        self.layers.weaks()
    }
    pub(super) fn paints(&self) -> usize {
        self.paints.get()
    }

    /// Hover uses the actual complete published frame, without equating Ready or a candidate with
    /// displayed pixels.
    /// Return temporary Rc claims on this stack and recheck publication identity after Native
    /// validation, retaining no Scene or pixel history.
    pub(super) fn cursor_publication_current(
        &self,
        view: &NativeMarkdownPagedTextView,
        key: &NativeMarkdownPagedSceneKey,
        current: &impl Fn() -> bool,
    ) -> Result<bool> {
        let published = self.published.borrow().clone();
        let Some(published) = published.filter(|published| published.key.same(key)) else {
            return Ok(false);
        };
        self.layer_current(view, current)?;
        self.layers
            .validate(&self.layer, &published.publication, current)?;
        if !self
            .published
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &published))
            || !current()
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(true)
    }

    /// The production Source probe borrows the actual published key and paint count without cloning
    /// pixels, layers, or candidate frames.
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn published_frame(&self) -> Option<(NativeMarkdownPagedSceneKey, usize)> {
        self.published
            .borrow()
            .as_ref()
            .map(|published| (published.key.clone(), self.paints.get()))
    }

    pub(super) fn reuses(&self) -> usize {
        self.reuses.get()
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn reused_images(&self) -> Option<usize> {
        let published = self.published.borrow().clone();
        published.as_ref().and_then(|published| {
            published
                .frame
                .borrow()
                .as_ref()
                .map(|frame| frame.reused_images)
        })
    }

    fn layer_current(
        &self,
        view: &NativeMarkdownPagedTextView,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        let actual = checked(current, || view.layer())?;
        let matches = actual
            .as_deref()
            .is_some_and(|actual| std::ptr::eq::<CALayer>(actual, &*self.layer));
        drop(actual); // Release the temporary Native claim before the final pure-authority check.
        if !matches || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// AppKit display callbacks acquire the current picture; abandon only that request if original
    /// Input or geometry changes during acquisition.
    /// Preserve acquisition failure before accepted close, real resource errors, and CPU/layer checks
    /// after Ready.
    pub(super) fn paint_current(self: &Rc<Self>, view: &NativeMarkdownPagedTextView) -> Result<()> {
        let result = (|| {
            let Some(origin) = view.display_origin()? else {
                // The Body accepted close and revoked active authority while Native still detaches the
                // View; acquire no new Scene or pixels.
                // End Origin acquisition only; later resource and Native failures retain their existing
                // strict boundary.
                return Ok(());
            };
            let ticket = candidate::Ticket::new(&origin);
            let scene = match view.scene_for_origin(origin) {
                Ok(scene) => scene,
                Err(NativeMarkdownPagedTextViewError::Pending) => {
                    return Ok(()); // The unique layout task is queued; publish no pixels and do not present an old image as newly Ready.
                }
                Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(view)? => {
                    return Ok(()); // Require actual original-client, complete-Input, or geometry supersession rather than trusting !scene_current.
                }
                Err(cause) => return Err(cause),
            };
            self.paint(view, &scene) // After Ready, acquisition tickets cannot swallow Source, font, or Native ownership errors.
        })();
        if result.is_err() {
            self.clear_calibrations(); // Return optimization pages after real acquisition, reentry-health, or display failure while preserving the typed primary error.
        }
        result
    }

    fn paint(self: &Rc<Self>, view: &NativeMarkdownPagedTextView, scene: &SceneUse) -> Result<()> {
        let painting = match PaintUse::enter(&self.painting) {
            Ok(painting) => painting,
            Err(NativeMarkdownPagedTextViewError::Pending) => {
                // updateLayer may reenter during synchronous Core Text/AppKit callbacks. Reject a
                // second writer only;
                // the outer stack retains exclusive candidate ownership, and a healthy display request
                // must not become permanent input failure.
                let current = || scene.current(view);
                if !Self::scene_ready_for_display(view, scene)? {
                    return Ok(());
                }
                if let Err(cause) = self.layer_current(view, &current) {
                    return Err(Self::native_failure(view, scene, cause));
                }
                if !Self::scene_ready_for_display(view, scene)? {
                    return Ok(());
                } // Actual Source, font, and Native ownership failures remain errors and cannot hide behind a coalesced request.
                self.repaint_requested.set(true);
                return Ok(());
            }
            Err(cause) => return Err(cause),
        };
        let result = self.paint_inner(view, scene);
        drop(painting);
        let wake = self.wake_deferred(view); // If nested main-queue delivery deferred this continuation, reschedule the same serial only after its original paint stack exits.
        match result {
            Err(error) => Err(error),
            Ok(()) => wake,
        } // A later scheduling rejection cannot replace the original paint/Source error.
    }
    fn paint_inner(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
    ) -> Result<()> {
        let current = || scene.current(view);
        if !Self::scene_ready_for_display(view, scene)? {
            return Ok(()); // Original Source/font/output health precedes retirement classification; an equal nonce cannot hide EIO.
        }
        let geometry = match self.geometry(view, scene) {
            Ok(geometry) => geometry,
            Err(cause) => return Err(Self::native_failure(view, scene, cause)),
        };
        if !Self::scene_ready_for_display(view, scene)? {
            return Ok(());
        }
        let prior = self.published.borrow().clone(); // Borrow finite metadata through Rc and end the slot borrow before Native/current calls.
        if let Some(prior) = prior
            && prior.key.same(&scene.key)
            && prior.geometry == geometry
        {
            let native = self
                .layers
                .validate(&self.layer, &prior.publication, &current);
            if let Err(cause) = native {
                return Err(Self::native_failure(view, scene, cause));
            }
            if !Self::scene_ready_for_display(view, scene)? {
                return Ok(());
            }
            let same_geometry = match self.geometry(view, scene) {
                Ok(actual) => actual == geometry,
                Err(cause) => return Err(Self::native_failure(view, scene, cause)),
            };
            if !Self::scene_ready_for_display(view, scene)? {
                return Ok(());
            }
            let native = self
                .layers
                .validate(&self.layer, &prior.publication, &current);
            if let Err(cause) = native {
                return Err(Self::native_failure(view, scene, cause));
            }
            if !Self::scene_ready_for_display(view, scene)? {
                return Ok(());
            }
            if !same_geometry {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if !current() {
                // Actual Native geometry and layers already passed; classify only the final
                // original-ticket revocation here.
                // Same-ticket Scene failure remains an error; another current result cannot erase the
                // observed rejection.
                if !Self::scene_ready_for_display(view, scene)? {
                    return Ok(());
                }
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let count = self
                .reuses
                .get()
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
            self.cancel_candidate(); // Even for an already displayed nonce, consume any unrelated unpublished candidate first.
            if !Self::scene_ready_for_display(view, scene)? {
                return Ok(());
            }
            self.reuses.set(count);
            return Ok(()); // Reuse the same healthy complete frame without allocating a third frame or resubmitting glyphs to CTFont.
        }
        let existing = self
            .candidate
            .borrow()
            .as_ref()
            .map(|candidate| candidate.key.same(&scene.key) && candidate.geometry == geometry);
        if existing == Some(true) {
            return Ok(()); // The candidate already has one coalesced callback; duplicate updateLayer must not allocate again from its first tile.
        }
        self.cancel_candidate();
        let serial = self
            .next_serial
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
        self.next_serial.set(serial);
        self.active_serial.set(Some(serial));
        let reusable = self
            .published
            .borrow()
            .as_ref()
            .and_then(|published| published.frame.borrow().as_ref().map(Rc::downgrade))
            .unwrap_or_default();
        let candidate = candidate::Candidate::new(serial, scene, geometry, self, reusable);
        match candidate {
            Ok(candidate) => self.advance(view, candidate),
            Err(error) => {
                self.cancel_candidate();
                Err(error)
            }
        }
    }

    /// Normal retirement is possible only after this original Scene health check fails with no pending
    /// Native error.
    /// validate_ready preserves actual Source/font/directory errors; abandon old display only after the
    /// original client or complete Input/geometry
    /// actually changes, without treating a plain false scene_current or empty candidate serial as
    /// proof.
    fn scene_ready_for_display(
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
    ) -> Result<bool> {
        let ticket = candidate::Ticket::new(&scene.origin);
        match scene.scene.validate_ready(&|| scene.current(view)) {
            Ok(()) => Ok(true),
            Err(NativeMarkdownPagedTextViewError::Revoked) if ticket.superseded(view)? => Ok(false),
            Err(cause) => Err(cause),
        }
    }

    /// After geometry or layer validation fails, later ordinary revocation cannot turn that Native
    /// failure into retirement.
    /// Recheck Source/font on the same Scene; only a specific resource failure can precede the original
    /// Native error.
    fn native_failure(
        view: &NativeMarkdownPagedTextView,
        scene: &SceneUse,
        cause: NativeMarkdownPagedTextViewError,
    ) -> NativeMarkdownPagedTextViewError {
        match scene.scene.validate_ready(&|| scene.current(view)) {
            Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => cause,
            Err(resource) => resource,
        }
    }

    /// New original authority or close cancels this surface candidate only; weak queued closures retain
    /// none of its resources.
    pub(super) fn cancel_candidate(&self) {
        let active = self.active_serial.replace(None).is_some();
        self.scheduled.set(None);
        self.deferred.set(None);
        let candidate = self.candidate.borrow_mut().take();
        drop(candidate); // End the slot borrow before actually dropping sessions, contexts, and bounded sealed tiles.
        if active {
            self.clear_calibrations(); // Actual unfinished-frame cancellation returns tables immediately; ordinary Scene retirement without a candidate may retain bounded pure facts.
        }
    }

    /// Return all actual pure tables and report whether optional page state was evicted, without a
    /// generic budget retry policy.
    fn clear_calibrations(&self) -> bool {
        let calibrations = std::mem::take(&mut *self.calibrations.borrow_mut());
        let occupied = calibrations.has_pages();
        drop(calibrations); // Final-Arc unmapping occurs after the slot borrow ends; failures remain in the original pixel-release receipt.
        occupied
    }

    /// Release actual paint readers for cell admission, rejecting a live synchronous paint stack
    /// instead of mistaking cancellation for destruction.
    pub(super) fn release_source_readers(&self) -> Result<()> {
        if self.painting.get() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        self.cancel_candidate();
        Ok(()) // Published retains weak key facts and pixels only, so keeping the old frame does not retain draft Source.
    }
    pub(super) fn cancel_if_not_key(&self, key: &NativeMarkdownPagedSceneKey) {
        let obsolete = self
            .candidate
            .borrow()
            .as_ref()
            .is_some_and(|candidate| !candidate.key.same(key));
        if obsolete {
            self.cancel_candidate();
        }
    }
    pub(super) fn pending(&self) -> bool {
        self.active_serial.get().is_some()
    }
    pub(super) fn steps(&self) -> usize {
        self.steps.get()
    }

    /// Briefly borrow immutable images from the current live frame without old Scene, input authority,
    /// or tile positions.
    /// Wider line numbers or resized windows change full geometry while equal-size image regions may
    /// remain byte-identical;
    /// fully validate old mounting relationships and publish new positions and Ready through the
    /// current candidate only.
    fn reusable_frame(
        &self,
        view: &NativeMarkdownPagedTextView,
        expected: &std::rc::Weak<TileFrame>,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Rc<TileFrame>>> {
        let Some(frame) = expected.upgrade() else {
            return Ok(None); // The old frame already released through its original lifecycle; do not recover historical caches or reconstruct old images.
        };
        let published = self.published.borrow().clone();
        let Some(published) = published else {
            return Ok(None);
        };
        let same_frame = published
            .frame
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &frame));
        if !same_frame {
            return Ok(None); // Borrow only the expected current live frame, never historical images or another publication pixels.
        }
        self.layer_current(view, current)?;
        self.layers
            .validate(&self.layer, &published.publication, current)?;
        let same = self
            .published
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, &published))
            && published
                .frame
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &frame));
        if !same || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        } // After Native/current reentry, consume only the same original live frame without certifying another publication.
        Ok(Some(frame))
    }

    fn advance(
        self: &Rc<Self>,
        view: &NativeMarkdownPagedTextView,
        candidate: candidate::Candidate,
    ) -> Result<()> {
        let serial = candidate.serial;
        let ticket = candidate.ticket.clone();
        let key = candidate.key.clone();
        let scene = Rc::clone(&candidate.scene);
        // Candidate moves into Pending/Complete or actually drops on failure; this synchronous stack
        // keeps only the original weak ticket and same Scene,
        // without extra Input/document ownership or a newer Scene certifying old paint. Classify
        // failure before clearing the serial.
        let result = (|| {
            let step = self
                .steps
                .get()
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
            self.steps.set(step); // Overflow follows the same terminal cleanup, preventing active state without a candidate.
            match candidate.advance(self, view)? {
                candidate::Advance::Pending(candidate) => {
                    if self.active_serial.get() != Some(serial) {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    }
                    let replaced = self.candidate.replace(Some(candidate));
                    drop(replaced);
                    self.schedule(view, serial)?;
                    Ok(()) // Keep old pixels until the full frame completes; Pending is not an actual publication.
                }
                candidate::Advance::Complete(completed) => {
                    completed.validate(self, view)?;
                    let current = || completed.current(self, view);
                    self.layer_current(view, &current)?;
                    let prior = self.published.borrow().clone();
                    let reused = if let Some(prior) = &prior
                        && prior.geometry == completed.geometry
                    {
                        self.layers.reuse_publication(
                            &self.layer,
                            &completed.frame,
                            &prior.publication,
                            completed.geometry.backing,
                            completed.geometry.scale,
                            &current,
                        )
                    } else {
                        Ok(None)
                    };
                    completed.validate(self, view)?; // Original Source/font failure still precedes Native graph validation failure.
                    let reused = reused?;
                    if reused.is_some()
                        && !self.published.borrow().as_ref().is_some_and(|actual| {
                            prior
                                .as_ref()
                                .is_some_and(|prior| Rc::ptr_eq(actual, prior))
                        })
                    {
                        return Err(NativeMarkdownPagedTextViewError::Revoked);
                    } // After Native getters, require the same current publication; old graph receipts cannot certify a successor owner.
                    let mut publication_use = if reused.is_none() {
                        Some(PublicationUse::new(self)?)
                    } else {
                        None // Reuse performs no Native setter, so failure must not revoke the graph still owned by the previous frame.
                    };
                    let retired = prior
                        .as_ref()
                        .and_then(|published| published.frame.borrow_mut().take());
                    drop(retired); // Original slots retain visible images; return the frame cache first and replace old metadata/Ready at the actual commit point only.
                    let publication = match reused {
                        Some(publication) => Ok(publication),
                        None => (|| {
                            let attempt = &mut publication_use
                                .as_mut()
                                .expect("新图发布持有原补偿票")
                                .attempt;
                            // Partial changes also commit Native effects and require the original
                            // PublicationUse compensation ticket first.
                            // The old frame pixel slot was returned; borrow current publication
                            // identity only, without historical Scene/provider claims.
                            if let Some(prior) = &prior
                                && prior.geometry == completed.geometry
                                && let Some(publication) = self.layers.publish_partial(
                                    &self.layer,
                                    &completed.frame,
                                    &prior.publication,
                                    attempt,
                                    &current,
                                )?
                            {
                                return Ok(publication);
                            }
                            self.layers.publish(
                                &self.layer,
                                &completed.frame,
                                attempt,
                                completed.geometry.backing,
                                completed.geometry.scale,
                                &current,
                            )
                        })(),
                    };
                    drop(prior); // Keep the actual Native result local until the same following validation decides Source/font priority.
                    completed.validate(self, view)?; // Source failure precedes Native revocation, without certifying a newer Scene.
                    let publication = publication?;
                    self.layer_current(view, &current)?;
                    completed.validate(self, view)?;
                    let count = self
                        .paints
                        .get()
                        .checked_add(1)
                        .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?;
                    let published = Rc::new(Published {
                        key: completed.key.clone(),
                        geometry: completed.geometry,
                        publication,
                        frame: RefCell::new(Some(Rc::clone(&completed.frame))),
                    });
                    let old = self.published.replace(Some(published));
                    drop(old);
                    self.paints.set(count);
                    self.active_serial.set(None);
                    if let Some(publication_use) = &mut publication_use {
                        publication_use.commit();
                    } // All Source/geometry checks passed; subsequent commit observation introduces no rollback-capable failure.
                    // Both paths publish a new key only after the original Candidate actually finishes
                    // the full frame,
                    // incrementing paint and notifying once at the existing tail; layer reuse claims no
                    // extra paint or old Scene replay.
                    completed.scene.cpu_frame_published(count); // Notify actual full-frame commit only; add no later rollback Result or new Scene acquisition.
                    // The paint stack still holds Scene; queue original weak facts so AppKit rebuilds
                    // cursor regions after all readers return.
                    view.queue_link_cursor_refresh(&completed.key);
                    Ok(())
                }
            }
        })();
        let failed = result.is_err();
        let result = match result {
            Err(NativeMarkdownPagedTextViewError::Revoked) => (|| {
                let current =
                    || self.active_serial.get() == Some(serial) && ticket.current(view, &key);
                match scene.validate_ready(&current) {
                    Ok(()) | Err(NativeMarkdownPagedTextViewError::Revoked) => {}
                    Err(cause) => return Err(cause),
                } // Preserve Source/font/resource errors through original validate_ready without bypassing current to renew old font authority.
                if ticket.superseded(view)? {
                    Ok(()) // Accepted new client/Input/geometry retires the old candidate without poisoning the new client permanent input-failure slot.
                } else {
                    Err(NativeMarkdownPagedTextViewError::Revoked)
                } // Layer/Scene failures under the same complete ticket remain errors; do not swallow every false scene_current.
            })(),
            other => other,
        };
        if failed && self.active_serial.get() == Some(serial) {
            self.cancel_candidate(); // Failure is terminal; the next turn cannot resume or replay the failed session.
        }
        result
    }

    fn geometry(&self, view: &NativeMarkdownPagedTextView, scene: &SceneUse) -> Result<Geometry> {
        self.geometry_for(view, &scene.key, &|| scene.current(view))
    }
    // Match the current View, backing, Scene width and viewport before deriving actual device
    // allocation dimensions.
    fn geometry_for(
        &self,
        view: &NativeMarkdownPagedTextView,
        key: &NativeMarkdownPagedSceneKey,
        current: &impl Fn() -> bool,
    ) -> Result<Geometry> {
        self.layer_current(view, current)?;
        let bounds = checked(current, || view.bounds())?;
        let backing = checked(current, || self.layer.bounds())?;
        let window =
            checked(current, || view.window())?.ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let scale = checked(current, || window.backingScaleFactor())?;
        if !finite_rect(bounds, true)
            || !finite_rect(backing, true)
            || bounds.size != backing.size
            || bounds.size.width != key.request.bounds_width
            || !scale.is_finite()
            || scale <= 0.0
            || !contains(bounds, key.request.viewport)
            || !finite_edges(bounds)
            || !(-scale * bounds.origin.x).is_finite()
            || !(-scale * bounds.origin.y).is_finite()
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(Geometry {
            bounds,
            backing,
            scale,
            width: dimension(bounds.size.width, scale)?,
            height: dimension(bounds.size.height, scale)?,
        })
    }

    pub(super) fn detach(&self, view: &NativeMarkdownPagedTextView) -> Result<()> {
        self.repaint_requested.set(false); // Close cannot revive old display requests; this scalar retains no asynchronous business owner.
        self.cancel_candidate(); // Return every Pending session/context first so actual business data cannot hide in queued tasks.
        self.clear_calibrations(); // An idle surface may retain pure tables; close returns them now rather than waiting for later Rust owner destruction.
        let published = self.published.borrow_mut().take();
        drop(published);
        let actual = view.layer();
        let owned = actual
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
        drop(actual);
        CATransaction::begin();
        let transaction = Transaction;
        CATransaction::setDisableActions(true);
        let intact = self.layers.detach(&self.layer); // Clear owned images even if backing changed, preserving the external parent relationship.
        let delegate = self.layer.delegate();
        let our_delegate = delegate.as_deref().is_some_and(|delegate| {
            std::ptr::from_ref(delegate).cast::<()>() == std::ptr::from_ref(view).cast::<()>()
        });
        drop(delegate);
        if our_delegate {
            self.layer.setDelegate(None);
        } // Detach only the original AppKit-to-View delegate relationship, leaving a later external delegate intact.
        let actual = view.layer();
        let still_owned = actual
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
        drop(actual);
        if still_owned {
            view.setWantsLayer(false);
            let actual = view.layer();
            let may_clear = actual
                .as_deref()
                .is_none_or(|layer| std::ptr::eq::<CALayer>(layer, &*self.layer));
            drop(actual);
            if may_clear {
                view.setLayer(None);
            } // The setter may already have cleared the layer; a successor backing must remain untouched.
        }
        drop(transaction);
        let actual = view.layer();
        let final_layer = actual.is_some();
        drop(actual);
        let final_wants_layer = view.wantsLayer();
        if !owned || !intact || final_layer || final_wants_layer {
            return Err(NativeMarkdownPagedTextViewError::Revoked); // Complete owned business cleanup without claiming an external live weak object is nil.
        }
        Ok(())
    }
}

// Retain original caller authority across each reentrant Native operation; local results drop on
// rejection.
fn checked<T>(current: &impl Fn() -> bool, call: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    let result = call();
    if !current() {
        return Err(NativeMarkdownPagedTextViewError::Revoked);
    }
    Ok(result)
}
fn dimension(points: f64, scale: f64) -> Result<usize> {
    let pixels = points * scale;
    // Actual Native ruler layout may produce fractional points; round outward to cover device pixels
    // without rejecting or scaling text.
    // TiledBitmap accounts actual ceiling dimensions; use a strict usize bound because f64 may round
    // MAX to 2^64.
    if !points.is_finite()
        || points <= 0.0
        || !scale.is_finite()
        || scale <= 0.0
        || !pixels.is_finite()
        || pixels <= 0.0
        || pixels.ceil() >= usize::MAX as f64
    {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    Ok(pixels.ceil() as usize)
}
fn contains(outer: NSRect, inner: NSRect) -> bool {
    finite_rect(inner, true)
        && finite_edges(outer)
        && finite_edges(inner)
        && inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn finite_edges(rect: NSRect) -> bool {
    (rect.origin.x + rect.size.width).is_finite() && (rect.origin.y + rect.size.height).is_finite()
}
fn tile_transform(bounds: NSRect, scale: f64, tile: PixelTile) -> Result<CGAffineTransform> {
    let tx = -scale * bounds.origin.x - tile.x as f64;
    let ty = -scale * bounds.origin.y - tile.bottom as f64;
    if !tx.is_finite() || !ty.is_finite() {
        return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
    }
    // View coordinates are Y-up and tile.y is the provider top row; actual bottom maps the same Source
    // point into its tile context.
    Ok(CGAffineTransform {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: scale,
        tx,
        ty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    #[test]
    fn nested_native_paint_cannot_steal_or_release_the_original_stack_guard() {
        let slot = Cell::new(false);
        let original = PaintUse::enter(&slot).unwrap();
        assert!(matches!(
            PaintUse::enter(&slot),
            Err(NativeMarkdownPagedTextViewError::Pending)
        ));
        assert!(slot.get());
        drop(original);
        assert!(!slot.get());
        drop(PaintUse::enter(&slot).unwrap());
        assert!(!slot.get());
    }

    #[test]
    fn finite_components_cannot_hide_overflowing_edges_or_backing_transform() {
        let overflowing = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(f64::MAX, 10.0));
        assert!(!contains(overflowing, overflowing));
        let source = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(1.0, 1.0));
        let tile = PixelTile {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bottom: 0,
        };
        assert!(matches!(
            tile_transform(source, 2.0, tile),
            Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
        ));
        assert!(matches!(
            dimension(f64::MAX, 2.0),
            Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
        ));
        // Accept original fractional points without saturating away actually unallocatable device
        // dimensions.
        assert_eq!(dimension(613.69140625, 2.0).unwrap(), 1228);
        for (points, scale) in [(usize::MAX as f64, 1.0), (-1.0, -1.0), (1.0, 0.0)] {
            assert!(matches!(
                dimension(points, scale),
                Err(NativeMarkdownPagedTextViewError::InvalidGeometry)
            ));
        }
        // Preserve the same Source exact Y-up device origin in a partial tail tile, without truncation
        // or reversed images.
        let valid = NSRect::new(NSPoint::new(12.0, 7.0), NSSize::new(640.0, 320.0));
        let transform = tile_transform(
            valid,
            2.0,
            PixelTile {
                x: 512,
                y: 512,
                width: 256,
                height: 128,
                bottom: 0,
            },
        )
        .unwrap();
        assert_eq!(
            (transform.a, transform.d, transform.tx, transform.ty),
            (2.0, 2.0, -536.0, -14.0)
        );
        // After actual line-number growth from 9 to 10, preserve 2x device scale instead of compressing
        // 1228 columns into 1227.3828125.
        let fractional = NSRect::new(NSPoint::ZERO, NSSize::new(613.69140625, 240.0));
        let tail = tile_transform(
            fractional,
            2.0,
            PixelTile {
                x: 1024,
                y: 256,
                width: 204,
                height: 224,
                bottom: 0,
            },
        )
        .unwrap();
        assert_eq!((tail.a, tail.d, tail.tx, tail.ty), (2.0, 2.0, -1024.0, 0.0));
    }
}
