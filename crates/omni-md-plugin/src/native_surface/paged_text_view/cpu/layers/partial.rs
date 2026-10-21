//! For a complete same-geometry candidate, replace changed slots only; unchanged roles retain original
//! parents and CGImages without contents setters.

use super::*;

/// The original group pending list owns bounded new slots before installation, permitting real graph
/// cleanup rather than a local Rc drop alone.
struct PendingSlots<'a>(&'a LayerGroup);
impl Drop for PendingSlots<'_> {
    fn drop(&mut self) {
        self.0.clear_pending();
    }
}

/// The first replacement is an actual effect; failure before transaction commit revokes the partial
/// graph inside that transaction,
/// while validation after commit revokes the complete graph. The outer ticket retires Published
/// metadata for this same serial only.
struct Replacement<'a> {
    pool: &'a LayerPool,
    attempt: &'a Attempt,
    backing: &'a CALayer,
    committed: bool,
}
impl Drop for Replacement<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.pool.revoke_attempt(self.attempt, self.backing);
        }
    }
}

impl LayerPool {
    /// The caller must finish full painting and verify equal Geometry; this boundary rechecks actual
    /// publication, pixel dimensions, and tile positions.
    /// None means inapplicable without changing the old graph or Attempt; an existing Native graph
    /// mismatch remains an error.
    pub(in super::super) fn publish_partial(
        &self,
        backing: &CALayer,
        frame: &TileFrame,
        previous: &Publication,
        attempt: &mut Attempt,
        current: &impl Fn() -> bool,
    ) -> Result<Option<Publication>> {
        if !std::ptr::eq(attempt.pool, self)
            || self.serial.get() != attempt.before
            || previous.serial != attempt.before
            || self.active.get() != Some(previous.index)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if frame.tiles.is_empty()
            || frame.tiles.len() > MAX_TILE_SLOTS
            || dimension(previous.bounds.size.width, previous.scale)? != frame.width
            || dimension(previous.bounds.size.height, previous.scale)? != frame.height
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        if previous.tiles.len() != frame.tiles.len()
            || previous
                .tiles
                .iter()
                .zip(&frame.tiles)
                .any(|(old, image)| old.tile != image.tile)
        {
            return Ok(None); // Changed partitioning, positions, or slot count uses full two-group mounting instead of moving old roles here.
        }
        let unchanged = previous
            .tiles
            .iter()
            .zip(&frame.tiles)
            .map(|(old, image)| {
                *old == slot::Receipt::new(
                    image.tile,
                    &image.image,
                    image.secondary_image.as_deref(),
                    image.presentation,
                )
            })
            .collect::<Vec<_>>();
        if unchanged.iter().all(|same| *same) || unchanged.iter().all(|same| !*same) {
            return Ok(None); // Full equality already has setter-free reuse; without any original slot to retain, use normal complete publication.
        }
        self.validate(backing, previous, current)?;
        let group = &self.groups[previous.index];
        if !group.pending.borrow().is_empty() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let old_slots = group.slots.borrow().clone();
        if old_slots.len() != frame.tiles.len() {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // One explicit transaction covers new-slot installation, every replacement, and compensation,
        // without forced flush or partial pixel publication.
        let transaction = checked(current, || {
            CATransaction::begin();
            Transaction
        })?;
        checked(current, || CATransaction::setDisableActions(true))?;
        let pending = PendingSlots(group);
        let mut next = Vec::with_capacity(frame.tiles.len());
        for ((old, image), same) in old_slots.iter().zip(&frame.tiles).zip(&unchanged) {
            if *same {
                next.push(Rc::clone(old));
                continue; // Keep original parent, contents, masks, colors, and geometry untouched so Core Animation may retain its prepared result.
            }
            let fresh = Rc::new(slot::Slot::new(image.presentation, current)?);
            group.pending.borrow_mut().push(Rc::clone(&fresh));
            let tile = image.tile;
            let rect = NSRect::new(
                NSPoint::new(
                    tile.x as f64 / previous.scale,
                    tile.bottom as f64 / previous.scale,
                ),
                NSSize::new(
                    tile.width as f64 / previous.scale,
                    tile.height as f64 / previous.scale,
                ),
            );
            fresh.install(
                &image.image,
                image.secondary_image.as_ref(),
                slot::Layout {
                    rect,
                    scale: previous.scale,
                    presentation: image.presentation,
                },
                &group.palette,
                current,
            )?;
            next.push(fresh);
        }
        // Installation may reenter Native; the complete original graph must still match before the
        // first replacement.
        // Pending leaves original slots unchanged, so validation sees neither untransferred new roles
        // nor missing retiring old slots.
        self.validate(backing, previous, current)?;
        attempt.index = previous.index;
        let mut replacement = Replacement {
            pool: self,
            attempt,
            backing,
            committed: false,
        };
        for ((old, fresh), same) in old_slots.iter().zip(&next).zip(&unchanged) {
            if *same {
                continue;
            }
            let replaced = checked(current, || {
                let parents = (old.container().superlayer(), fresh.container().superlayer());
                let valid = parents
                    .0
                    .as_deref()
                    .is_some_and(|parent| std::ptr::eq::<CALayer>(parent, &*group.layer))
                    && parents.1.is_none();
                drop(parents);
                if !valid {
                    return false;
                }
                // SAFETY: Immediately verify the old parent is this group and the fresh slot has none.
                // Public replacement does not require
                // detaching unchanged layers or ambiguously handing already-parented layers back to
                // setSublayers.
                // No current callback occurs between the parent check and replacement, avoiding stale
                // relationships after reentry.
                unsafe {
                    group
                        .layer
                        .replaceSublayer_with(old.container(), fresh.container())
                };
                // Record the actual active group/serial immediately after the setter, before a
                // post-call current check may revoke authority.
                // Before any replacement the serial remains before, so failure clears pending while
                // preserving the original complete picture.
                self.active.set(Some(replacement.attempt.index));
                self.serial.set(replacement.attempt.serial);
                true
            })?;
            if !replaced {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
        }
        // Transfer long-lived slots after all replacements; move Rust ownership only and end borrows
        // before Native cleanup.
        let displaced = group.slots.replace(next);
        drop(std::mem::take(&mut *group.pending.borrow_mut()));
        drop(old_slots);
        let removed = displaced
            .into_iter()
            .zip(&unchanged)
            .filter_map(|(slot, same)| (!*same).then_some(slot))
            .collect();
        if !group.retire_slots(removed) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let publication = Publication {
            index: replacement.attempt.index,
            serial: replacement.attempt.serial,
            bounds: previous.bounds,
            scale: previous.scale,
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
        drop(pending); // Successful transfer left pending empty, so unchanged slots receive neither cleanup nor reinstallation.
        drop(transaction);
        // Commit is another Native boundary that may reenter and revoke the original graph; complete
        // replacement already happened,
        // so the same Attempt compensates post-commit failure before an invalid Publication can return
        // to CPU.
        self.validate(backing, &publication, current)?;
        replacement.committed = true;
        drop(replacement);
        Ok(Some(publication))
    }
}
