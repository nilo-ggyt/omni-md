//! 真正系统页驱逐、并发pin与原子准入对照；没有用Drop步骤代替物理VM收据。

use std::io::Write;
use std::sync::Barrier;

use super::*;

fn fixture() -> OriginalBacking {
    let mut file = tempfile::tempfile().unwrap();
    // 有限测试显式预置输入；每页首字节不同，驱逐重载后可以核同一冻结内容。
    let bytes: Vec<_> = (0..MANAGED_WINDOW_BYTES * 3)
        .map(|at| (at / 4096) as u8)
        .collect();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
    // SAFETY: 独立匿名文件写者已结束，没有外部句柄，长度与写入完全一致。
    unsafe { OriginalBacking::map(file, bytes.len()) }.unwrap()
}

#[test]
fn same_system_page_hits_do_not_reclaim_on_every_lease_and_close_reclaims_once() {
    let backing = fixture();
    let observer = backing.observer();
    for _ in 0..500 {
        backing
            .with_bytes(1..9, |bytes| assert_eq!(bytes, &[0; 8]))
            .unwrap();
    }
    assert_eq!(observer.managed_resident_bytes(), backing.page_size);
    assert_eq!(observer.reclaim_attempts(), 0);
    backing.close().unwrap();
    assert_eq!(observer.reclaim_attempts(), 1);
    assert_eq!(observer.reclaim_successes(), 1);
    assert_eq!(observer.managed_resident_bytes(), 0);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.first_errno(), None);
}

#[test]
fn sequential_eviction_has_a_fixed_managed_budget_and_retains_original_values() {
    let backing = fixture();
    let observer = backing.observer();
    for page in 0..backing.window_slots + 2 {
        let at = page * backing.page_size;
        backing
            .with_bytes(at..at + 1, |bytes| assert_eq!(bytes[0], (at / 4096) as u8))
            .unwrap();
        assert!(observer.managed_resident_bytes() <= MANAGED_WINDOW_BYTES);
    }
    assert_eq!(observer.reclaim_attempts(), 2);
    assert_eq!(observer.peak_managed_resident_bytes(), MANAGED_WINDOW_BYTES);
    backing
        .with_bytes(0..1, |bytes| assert_eq!(bytes, &[0]))
        .unwrap();
    assert_eq!(observer.reclaim_attempts(), 3);
    backing.close().unwrap();
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
    assert!(!observer.failed());
}

#[test]
fn every_pinned_slot_rejects_a_new_page_without_eviction_or_extra_slots() {
    let backing = fixture();
    let observer = backing.observer();
    let mut held = Vec::new();
    for page in 0..backing.window_slots {
        held.push(
            backing
                .lease(page * backing.page_size..page * backing.page_size + 1)
                .unwrap(),
        );
    }
    assert!(matches!(
        backing.lease(MANAGED_WINDOW_BYTES..MANAGED_WINDOW_BYTES + 1),
        Err(OriginalBackingError::LeaseLimitExceeded)
    ));
    assert_eq!(observer.reclaim_attempts(), 0);
    assert_eq!(observer.managed_resident_bytes(), MANAGED_WINDOW_BYTES);
    assert!(!observer.failed()); // 预算不足是有限准入失败，不毒化其他有效借用。
    drop(held);
    backing
        .with_bytes(MANAGED_WINDOW_BYTES..MANAGED_WINDOW_BYTES + 1, |_| ())
        .unwrap();
    assert_eq!(observer.reclaim_attempts(), 1);
    backing.close().unwrap();
}

