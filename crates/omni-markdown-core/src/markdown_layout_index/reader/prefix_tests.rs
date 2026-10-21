//! Exercise immutable prefix authority, descriptor quota, and failure release while the original
//! writer keeps appending to the same anonymous inode, without a resident copy of layout records.

use super::*;
use crate::markdown_layout_index::LayoutIndexSnapshotError;
use std::{
    os::unix::fs::MetadataExt,
    sync::{Arc, Barrier, atomic::Ordering},
};

#[test]
fn immutable_prefix_has_a_distinct_fd_and_hard_logical_end_while_writer_continues() {
    // First publication gives a distinct owned descriptor to the same real inode. Reader
    // logical bounds remain independent of both later appends and the writer's current length.
    let mut writer = index::<64>(2);
    assert!(!writer.resource_observer().snapshot().file_open);
    let first = writer.snapshot_reader(1).unwrap(); // First publication creates the original file and flushes only the committed suffix.
    let writer_fd = writer.file.as_ref().unwrap().as_raw_fd();
    let identity = writer.file.as_ref().unwrap().metadata().unwrap();
    let first_observer = first.resource_observer();
    let actual = first.file.as_ref().unwrap();
    assert_ne!(actual.as_raw_fd(), writer_fd);
    let metadata = actual.metadata().unwrap();
    assert_eq!(
        (metadata.dev(), metadata.ino()),
        (identity.dev(), identity.ino())
    );
    assert_eq!(first_observer.snapshot().mapped_bytes, 0);
    assert_eq!(writer.resource_observer().snapshot().mapped_bytes, 0);
    assert_eq!(writer.resource_observer().snapshot().buffer_bytes, PAGE);
    let second = writer.snapshot_reader(2).unwrap();
    // A full delivery quota is backpressure, not writer failure. Releasing a failed old reader
    // must return its own slot without revoking the still-active producer or another reader.
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 2);
    assert!(matches!(
        writer.snapshot_reader(2),
        Err(LayoutIndexSnapshotError::ReadersBusy)
    ));
    writer.append(record::<64>(2)).unwrap(); // Quota restricts delivery only; it must not poison the producer or prevent new records.
    assert_eq!(first.read(0).unwrap(), record::<64>(0));
    assert_eq!(second.read(1).unwrap(), record::<64>(1));
    assert!(matches!(first.read(1), Err(Error::InvalidRecord))); // An existing row in the same inode does not extend this snapshot's logical EOF.
    assert!(matches!(first.close(), Err(Error::Poisoned)));
    assert!(!first_observer.snapshot().file_open);
    assert!(!first_observer.snapshot().cleanup_failed);
    writer.append(record::<64>(3)).unwrap(); // Invalid reader coordinates revoke that reader only, without ending the successor worker's producer.
    let third = writer.snapshot_reader(4).unwrap();
    assert_eq!(third.read(3).unwrap(), record::<64>(3));
    second.close().unwrap();
    third.close().unwrap();
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
    writer.close().unwrap();
}

#[test]
fn snapshot_reads_on_another_thread_while_the_original_writer_appends_across_pages() {
    // A real barrier starts immutable-prefix reads alongside original-writer appends. No
    // shared writable record page or timed success polling substitutes for this descriptor use.
    let mut writer = index::<64>(3);
    let reader = writer.snapshot_reader(3).unwrap();
    let observer = reader.resource_observer();
    let ready = Arc::new(Barrier::new(2));
    let other = Arc::clone(&ready);
    let thread = std::thread::spawn(move || {
        other.wait();
        for round in 0..256 {
            let row = round % 3;
            assert_eq!(reader.read(row).unwrap(), record::<64>(row));
        } // This fixed read sequence checks old rows concurrently, without sleeps, success polling, or a shared writable page.
        reader.close().unwrap();
    });
    ready.wait();
    for row in 3..1_027 {
        writer.append(record::<64>(row)).unwrap();
    }
    thread.join().unwrap();
    // Join settles the actual prefix owner before its quota slot and a later complete prefix
    // are inspected. Closing the reader must not stop the independent writer from progressing.
    assert!(!observer.snapshot().file_open && !observer.snapshot().cleanup_failed);
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
    let last = writer.snapshot_reader(1_027).unwrap();
    assert_eq!(last.read(1_026).unwrap(), record::<64>(1_026));
    last.close().unwrap();
    writer.close().unwrap();
}

