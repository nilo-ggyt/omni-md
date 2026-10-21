//! Fixed 64-KiB page directory containing only page addresses and pin counts.
//! It neither copies document bytes nor retains external objects.

use super::*;

pub(super) const MANAGED_WINDOW_BYTES: usize = 64 * 1024;
pub(super) const MAX_WINDOW_SLOTS: usize = MANAGED_WINDOW_BYTES / ORIGINAL_READ_BYTES;

#[derive(Clone, Copy, Default)]
pub(super) struct WindowSlot {
    pub(super) page: Option<usize>,
    pub(super) pins: usize,
    used: u64,
}

#[derive(Clone, Copy, Default)]
pub(super) struct LeasePins {
    pub(super) slots: [usize; 2],
    pub(super) pages: [usize; 2],
    pub(super) count: usize,
}

pub(super) struct WindowState {
    pub(super) slots: [WindowSlot; MAX_WINDOW_SLOTS],
    clock: u64,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            slots: [WindowSlot::default(); MAX_WINDOW_SLOTS],
            clock: 0,
        }
    }
}

impl WindowState {
    /// Plans the complete request before eviction. Insufficient unpinned slots grant
    /// no partial authority and leave the existing directory unchanged.
    pub(super) fn plan(&self, pages: &[usize], capacity: usize) -> Option<LeasePins> {
        if pages.len() > 2 || capacity > self.slots.len() {
            return None;
        }
        let mut pins = LeasePins {
            count: pages.len(),
            ..LeasePins::default()
        };
        let mut selected = [false; MAX_WINDOW_SLOTS];
        // Reserve every existing hit before choosing victims, so a later miss cannot
        // evict another page required by this same request.
        for (at, &page) in pages.iter().enumerate() {
            pins.pages[at] = page;
            if let Some(slot) = self.slots[..capacity]
                .iter()
                .position(|slot| slot.page == Some(page))
            {
                pins.slots[at] = slot;
                selected[slot] = true;
            } else {
                pins.slots[at] = usize::MAX;
            }
        }
        for at in 0..pages.len() {
            if pins.slots[at] != usize::MAX {
                continue;
            }
            // Prefer an empty slot, otherwise the least recently used unpinned slot.
            // Excluding each selected slot prevents two missing pages claiming it.
            let slot = self.slots[..capacity]
                .iter()
                .enumerate()
                .filter(|(at, slot)| !selected[*at] && slot.pins == 0)
                .min_by_key(|(_, slot)| (slot.page.is_some(), slot.used))
                .map(|(at, _)| at)?;
            pins.slots[at] = slot;
            selected[slot] = true;
        }
        Some(pins)
    }

    /// Installs the original request only after every required real eviction succeeds.
    /// No visitor observes the intermediate directory state.
    pub(super) fn pin(&mut self, pins: LeasePins) {
        // The clock chooses replacement order only. Saturation keeps deterministic
        // ties within the fixed budget instead of overflowing into new authority.
        self.clock = self.clock.saturating_add(1);
        for at in 0..pins.count {
            let slot = &mut self.slots[pins.slots[at]];
            slot.page = Some(pins.pages[at]);
            slot.pins += 1; // The same lock limits active leases to 32, so a slot cannot exceed 32 pins.
            slot.used = self.clock;
        }
    }

    pub(super) fn managed_bytes(&self, capacity: usize, page_size: usize) -> usize {
        // Only admitted directory entries count toward this owner's budget.
        // The kernel's file cache and unrelated process allocations live elsewhere.
        self.slots[..capacity]
            .iter()
            .filter(|slot| slot.page.is_some())
            .count()
            * page_size
    }
}
