//! 封闭后的原像素只由真实最后munmap归还；登记和最后析构共用同一小临界区。

use super::*;
use crate::native_surface::deallocation::NativeMarkdownDeallocationClaim;

#[derive(Default)]
pub(super) struct ReleaseState {
    pub(super) sealed: bool,
    claims: Vec<NativeMarkdownDeallocationClaim>,
}

impl NativeMarkdownViewportPixels {
    /// 关闭装配登记原cohort义务；已同步归零也交同一真实事实，不丢先完成后订阅的事件。
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
            claim.release(); // 先结束资源锁，再交既有cohort排队；不执行Native或保活业务owner。
        }
    }

    /// 此方法只在实际munmap成功后调用；原三项归还和通知决定处于同一锁，最后更新不会漏唤醒。
    pub(super) fn released_allocation(&self, length: usize, receipt: Option<&AtomicUsize>) {
        let ready = {
            let mut release = self.release.lock().expect("像素释放锁不跨外部调用");
            self.live_pixel_buffers.fetch_sub(1, Ordering::AcqRel);
            self.live_pixel_bytes.fetch_sub(length, Ordering::AcqRel);
            self.reserved_bytes.fetch_sub(length, Ordering::AcqRel);
            if let Some(receipt) = receipt {
                receipt.store(1, Ordering::Release); // 真实munmap与原分配收据先完成，后授cohort归还权。
            }
            self.take_released_claims(&mut release)
        };
        for claim in ready {
            claim.release(); // 可以来自CoreGraphics后台释放栈；此处仍只有Send/Sync原子元数据。
        }
    }

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
            Vec::new() // 原分配仍活或归还失败时保持义务；没有轮询、超时或成功计数补写。
        }
    }
}
