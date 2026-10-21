//! Actual page admission, concurrent leases, and kernel release results.
//! Managed slots and successful msync calls are not process RSS measurements.

use std::io::Write;
use std::sync::mpsc;

use super::*;

fn fixture() -> OriginalBacking {
    let mut file = tempfile::tempfile().unwrap();
    // Distinct bytes identify each finite source page after eviction and reloading.
    let bytes: Vec<_> = (0..MANAGED_WINDOW_BYTES * 3)
        .map(|at| (at / 4096) as u8)
        .collect();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
    // SAFETY: The anonymous inode has no other handles or writers, and its
    // validated extent equals the completed fixture writes.
    unsafe { OriginalBacking::map(file, bytes.len()) }.unwrap()
}

#[test]
fn same_system_page_hits_do_not_reclaim_on_every_lease_and_close_reclaims_once() {
    let backing = fixture();
    let observer = backing.observer();
    // Repeated real reads of one page must reuse its admitted slot; final close
    // must still perform the actual eviction and unmap operations.
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
    // Exceed the actual host page directory without retaining byte slices.
    // Every returned value must still come from the original frozen inode.
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
    // Keep every real lease alive so a new page cannot claim an unpinned victim.
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
    assert!(!observer.failed()); // Admission pressure does not revoke other healthy leases.
    // Returning the actual leases makes a victim available; rejection must not
    // have permanently damaged the mapping or its original bytes.
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
    // Leave only one slot free: the crossing request needs two distinct pages
    // and must fail before any eviction or partial admission.
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
    // The failed request must leave the original borrowed values readable.
    for (page, lease) in held.iter().enumerate() {
        assert_eq!(&**lease, &[(page * backing.page_size / 4096) as u8]);
    }
    drop(held);
    if backing.window_slots >= 2 {
        // Once the old owners return their claims, the exact two-byte request
        // succeeds through the same admission path and is explicitly finished.
        let lease = backing.lease(start..start + 2).unwrap();
        assert_eq!(&*lease, &[(start / 4096) as u8, ((start + 1) / 4096) as u8]);
        lease.finish().unwrap();
    }
    backing.close().unwrap();
}

#[test]
fn overlapping_threads_keep_one_true_pin_until_each_lease_finishes() {
    let backing = Arc::new(fixture());
    let observer = backing.observer();
    // Scoped threads always join. Dropping a permit sender also wakes its worker
    // during unwinding, so a failed foreground assertion cannot strand a barrier.
    std::thread::scope(|scope| {
        let (ready, acquired) = mpsc::channel();
        let mut permits = Vec::new();
        for _ in 0..2 {
            let backing = Arc::clone(&backing);
            let ready = ready.clone();
            let (permit, released) = mpsc::channel::<()>();
            permits.push(permit);
            scope.spawn(move || {
                let lease = backing.lease(0..1).unwrap();
                ready.send(()).unwrap();
                drop(ready); // A sibling failing before admission must disconnect readiness, too.
                // Both normal completion and sender disconnection return this
                // original lease before the scoped worker can terminate.
                let _ = released.recv();
                assert_eq!(&*lease, &[0]);
                lease.finish().unwrap();
            });
        }
        drop(ready);
        for _ in 0..2 {
            acquired.recv().unwrap();
        }
        // Both readers retain the first page while unrelated unpinned pages
        // cycle through the fixed directory. Their borrowed bytes stay valid.
        for page in 1..backing.window_slots + 3 {
            let at = page * backing.page_size;
            if backing.window_slots > 1 {
                backing
                    .with_bytes(at..at + 1, |bytes| assert_eq!(bytes[0], (at / 4096) as u8))
                    .unwrap();
            }
        }
        drop(permits);
    });
    drop(backing);
    assert!(observer.unmap_succeeded());
    assert!(!observer.failed());
}
