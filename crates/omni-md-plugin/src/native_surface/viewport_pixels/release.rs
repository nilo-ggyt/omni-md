//! Closed pixels complete release only after actual final munmap; subscription and final destruction
//! share one small critical section.

use super::*;
use crate::native_surface::deallocation::NativeMarkdownDeallocationClaim;

#[derive(Default)]
pub(super) struct ReleaseState {
    pub(super) sealed: bool,
    claims: Vec<NativeMarkdownDeallocationClaim>,
}

impl NativeMarkdownViewportPixels {
    /// Register the original close-cohort obligation; already-released pixels use the same actual fact,
    /// preserving completion-before-subscription.
    pub(in crate::native_surface) fn observe_release(
        &self,
        claim: NativeMarkdownDeallocationClaim,
    ) {
        let ready = {
            let mut release = self.release.lock().expect("像素释放锁不跨外部调用");
            release.sealed = true;
            release.claims.push(claim);
            self.take_released_claims(&mut release)
        };
        for claim in ready {
            claim.release(); // Release the resource lock before queueing the existing cohort, without Native calls or retained business ownership.
        }
    }

    /// Call only after actual munmap success; update the three release counters and notification
    /// decision under one lock so the final update cannot lose its wake.
    pub(super) fn released_allocation(&self, length: usize, receipt: Option<&AtomicUsize>) {
        let ready = {
            let mut release = self.release.lock().expect("像素释放锁不跨外部调用");
            self.live_pixel_buffers.fetch_sub(1, Ordering::AcqRel);
            self.live_pixel_bytes.fetch_sub(length, Ordering::AcqRel);
            self.reserved_bytes.fetch_sub(length, Ordering::AcqRel);
            if let Some(receipt) = receipt {
                receipt.store(1, Ordering::Release); // Actual munmap and the original allocation receipt precede cohort-release authority.
            }
            self.take_released_claims(&mut release)
        };
        for claim in ready {
            claim.release(); // This may run on a Core Graphics release thread, carrying Send/Sync atomic metadata only.
        }
    }

    /// Transfer subscribed claims only after every actual allocation counter reached zero without
    /// failure.
    fn take_released_claims(
        &self,
        release: &mut ReleaseState,
    ) -> Vec<NativeMarkdownDeallocationClaim> {
        if release.sealed
            && self.live_pixel_buffers.load(Ordering::Acquire) == 0
            && self.live_pixel_bytes.load(Ordering::Acquire) == 0
            && self.reserved_bytes.load(Ordering::Acquire) == 0
            && !self.pixel_release_failed.load(Ordering::Acquire)
        {
            std::mem::take(&mut release.claims)
        } else {
            Vec::new() // Retain obligations while an allocation lives or release failed, without polling, timeouts, or invented success counters.
        }
    }
}
