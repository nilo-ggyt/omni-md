//! 固定64KiB系统页目录；只保存地址范围和借用计数，不复制正文或保活外部对象。

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
    /// 只计划完整请求；没有足够未pin槽时不先驱逐或取得半份权限。
    pub(super) fn plan(&self, pages: &[usize], capacity: usize) -> Option<LeasePins> {
        if pages.len() > 2 || capacity > self.slots.len() {
            return None;
        }
        let mut pins = LeasePins {
            count: pages.len(),
            ..LeasePins::default()
        };
        let mut selected = [false; MAX_WINDOW_SLOTS];
        // 先固定已经命中的全部槽，后续缺页不能错误驱逐同一请求的另一页。
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
            // 空槽优先，否则取最旧的未pin槽；选中的槽立即排除，两个缺页不能争同一槽。
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

    /// 在真实驱逐全部完成后一次安装原请求；期间不会向业务回调暴露槽。
    pub(super) fn pin(&mut self, pins: LeasePins) {
        // clock只影响替换次序；饱和后的同龄槽仍保持固定预算和确定性，不溢出成新权限。
        self.clock = self.clock.saturating_add(1);
        for at in 0..pins.count {
            let slot = &mut self.slots[pins.slots[at]];
            slot.page = Some(pins.pages[at]);
            slot.pins += 1; // 全局活lease已在同一锁内限制到32，单槽不会超过32。
            slot.used = self.clock;
        }
    }

    pub(super) fn managed_bytes(&self, capacity: usize, page_size: usize) -> usize {
        self.slots[..capacity]
            .iter()
            .filter(|slot| slot.page.is_some())
            .count()
            * page_size
    }
}