#[test]
fn normal_writer_drop_keeps_existing_snapshot_readable_until_its_own_actual_close() {
    // Only the first row is published. Dropping the writer discards its two private rows and
    // descriptor, while the already delivered prefix keeps its own same-inode read authority.
    let mut writer = index::<64>(2);
    let original = writer.resource_observer();
    let reader = writer.snapshot_reader(1).unwrap();
    let snapshot = reader.resource_observer();
    writer.append(record::<64>(2)).unwrap();
    assert_eq!(writer.file.as_ref().unwrap().metadata().unwrap().len(), 64);
    drop(writer); // Three records exist, but only one was published; the two unpublished rows return with the buffer.
    assert!(
        !original.snapshot().file_open
            && original.snapshot().mapped_bytes == 0
            && original.snapshot().buffer_bytes == 0
    );
    assert!(snapshot.snapshot().file_open && !snapshot.snapshot().reader_failed);
    assert_eq!(reader.read(0).unwrap(), record::<64>(0));
    reader.close().unwrap();
    assert!(!snapshot.snapshot().file_open);
}

#[test]
fn actual_readonly_prefix_write_failure_returns_the_reserved_fd_slot() {
    // The existing constructor supplies a real read-only file. Prefix publication reserves
    // delivery capacity before an actual write fails, so all reservation and owner cleanup matters.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly-prefix");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut writer = PagedLayoutIndex::<64>::with_file(File::open(&path).unwrap()).unwrap();
    writer.append(record::<64>(0)).unwrap();
    let original = writer.resource_observer();
    assert!(matches!(writer.snapshot_reader(1),
        Err(LayoutIndexSnapshotError::Index(Error::Io(error))) if error.raw_os_error() == Some(libc::EBADF)));
    // Actual write failure occurs before dup. It must return the undelivered reader slot and
    // leave the original buffer revoked rather than permitting another publication attempt.
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
    assert!(matches!(writer.read(0), Err(Error::Poisoned)));
    assert!(matches!(
        writer.snapshot_reader(1),
        Err(LayoutIndexSnapshotError::Index(Error::Poisoned))
    ));
    assert!(matches!(writer.close(), Err(Error::Poisoned)));
    let released = original.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(released.cleanup_failed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[test]
fn actual_short_read_revokes_the_same_inode_producer_before_any_new_append() {
    // The snapshot and writer use distinct descriptors for one inode. Actual snapshot I/O
    // failure must propagate through their shared source-failure authority before further append.
    let mut writer = index::<64>(2);
    let reader = writer.snapshot_reader(2).unwrap();
    reader.file.as_ref().unwrap().set_len(96).unwrap(); // Test-only truncation of the same inode; the writer buffer cannot hide this reader's actual pread failure.
    let mut rows = [[0xab; 64]; 2];
    assert!(
        matches!(reader.read_batch(0..2, &mut rows), Err(Error::Io(ref error)) if error.kind()==std::io::ErrorKind::UnexpectedEof)
    );
    assert_eq!(rows[0], record::<64>(0));
    assert!(matches!(
        writer.append(record::<64>(2)),
        Err(Error::Poisoned)
    ));
    assert_eq!(writer.resource_observer().snapshot().mapped_bytes, 0);
    assert_eq!(writer.resource_observer().snapshot().buffer_bytes, PAGE);
    assert!(matches!(reader.close(), Err(Error::Poisoned)));
    writer.close().unwrap(); // Data failure and actual descriptor cleanup are separate facts; this close has no kernel error.
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
}

#[test]
fn final_same_fd_handoff_and_prior_prefix_share_real_io_failure_without_copying_records() {
    // Final handoff moves the writer's own descriptor while an earlier prefix owns its dup.
    // Truncating their actual shared inode makes final-reader I/O fail and revokes that prefix,
    // without copying records or substituting another failure flag solely for this assertion.
    let mut writer = index::<64>(2);
    let prefix = writer.snapshot_reader(1).unwrap();
    let prefix_observer = prefix.resource_observer();
    let final_reader = writer.into_reader().unwrap();
    final_reader.file.as_ref().unwrap().set_len(0).unwrap();
    assert!(matches!(final_reader.read(0), Err(Error::Io(_))));
    assert!(matches!(prefix.read(0), Err(Error::Poisoned)));
    assert!(matches!(final_reader.close(), Err(Error::Poisoned)));
    assert!(matches!(prefix.close(), Err(Error::Poisoned)));
    assert!(!prefix_observer.snapshot().file_open);
    assert!(!prefix_observer.snapshot().cleanup_failed);
}
