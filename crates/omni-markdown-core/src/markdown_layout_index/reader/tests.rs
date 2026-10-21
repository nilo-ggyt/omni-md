//! Exercise actual final-reader handoff, bounded record delivery, and descriptor release.
//! Existing private observations supplement those results without claiming Native or UI acceptance.

use super::*;
use crate::markdown_layout_index::PagedLayoutIndex;
use std::os::fd::AsRawFd;
#[cfg(feature = "paged-shaping-frontier")]
#[path = "prefix_tests.rs"]
mod prefix_tests;

fn record<const N: usize>(row: usize) -> [u8; N] {
    // Encode ordinal and a trailing witness independently of the reader's returned storage.
    let mut bytes = [0; N];
    bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
    bytes[N - 1] = (row % 251) as u8;
    bytes
}

fn index<const N: usize>(count: usize) -> PagedLayoutIndex<N> {
    // Build each fixture through real appends; the resulting production owner keeps the actual
    // private buffer and lazily allocated anonymous file until handoff or failure consumes them.
    let mut index = PagedLayoutIndex::new().unwrap();
    for row in 0..count {
        index.append(record(row)).unwrap();
    }
    index
}

#[test]
fn empty_final_handoff_creates_one_real_file_without_allocating_a_record_buffer() {
    // An empty writer needs no private page, but final handoff still has one actual descriptor
    // owner. Read and close therefore use the ordinary reader path rather than an empty substitute.
    let index = index::<64>(0);
    let observer = index.resource_observer();
    assert!(!observer.snapshot().file_open);
    assert_eq!(observer.snapshot().peak_buffer_bytes, 0);
    let reader = index.into_reader().unwrap();
    // Empty output transfers a real file; the existing reader close and logical-EOF protocol
    // needs neither another implementation nor a fallback owner.
    assert!(observer.snapshot().file_open);
    #[cfg(feature = "paged-graphemes")]
    assert!(observer.snapshot().owner_closed); // Writer handoff ended, while the reader below still exclusively owns the file.
    assert_eq!(reader.file.as_ref().unwrap().metadata().unwrap().len(), 0);
    assert_eq!(reader.len(), 0);
    reader.read_batch(0..0, &mut []).unwrap();
    reader.close().unwrap();
    let released = observer.snapshot();
    assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
    assert_eq!(released.peak_buffer_bytes, 0);
}

#[test]
fn seal_moves_the_same_file_to_a_naturally_send_reader_and_closes_on_worker() {
    // Retain the existing compile-time Send/Sync check, then prove actual same-descriptor
    // transfer and worker-owned reading/close separately through real file results.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LayoutIndexReader<64>>();
    let index = index::<64>(2051);
    let observer = index.resource_observer();
    let descriptor = index.file.as_ref().unwrap().as_raw_fd();
    assert_eq!(observer.snapshot().mapped_bytes, 0);
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    let reader = index.into_reader().unwrap();
    assert_eq!(reader.file.as_ref().unwrap().as_raw_fd(), descriptor);
    let transferred = reader.resource_observer().snapshot();
    assert!(
        transferred.file_open && transferred.mapped_bytes == 0 && transferred.buffer_bytes == 0
    );
    assert!(!transferred.reader_failed && !transferred.cleanup_failed);

    // The builder never crosses threads. Only its sealed original file and atomic receipt move
    // to this real worker; bounded reads must not create another mapping or mutable page cache.
    std::thread::spawn(move || {
        assert_eq!(reader.len(), 2051);
        assert_eq!(reader.read(2050).unwrap(), record::<64>(2050));
        let mut output = [[0; 64]; 256];
        reader.read_batch(250..506, &mut output).unwrap();
        // Check every complete returned record against fixture bytes, including the old page boundary.
        for (offset, actual) in output.iter().enumerate() {
            assert_eq!(*actual, record::<64>(250 + offset));
        }
        reader.read_batch(2051..2051, &mut []).unwrap();
        assert_eq!(reader.resource_observer().snapshot().mapped_bytes, 0);
        reader.close().unwrap();
    })
    .join()
    .unwrap();
    // Join establishes that the worker actually closed its descriptor before inspecting the
    // original receipt. A queued completion or dropped caller claim would not prove that release.
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(!released.reader_failed && !released.cleanup_failed);
    assert_eq!(released.peak_mapped_bytes, 0);
    assert_eq!(released.peak_buffer_bytes, PAGE);
}