#[test]
fn crossing_two_pages_is_admitted_atomically_or_leaves_existing_pins_unchanged() {
    let backing = fixture();
    let observer = backing.observer();
    let held: Vec<_> = (0..backing.window_slots.saturating_sub(1))
        .map(|page| {
            backing
                .lease(page * backing.page_size..page * backing.page_size + 1)
                .unwrap()
        })
        .collect();
    let start = MANAGED_WINDOW_BYTES + backing.page_size - 1;
    assert!(matches!(
        backing.lease(start..start + 2),
        Err(OriginalBackingError::LeaseLimitExceeded)
    ));
    assert_eq!(observer.reclaim_attempts(), 0);
    let state = backing.windows.lock().unwrap();
    assert_eq!(
        state.slots.iter().map(|slot| slot.pins).sum::<usize>(),
        held.len()
    );
    assert_eq!(
        state
            .slots
            .iter()
            .filter(|slot| slot.page.is_some())
            .count(),
        held.len()
    );
    drop(state);
    drop(held);
    if backing.window_slots >= 2 {
        let lease = backing.lease(start..start + 2).unwrap();
        assert_eq!(lease.pins.count, 2);
        assert_eq!(lease.len(), 2);
        lease.finish().unwrap();
    }
    backing.close().unwrap();
}

#[test]
fn overlapping_threads_keep_one_true_pin_until_each_lease_finishes() {
    let backing = Arc::new(fixture());
    let observer = backing.observer();
    let acquired = Arc::new(Barrier::new(3));
    let released = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let backing = Arc::clone(&backing);
        let acquired = Arc::clone(&acquired);
        let released = Arc::clone(&released);
        workers.push(std::thread::spawn(move || {
            let lease = backing.lease(0..1).unwrap();
            acquired.wait();
            released.wait();
            assert_eq!(&*lease, &[0]);
            lease.finish().unwrap();
        }));
    }
    acquired.wait();
    for page in 1..backing.window_slots + 3 {
        let at = page * backing.page_size;
        // 两线程仍持首槽；其他未pin页可轮换，首槽不能被驱逐或扣成一次借用。
        if backing.window_slots > 1 {
            backing.with_bytes(at..at + 1, |_| ()).unwrap();
        }
    }
    let state = backing.windows.lock().unwrap();
    assert_eq!(
        state
            .slots
            .iter()
            .find(|slot| slot.page == Some(0))
            .unwrap()
            .pins,
        2
    );
    drop(state);
    released.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
    drop(backing);
    assert!(observer.unmap_succeeded());
    assert!(!observer.failed());
}

#[test]
fn mutex_poison_and_eviction_io_failure_remain_sticky_through_final_real_unmap() {
    let backing = fixture();
    let observer = backing.observer();
    backing.with_bytes(0..1, |_| ()).unwrap();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _state = backing.windows.lock().unwrap();
        panic!("合成目录poison");
    }));
    assert!(poisoned.is_err());
    assert!(matches!(
        backing.lease(0..1),
        Err(OriginalBackingError::Unavailable(libc::EIO))
    ));
    assert_eq!(
        backing.close(),
        Err(OriginalBackingError::Unavailable(libc::EIO))
    );
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.managed_resident_bytes(), 0);
    assert_eq!(observer.first_errno(), Some(libc::EIO));

    let backing = fixture();
    let observer = backing.observer();
    for page in 0..backing.window_slots {
        backing
            .with_bytes(
                page * backing.page_size..page * backing.page_size + 1,
                |_| (),
            )
            .unwrap();
    }
    observer.fail_next_reclaim_for_test();
    // 公开诊断票只设置同一个原子位；命中已有真实页仍可读取，不提前伪造I/O错误。
    assert!(!observer.failed());
    assert_eq!(observer.reclaim_attempts(), 0);
    backing
        .with_bytes(0..1, |bytes| assert_eq!(bytes, &[0]))
        .unwrap();
    assert!(!observer.failed());
    assert_eq!(observer.reclaim_attempts(), 0);
    assert_eq!(observer.reclaim_successes(), 0);
    assert_eq!(
        backing.with_bytes(MANAGED_WINDOW_BYTES..MANAGED_WINDOW_BYTES + 1, |_| ()),
        Err(OriginalBackingError::Unavailable(libc::EIO))
    );
    assert_eq!(
        backing.with_bytes(0..1, |_| ()),
        Err(OriginalBackingError::Unavailable(libc::EIO))
    );
    assert_eq!(observer.reclaim_attempts(), 1);
    assert_eq!(observer.reclaim_successes(), 1); // 注入只发生在实际msync返回成功之后。
    assert_eq!(
        backing.close(),
        Err(OriginalBackingError::Unavailable(libc::EIO))
    );
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(observer.failed());
}
