//! Actual VM/descriptor success and failure receipts. Existing injection changes only this test's
//! syscall arguments; it neither fabricates success nor leaves test-owned resources behind.
use super::*;
use crate::immutable_snapshot::snapshot_font_file;
use std::{cell::Cell, fs::File};

fn frozen() -> (tempfile::TempDir, File, FrozenFontFile) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("font.bin");
    std::fs::write(&path, [0, 0xff, 0x80, 7]).unwrap();
    let source = File::open(path).unwrap();
    let frozen = snapshot_font_file(&source, 4).unwrap().unwrap();
    (directory, source, frozen)
}

#[test]
fn successful_map_closes_backing_fd_and_drop_reports_actual_unmap() {
    let (_directory, _source, file) = frozen();
    let mapping = ReadonlyFontMapping::map(file).unwrap();
    let facts = mapping.facts();
    assert_eq!(mapping.as_ref(), [0, 0xff, 0x80, 7]);
    assert!(facts.backing_closed.load(Ordering::Acquire));
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), 4);
    assert!(!facts.completed.load(Ordering::Acquire));
    drop(mapping);
    assert!(facts.completed.load(Ordering::Acquire));
    assert!(facts.unmapped.load(Ordering::Acquire));
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), 0);
    assert_eq!(facts.first_errno(), None);
}

#[test]
fn real_failing_close_syscall_prevents_publication_and_preserves_negative_fact_after_cleanup() {
    let (_directory, _source, file) = frozen();
    let facts = Arc::new(MappingFacts::default());
    let original_fd = Cell::new(None);
    let result = ReadonlyFontMapping::map_with_close(file, Arc::clone(&facts), |fd| {
        original_fd.set(Some(fd));
        // Make a real failing close on invalid -1, never double-close the actual descriptor and
        // risk closing another concurrent test's reused resource.
        close_descriptor(-1)
    });
    assert!(result.is_err());
    assert_eq!(facts.first_errno(), Some(libc::EBADF));
    assert!(!facts.backing_closed.load(Ordering::Acquire));
    assert!(facts.completed.load(Ordering::Acquire));
    assert!(facts.unmapped.load(Ordering::Acquire)); // Actual compensating unmap does not erase the original close failure.
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), 0);
    // Injection did not close the real descriptor. Return it separately without rewriting this
    // external compensation as a successful production-owner receipt.
    close_descriptor(original_fd.get().unwrap()).unwrap();
    assert!(!facts.backing_closed.load(Ordering::Acquire));
    assert_eq!(facts.first_errno(), Some(libc::EBADF));
}

#[test]
fn actual_misaligned_unmap_failure_never_fakes_zero_and_drop_does_not_retry() {
    let (_directory, _source, file) = frozen();
    let mut mapping = ReadonlyFontMapping::map(file).unwrap();
    let facts = mapping.facts();
    let address = mapping.address;
    let length = mapping.length;
    let result = mapping.release_with(|address, length| {
        // SAFETY: an intentionally unaligned address makes munmap return EINVAL; it neither
        // unmaps the valid extent nor exposes a reference through that pointer.
        unsafe { libc::munmap(address.add(1).cast(), length) }
    });
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EINVAL));
    assert_eq!(facts.first_errno(), Some(libc::EINVAL));
    assert!(facts.completed.load(Ordering::Acquire));
    assert!(!facts.unmapped.load(Ordering::Acquire));
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), length);
    drop(mapping); // The attempted owner cannot retry this address; its negative facts remain intact.
    assert!(!facts.unmapped.load(Ordering::Acquire));
    // SAFETY: this isolated test still exclusively owns the known actual mapping with no borrows;
    // compensate its injected failure explicitly rather than leaving VM in the test process.
    assert_eq!(unsafe { libc::munmap(address.as_ptr().cast(), length) }, 0);
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), length); // External recovery cannot rewrite the owner's failure as success.
    assert_eq!(facts.first_errno(), Some(libc::EINVAL));
}
