//! Two groups create layers for actual tiles only; the mount switches complete candidates and
//! immediately returns all old roles.

use super::*;
use crate::native_surface::viewport_pixels::MAX_TILE_SLOTS;
use objc2::rc::Weak;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSSize;
use objc2_quartz_core::{kCAContentsFormatGray8Uint, kCAContentsFormatRGBA8Uint, kCAGravityResize};

mod partial;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) mod probe;
mod slot;

pub(super) struct LayerPool {
    mount: Retained<CALayer>,
    groups: [LayerGroup; 2],
    active: Cell<Option<usize>>,
    serial: Cell<usize>,
}

/// Record this pool actual finite publication identities without strong CGImage, text, Source, or Scene
/// ownership.
#[derive(Clone)]
pub(super) struct Publication {
    index: usize,
    serial: usize,
    bounds: NSRect,
    scale: f64,
    tiles: Box<[slot::Receipt]>,
}
struct LayerGroup {
    layer: Retained<CALayer>,
    slots: RefCell<Vec<Rc<slot::Slot>>>,
    // Partial updates stage newly created changed slots only; the original complete slots remain intact
    // until actual replacement.
    // Native construction/installation may reenter; close and weak receipts must observe the same
    // bounded unique resource owner.
    pending: RefCell<Vec<Rc<slot::Slot>>>,
    retired: RefCell<Vec<Weak<AnyObject>>>,
    palette: slot::Palette,
}

/// One actual publication belongs to the same pool and original before/after serials, without Source or
/// image ownership.
pub(super) struct Attempt {
    pool: *const LayerPool,
    before: usize,
    serial: usize,
    index: usize,
}

/// On failure clear detached candidates only; after a mounting setter, retain the complete group for
/// actual-effect validation.
struct Candidate<'a>(&'a LayerGroup);
impl Drop for Candidate<'_> {
    fn drop(&mut self) {
        if self.0.layer.superlayer().is_none() {
            self.0.clear();
        } // Cleanup touches this owner detached pool only, without acquiring another Scene.
    }
}