#[test]
fn each_batch_limit_and_exact_output_length_revoke_without_modifying_output() {
    fn rejects<const N: usize>(range: Range<usize>, output_count: usize) {
        // Each invalid request gets its own real reader. Admission failure must leave the entire
        // caller buffer unchanged, revoke later reads, and still return its actual descriptor.
        let reader = index::<N>(300).into_reader().unwrap();
        let observer = reader.resource_observer();
        let mut output = vec![[0xab; N]; output_count];
        assert!(matches!(
            reader.read_batch(range, &mut output),
            Err(Error::InvalidRecord)
        ));
        assert!(output.iter().all(|bytes| *bytes == [0xab; N]));
        assert!(matches!(reader.read(0), Err(Error::Poisoned)));
        assert!(matches!(reader.close(), Err(Error::Poisoned)));
        let released = observer.snapshot();
        assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
        assert!(released.reader_failed && !released.cleanup_failed);
    }
    // Record-count budget, byte budget, output length, reversal, and EOF reject independently;
    // none may be silently truncated into a smaller successful batch.
    rejects::<64>(0..257, 257);
    rejects::<128>(0..129, 129);
    rejects::<64>(0..2, 1);
    let reversed = Range { start: 2, end: 1 };
    rejects::<64>(reversed, 0);
    rejects::<64>(300..301, 1);
}

#[test]
fn actual_short_read_keeps_partial_output_negative_and_returns_the_file() {
    let reader = index::<64>(2).into_reader().unwrap();
    let observer = reader.resource_observer();
    // Truncate the one original file only inside this test, producing actual pread progress
    // followed by EOF. Do not duplicate its descriptor or close a number the OS could reuse.
    reader.file.as_ref().unwrap().set_len(96).unwrap();
    let mut output = [[0xab; 64]; 2];
    let error = reader.read_batch(0..2, &mut output).unwrap_err();
    assert!(
        matches!(error, Error::Io(ref cause) if cause.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    assert_eq!(output[0], record::<64>(0));
    assert_eq!(&output[1][..32], &record::<64>(1)[..32]);
    assert_eq!(&output[1][32..], &[0xab; 32]);
    // A partially filled output is not a successful batch. The original reader stays poisoned,
    // but explicit close must independently settle the descriptor without a cleanup failure.
    assert!(matches!(reader.read(0), Err(Error::Poisoned)));
    assert!(matches!(reader.close(), Err(Error::Poisoned)));
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(released.reader_failed && !released.cleanup_failed);
}

#[test]
fn actual_readonly_publish_failure_never_delivers_a_reader_and_returns_its_buffer() {
    // Supply an actual read-only descriptor through the existing test constructor. Private
    // sealing stays local; final handoff must attempt publication and receive a real EBADF.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly-index");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut index = PagedLayoutIndex::<64>::with_file(File::open(&path).unwrap()).unwrap();
    for row in 0..3 {
        index.append(record::<64>(row)).unwrap();
    }
    let observer = index.resource_observer();
    index.seal().unwrap(); // Private sealing writes nothing; actual into_reader must first publish every record to the original file.
    assert_eq!(index.read(2).unwrap(), record::<64>(2));
    assert!(
        matches!(index.into_reader(), Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    // No reader was delivered. Its consumed writer must return buffer/file ownership while the
    // original read-only bytes remain unchanged and the publication cleanup failure stays visible.
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(released.cleanup_failed && !released.reader_failed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[test]
fn dropping_a_transferred_reader_releases_the_original_file_without_a_second_owner() {
    // Exercise the ordinary RAII exit on the actual transferred reader, without explicitly
    // closing a second descriptor owner or replacing release with a synthetic receipt.
    let reader = index::<64>(2).into_reader().unwrap();
    let observer = reader.resource_observer();
    drop(reader);
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(!released.reader_failed && !released.cleanup_failed);
}