impl LayerPool {
    pub(super) fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let mount = checked(current, CALayer::new)?;
        // A final device pixel may cross a fractional point edge; clip at actual document bounds
        // without compressing the image.
        checked(current, || mount.setMasksToBounds(true))?;
        let first = LayerGroup::new(current)?;
        let second = LayerGroup::new(current)?;
        Ok(Self {
            mount,
            groups: [first, second],
            active: Cell::new(None),
            serial: Cell::new(0),
        })
    }
    pub(super) fn mount(&self) -> &CALayer {
        &self.mount
    }

    pub(super) fn mount_claim(&self) -> Retained<CALayer> {
        Retained::clone(&self.mount)
    }

    /// Observe roles in the actual current graph without retaining layers or constructing empty objects
    /// for unused tiles.
    pub(super) fn weaks(&self) -> Vec<Weak<AnyObject>> {
        let mut values = vec![layer_weak(&self.mount)];
        for group in &self.groups {
            values.push(layer_weak(&group.layer));
            values.extend(group.retired.borrow().iter().cloned());
            let slots = group.slots.borrow().clone();
            let pending = group.pending.borrow().clone();
            for slot in slots.into_iter().chain(pending) {
                slot.add_weaks(&mut values);
            }
        }
        values
    }

    /// Reserve the next original serial without Native effects; publication alone advances the actual
    /// serial.
    pub(super) fn attempt(&self) -> Result<Attempt> {
        let before = self.serial.get();
        Ok(Attempt {
            pool: self,
            before,
            serial: before
                .checked_add(1)
                .ok_or(NativeMarkdownPagedTextViewError::CounterExhausted)?,
            index: self.active.get().map_or(0, |old| 1 - old),
        })
    }
    /// Revoke images from this actual mounted attempt only while mount/group remain original; do not
    /// claim the commit never occurred.
    pub(super) fn revoke_attempt(&self, attempt: &Attempt, backing: &CALayer) -> bool {
        if !std::ptr::eq(attempt.pool, self) || self.serial.get() != attempt.serial {
            return false;
        }
        if self.active.get().is_none() {
            // Compensation already detached the actual successor serial inside its transaction; the
            // outer owner must still retire old Published metadata.
            // Serial advances only after Native commit, distinguishing this fact from an unpublished
            // candidate or different successor.
            return true;
        }
        if self.active.get() != Some(attempt.index) {
            return false;
        }
        let group = &self.groups[attempt.index];
        let parent = self.mount.superlayer();
        // SAFETY: Read the standard CALayer array and compare original retained object identities
        // without document callbacks.
        let children = unsafe { self.mount.sublayers() };
        let same = parent
            .as_deref()
            .is_some_and(|parent| std::ptr::eq(parent, backing))
            && children.as_ref().is_some_and(|children| {
                children.len() == 1
                    && std::ptr::eq::<CALayer>(&*children.objectAtIndex(0), &*group.layer)
            });
        drop((parent, children));
        if same {
            CATransaction::begin();
            let transaction = Transaction;
            CATransaction::setDisableActions(true);
            if parent_is(&group.layer, &self.mount) {
                group.layer.removeFromSuperlayer();
            }
            group.clear();
            self.active.set(None);
            drop(transaction);
        } else {
            // External reparenting does not change this attempt serial; do not drop claims while
            // business images remain in the external graph.
            // Clear original image/color/child identities individually, preserving external parent and
            // successor replacement contents.
            group.clear();
            self.active.set(None);
        }
        true // The private serial proves this attempt committed; the caller must retire its old weak metadata.
    }

    /// The new candidate already finished actual painting; reuse the Native graph only if every tile
    /// remains the current original image.
    /// Copy at most 64 scalar identity receipts without new layers, images, or historical frames; any
    /// presentation, position, or primary/secondary difference
    /// returns to normal publication. The upstream sealer proves byte equality; consume actual reused
    /// identities only here.
    pub(super) fn reuse_publication(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        previous: &Publication,
        bounds: NSRect,
        scale: f64,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Publication>> {
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(bounds.size.width, scale)? != frame.width
            || dimension(bounds.size.height, scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if previous.bounds != bounds
            || previous.scale != scale
            || previous.tiles.len() != frame.tiles.len()
            || previous.tiles.iter().zip(&frame.tiles).any(|(old, image)| {
                *old != slot::Receipt::new(
                    image.tile,
                    &image.image,
                    image.secondary_image.as_deref(),
                    image.presentation,
                )
            })
        {
            return Ok(None);
        }
        // An equal CGImage identity does not prove original installation; fully check mount, serial,
        // layer relationships,
        // colors, and primary/secondary images. External mutation or Native/current revocation is
        // failure rather than a cache miss.
        self.validate(backing, previous, current)?;
        Ok(Some(previous.clone()))
    }

    /// Install a detached complete group, record the actual mount setter, and validate before returning
    /// publication identity.
    pub(super) fn publish(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        attempt: &Attempt,
        bounds: NSRect,
        scale: f64,
        current: &impl Fn() -> bool,
    ) -> Result<Publication> {
        if !std::ptr::eq(attempt.pool, self) || self.serial.get() != attempt.before {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let serial = attempt.serial;
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(bounds.size.width, scale)? != frame.width
            || dimension(bounds.size.height, scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        self.require_mount(backing, current)?;
        let index = attempt.index;
        let group = &self.groups[index];
        if checked(current, || group.layer.superlayer())?.is_some() {
            return Err(NativeMarkdownPagedTextViewError::Revoked); // A still-mounted old group cannot be reused as a writable candidate.
        }
        if !group.clear() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Detached slots return this owner old images only; external slot mutation rejects reuse
        // without overwriting successor contents.
        let _candidate = Candidate(group);
        checked(current, CATransaction::begin)?;
        let transaction = Transaction;
        checked(current, || CATransaction::setDisableActions(true))?;
        let local = NSRect::new(NSPoint::new(0.0, 0.0), bounds.size);
        checked(current, || group.layer.setFrame(local))?;
        checked(current, || group.layer.setBounds(local))?;
        // Build only this frame actual roles and give them to the group before reentry can revoke and
        // clean constructed relationships.
        let slots = frame
            .tiles
            .iter()
            .map(|image| slot::Slot::new(image.presentation, current).map(Rc::new))
            .collect::<Result<Vec<_>>>()?;
        group.slots.replace(slots.clone());
        for (slot, image) in slots.iter().zip(&frame.tiles) {
            let tile = image.tile;
            // tile frames remain exact pixel dimensions divided by scale, keeping contents/masks 1:1
            // while mount clips tail pixels.
            let rect = NSRect::new(
                NSPoint::new(tile.x as f64 / scale, tile.bottom as f64 / scale),
                NSSize::new(tile.width as f64 / scale, tile.height as f64 / scale),
            );
            slot.install(
                &image.image,
                image.secondary_image.as_ref(),
                slot::Layout {
                    rect,
                    scale,
                    presentation: image.presentation,
                },
                &group.palette,
                current,
            )?;
        }
        let containers = slots
            .iter()
            .map(|slot| &**slot.container())
            .collect::<Vec<_>>();
        let tiles = checked(current, || NSArray::from_slice(&containers))?;
        drop(containers); // At most 64 borrowed identities are covered by the Native array retaining each container, without extra business ownership.
        // SAFETY: The bounded array contains this group exclusive CALayers; the detached group cannot
        // expose a partial candidate.
        checked(current, || unsafe {
            group.layer.setSublayers(Some(&tiles))
        })?;
        drop(tiles);
        self.require_mount(backing, current)?;
        checked(current, || self.mount.setFrame(bounds))?;
        checked(current, || self.mount.setBounds(local))?;
        let next = checked(current, || {
            NSArray::from_retained_slice(std::slice::from_ref(&group.layer))
        })?;
        if !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: This owner exclusive mount receives the complete candidate group; this setter is the
        // sole visible frame commit point.
        unsafe { self.mount.setSublayers(Some(&next)) };
        self.active.set(Some(index)); // Record actual Native commit immediately; post-call revocation cannot claim it never happened.
        self.serial.set(serial);
        drop(next);
        // Detach and empty the old group in this explicit transaction so another implicit transaction
        // cannot retain old pixels into a later frame.
        // Cleanup remains safe after Source revocation only for an actually detached inactive group,
        // never the current mounted frame.
        let old = &self.groups[1 - index];
        let old_intact = if old.layer.superlayer().is_none() {
            old.clear()
        } else {
            false
        };
        drop(transaction);
        if !old_intact {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let publication = Publication {
            index,
            serial,
            bounds,
            scale,
            tiles: frame
                .tiles
                .iter()
                .map(|image| {
                    slot::Receipt::new(
                        image.tile,
                        &image.image,
                        image.secondary_image.as_deref(),
                        image.presentation,
                    )
                })
                .collect(),
        };
        self.validate(backing, &publication, current)?;
        Ok(publication)
    }

    /// Require original group, slot order, exact geometry, and image identities; an active flag cannot
    /// substitute for the actual Native graph.
    pub(super) fn validate(
        &self,
        backing: &CALayer,
        publication: &Publication,
        current: &impl Fn() -> bool,
    ) -> Result<()> {
        self.require_mount(backing, current)?;
        if self.active.get() != Some(publication.index) || self.serial.get() != publication.serial {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let group = &self.groups[publication.index];
        empty_container(&self.mount, current)?;
        empty_container(&group.layer, current)?;
        let local = NSRect::new(NSPoint::ZERO, publication.bounds.size);
        let geometries = checked(current, || {
            self.mount.frame() == publication.bounds
                && self.mount.bounds() == local
                && group.layer.frame() == local
                && group.layer.bounds() == local
        })?;
        if !geometries {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: This owner sets a type-correct CALayer group on mount; reading compares actual
        // identities only.
        let actual = checked(current, || unsafe { self.mount.sublayers() })?;
        let matches = actual.as_ref().is_some_and(|layers| {
            layers.len() == 1 && std::ptr::eq::<CALayer>(&*layers.objectAtIndex(0), &*group.layer)
        });
        drop(actual);
        if !matches || !current() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: The original bounded group array contains this pool slots; their complete order
        // belongs to the original publication facts.
        let actual_tiles = checked(current, || unsafe { group.layer.sublayers() })?;
        let Some(actual_tiles) = actual_tiles else {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        };
        if actual_tiles.len() != publication.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // Snapshot bounded slots only and end the RefCell borrow before Native/current so real close
        // reentry remains possible.
        let slots = group.slots.borrow().clone();
        if slots.len() != publication.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        for (index, receipt) in publication.tiles.iter().enumerate() {
            let tile = receipt.tile;
            let actual_slot = checked(current, || actual_tiles.objectAtIndex(index))?;
            let expected = NSRect::new(
                NSPoint::new(
                    tile.x as f64 / publication.scale,
                    tile.bottom as f64 / publication.scale,
                ),
                NSSize::new(
                    tile.width as f64 / publication.scale,
                    tile.height as f64 / publication.scale,
                ),
            );
            let matches = checked(current, || {
                std::ptr::eq::<CALayer>(&*actual_slot, &**slots[index].container())
            })?;
            if !matches {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            drop(actual_slot);
            slots[index].validate(
                receipt,
                expected,
                publication.scale,
                &group.palette,
                current,
            )?;
        }
        drop(actual_tiles);
        self.require_mount(backing, current)?;
        if self.active.get() != Some(publication.index) || self.serial.get() != publication.serial {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    fn require_mount(&self, backing: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
        let parent = checked(current, || self.mount.superlayer())?;
        let matches = parent
            .as_deref()
            .is_some_and(|layer| std::ptr::eq::<CALayer>(layer, backing));
        drop(parent);
        if !matches || !checked(current, || self.mount.masksToBounds())? {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    pub(super) fn detach(&self, backing: &CALayer) -> bool {
        let parent = self.mount.superlayer();
        let foreign = parent
            .as_deref()
            .is_some_and(|parent| !std::ptr::eq(parent, backing));
        drop(parent);
        if parent_is(&self.mount, backing) {
            self.mount.removeFromSuperlayer();
        }
        // Preserve an externally reparented mount while returning internal business images and this
        // owner child relationships.
        // Preserve external display properties and report revocation; detaching cannot hide external
        // whole-graph hiding or transparency.
        let mut intact = !foreign
            && !self.mount.isHidden()
            && self.mount.opacity() == 1.0
            && self.mount.zPosition() == 0.0
            && self.mount.masksToBounds();
        for group in &self.groups {
            let parent = group.layer.superlayer();
            let foreign_group = parent
                .as_deref()
                .is_some_and(|parent| !std::ptr::eq::<CALayer>(parent, &*self.mount));
            if foreign_group {
                intact = false;
            }
            drop(parent);
            if parent_is(&group.layer, &self.mount) {
                group.layer.removeFromSuperlayer();
            }
            let slot_clear = group.clear();
            intact &= slot_clear;
        }
        self.active.set(None);
        intact
    }
}

impl LayerGroup {
    fn new(current: &impl Fn() -> bool) -> Result<Self> {
        let layer = checked(current, CALayer::new)?;
        // An empty group owns one structural layer; the inactive second group does not preallocate 64
        // tiles and 320 auxiliary roles.
        Ok(Self {
            layer,
            slots: RefCell::new(Vec::new()),
            pending: RefCell::new(Vec::new()),
            retired: RefCell::new(Vec::new()),
            palette: slot::Palette::new(current)?,
        })
    }
    fn clear(&self) -> bool {
        // Move bounded slots out before Native cleanup so close reentry neither clears them twice nor
        // retains historical empty layers.
        let mut slots = std::mem::take(&mut *self.slots.borrow_mut());
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        slots.extend(pending);
        let mut intact = self.retire_slots(slots);
        // This group installs no background, mask, contents, or delegate; preserve external additions
        // and reject reuse.
        // SAFETY: Owned slots were individually detached; remaining children belong elsewhere and
        // cannot be cleared as a whole array.
        let contents = unsafe { self.layer.contents() };
        // SAFETY: Read the standard layer array only, without traversing external objects or acquiring
        // their resources.
        let children = unsafe { self.layer.sublayers() };
        intact &= contents.is_none()
            && children.as_ref().is_none_or(|children| children.is_empty())
            && !self.layer.isHidden()
            && self.layer.opacity() == 1.0
            && self.layer.zPosition() == 0.0
            && self.layer.backgroundColor().is_none()
            && self.layer.mask().is_none()
            && self.layer.delegate().is_none();
        intact // This project installs no group delegate; a later external delegate does not belong to this cleanup owner.
    }

    /// Return changed slots not yet transferred on failure, preserving the original complete picture
    /// and published serial.
    fn clear_pending(&self) -> bool {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if pending.is_empty() {
            return true;
        }
        self.retire_slots(pending)
    }

    /// Full retirement and partial replacement share actual slot cleanup; register weak identities
    /// before clearing images, colors, and parents.
    /// The input is the unique bounded list removed from group ownership, without historical strong
    /// claims or an idle role pool.
    fn retire_slots(&self, slots: Vec<Rc<slot::Slot>>) -> bool {
        let mut intact = true;
        let mut retired = std::mem::take(&mut *self.retired.borrow_mut());
        // Historical identities remain weak; close receipts still observe actual lifetimes when
        // frameworks or external owners retain empty roles.
        objc2::rc::autoreleasepool(|_| retired.retain(|weak| weak.load().is_some()));
        if retired.is_empty() {
            // Once every old role actually dies, return the weak-list capacity too; never discard a
            // live weak observation.
            retired = Vec::new();
        }
        for slot in &slots {
            slot.add_weaks(&mut retired);
        }
        self.retired.replace(retired); // End the slot borrow before Native cleanup so the same close boundary can reenter.
        for slot in &slots {
            intact &= slot.clear(&self.layer, &self.palette);
        }
        drop(slots); // Return old-frame and resized-away roles before this transaction ends, without an idle layer cache.
        intact
    }
}

fn empty_container(layer: &CALayer, current: &impl Fn() -> bool) -> Result<()> {
    // SAFETY: Mount/group install no business contents; test emptiness without treating unknown
    // external objects as original images.
    let contents = checked(current, || unsafe { layer.contents() })?;
    let background = checked(current, || layer.backgroundColor())?;
    let mask = checked(current, || layer.mask())?;
    let delegate = checked(current, || layer.delegate())?;
    // Mount/group never hide, fade, or change Z order; empty payload alone cannot certify unchanged
    // actual output.
    let unchanged = checked(current, || {
        !layer.isHidden() && layer.opacity() == 1.0 && layer.zPosition() == 0.0
    })?;
    if contents.is_none()
        && background.is_none()
        && mask.is_none()
        && delegate.is_none()
        && unchanged
    {
        Ok(())
    } else {
        Err(NativeMarkdownPagedTextViewError::Revoked)
    }
}

/// Borrow the actual CALayer superclass as AnyObject; weak registration creates no additional
/// main-thread strong owner.
fn layer_weak(layer: &CALayer) -> Weak<AnyObject> {
    let object: &AnyObject = layer;
    Weak::from(object)
}

fn parent_is(layer: &CALayer, expected: &CALayer) -> bool {
    let parent = layer.superlayer();
    parent
        .as_deref()
        .is_some_and(|parent| std::ptr::eq(parent, expected))
}
