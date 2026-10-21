//! Exercise cross-page reads of the original anonymous index, bounded I/O, and revocation.
//! Existing buffer-identity assertions inspect this implementation; they do not establish UI behavior.

use super::*;

fn record<const N: usize>(row: usize) -> [u8; N] {
    // Encode both ordinal and a trailing witness so reads must preserve the complete fixed-width record.
    let mut bytes = [0; N];
    bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
    bytes[N - 1] = (row % 251) as u8;
    bytes
}

// Inspect only the actual Box address: cross-page pread and sealing must not replace this append buffer.
fn buffer_identity<const N: usize>(index: &PagedLayoutIndex<N>) -> *const u8 {
    index
        .page
        .as_ref()
        .unwrap()
        .bytes
        .as_ref()
        .unwrap()
        .as_ptr()
}

fn binary_search<const N: usize>(index: &mut PagedLayoutIndex<N>, target: usize) {
    // Query old and current pages through the real reader, checking each returned row before
    // using its stored ordinal to choose the next interval. No alternate metadata cache is built.
    let mut low = 0;
    let mut high = index.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let actual = index.read(middle).unwrap();
        assert_eq!(actual, record::<N>(middle));
        let row = u64::from_le_bytes(actual[..8].try_into().unwrap()) as usize;
        if row < target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    // The declared target exists, so exact lower-bound position and full target bytes must agree.
    assert_eq!(low, target);
    assert_eq!(index.read(low).unwrap(), record::<N>(target));
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn explicit_record_batches_match_original_rows_across_file_and_unpublished_append_buffer() {
    // An empty query must not acquire a file. Subsequent appends produce three flushed pages
    // and a dirty tail in the one original buffer, giving each batch a different real boundary.
    let per_page = PAGE / 64;
    let mut index = PagedLayoutIndex::<64>::new().unwrap();
    let mut empty: [[u8; 64]; 0] = [];
    index.read_batch(0..0, &mut empty).unwrap();
    assert!(!index.resource_observer().snapshot().file_open);
    for ordinal in 0..per_page * 3 + 7 {
        index.append(record::<64>(ordinal)).unwrap();
    }
    let identity = buffer_identity(&index);
    let file_length = index.file.as_ref().unwrap().metadata().unwrap().len();
    for first in [0, per_page - 1, per_page * 3 - 2, per_page * 3 + 4] {
        let mut rows = [[0; 64]; 3];
        index.read_batch(first..first + 3, &mut rows).unwrap();
        assert_eq!(rows, std::array::from_fn(|n| record::<64>(first + n)));
        assert_eq!(buffer_identity(&index), identity);
        assert_eq!(
            index.file.as_ref().unwrap().metadata().unwrap().len(),
            file_length
        );
    } // Old-page, file-to-dirty-tail, and buffer-only batches preserve rows without flushing for a query.
    // Private sealing removes append authority; batches still combine real file records and
    // the retained tail. Explicit close must revoke the same reader rather than reopen storage.
    index.seal().unwrap();
    let mut rows = [[0; 64]; 3];
    index
        .read_batch(per_page * 3 - 1..per_page * 3 + 2, &mut rows)
        .unwrap();
    assert_eq!(
        rows,
        std::array::from_fn(|n| record::<64>(per_page * 3 - 1 + n))
    );
    index.close().unwrap();
    assert!(matches!(
        index.read_batch(0..3, &mut rows),
        Err(Error::Poisoned)
    ));
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn explicit_batch_real_file_eof_rejects_its_partial_prefix_and_revokes_the_original_writer() {
    // Spill the first page, then shorten the real inode so this old-page batch reaches actual EOF.
    let mut index = PagedLayoutIndex::<64>::new().unwrap();
    for ordinal in 0..PAGE / 64 + 1 {
        index.append(record::<64>(ordinal)).unwrap();
    }
    let observer = index.resource_observer();
    // All three requested records belong to the old page; the current append buffer cannot
    // hide this disk short read or convert its available prefix into successful delivery.
    index.file.as_ref().unwrap().set_len(64).unwrap();
    let mut rows = [[0; 64]; 3];
    assert!(
        matches!(index.read_batch(0..3, &mut rows), Err(Error::Io(error))
        if error.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    // A data-read failure revokes subsequent use but does not manufacture a kernel close error.
    index.close().unwrap();
    let released = observer.snapshot();
    assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn explicit_batch_cannot_truncate_invalid_input_to_a_valid_record_prefix() {
    // Mismatched output length, beyond-EOF coverage, and reversed coordinates reject the whole
    // request. Each case owns a fresh index so one failure cannot contaminate another result.
    for (start, end) in [(0, 2), (1, 4), (3, 2)] {
        let mut index = PagedLayoutIndex::<64>::new().unwrap();
        for ordinal in 0..3 {
            index.append(record::<64>(ordinal)).unwrap();
        }
        let mut rows = [[0; 64]; 3];
        assert!(matches!(
            index.read_batch(start..end, &mut rows),
            Err(Error::InvalidRecord)
        ));
        assert!(matches!(index.read(0), Err(Error::Poisoned)));
        index.close().unwrap();
    } // No apparently valid subrange may replace the invalid original request.
}

#[test]
fn a_private_page_can_fill_seal_read_and_close_without_creating_a_file() {
    fn check<const N: usize>() {
        // Fill exactly one private page and read each appended record while observing the actual
        // resource owner. A full page alone does not yet require a successor or disk descriptor.
        let mut index = PagedLayoutIndex::<N>::new().unwrap();
        let observer = index.resource_observer();
        assert!(!observer.snapshot().file_open);
        #[cfg(feature = "paged-graphemes")]
        assert!(!observer.snapshot().owner_closed);
        assert_eq!(observer.snapshot().buffer_bytes, 0);
        for row in 0..PAGE / N {
            index.append(record::<N>(row)).unwrap();
            assert_eq!(index.read(row).unwrap(), record::<N>(row));
            assert!(!observer.snapshot().file_open);
        }
        // Sealing this full private page keeps the original buffer readable without eagerly
        // writing it or allocating a file that no reader handoff has requested.
        let identity = buffer_identity(&index);
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), record::<N>(0));
        assert_eq!(index.read(PAGE / N - 1).unwrap(), record::<N>(PAGE / N - 1));
        assert_eq!(buffer_identity(&index), identity);
        assert!(!observer.snapshot().file_open);
        index.close().unwrap();
        // After close, an absent buffer no longer means lazily unallocated. The same poison
        // gate must prevent old-record reads, appends, or sealing from recreating any resource.
        assert!(matches!(index.read(0), Err(Error::Poisoned)));
        assert!(matches!(index.append(record::<N>(0)), Err(Error::Poisoned)));
        assert!(matches!(index.seal(), Err(Error::Poisoned)));
        let released = observer.snapshot();
        assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
        #[cfg(feature = "paged-graphemes")]
        assert!(released.owner_closed);
        assert_eq!(released.peak_buffer_bytes, PAGE);
    }
    // Record widths exercise distinct page capacities through the same production owner.
    check::<16>();
    check::<32>();
    check::<64>();
}

#[test]
fn first_rollover_allocates_one_file_and_reuses_the_existing_buffer_and_fd() {
    use std::os::fd::AsRawFd;

    // Only the first record beyond the private page forces a real file and flushes that page.
    let mut index = PagedLayoutIndex::<64>::new().unwrap();
    let observer = index.resource_observer();
    for row in 0..PAGE / 64 {
        index.append(record::<64>(row)).unwrap();
    }
    let identity = buffer_identity(&index);
    assert!(!observer.snapshot().file_open);
    index.append(record::<64>(PAGE / 64)).unwrap();
    let descriptor = index.file.as_ref().unwrap().as_raw_fd();
    assert!(observer.snapshot().file_open);
    assert_eq!(
        index.file.as_ref().unwrap().metadata().unwrap().len(),
        PAGE as u64
    );
    assert_eq!(index.read(0).unwrap(), record::<64>(0));
    // Later rollovers keep the original descriptor and buffer while preserving old-page reads;
    // resource observations below remain implementation-specific, separate from returned bytes.
    for row in PAGE / 64 + 1..PAGE / 64 * 3 + 1 {
        index.append(record::<64>(row)).unwrap();
        assert_eq!(index.file.as_ref().unwrap().as_raw_fd(), descriptor);
        assert_eq!(buffer_identity(&index), identity);
    }
    index.close().unwrap();
    let released = observer.snapshot();
    assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
}

#[test]
fn actual_file_creation_failure_is_deferred_to_the_first_required_io_and_never_retried() {
    const CHILD: &str = "OMNI_LAYOUT_INDEX_CREATE_FAILURE_CHILD";
    if let Some(mode) = std::env::var_os(CHILD) {
        // Only this child receives a nonexistent TMPDIR, causing actual tempfile ENOENT without
        // changing the environment of tests running concurrently in the parent process.
        let mut index = PagedLayoutIndex::<64>::new().unwrap();
        let observer = index.resource_observer();
        index.append(record::<64>(0)).unwrap();
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), record::<64>(0));
        assert!(!observer.snapshot().file_open);
        // The private seal above must not create a file. Each mode below independently exercises
        // its production entry's first required I/O under this real filesystem failure.
        match mode.to_str().unwrap() {
            "rollover" => {
                // Fill a private page, then reject its first actual spill. Poison remains sticky
                // rather than retrying file creation through a later read or append.
                let mut index = PagedLayoutIndex::<64>::new().unwrap();
                let observer = index.resource_observer();
                for row in 0..PAGE / 64 {
                    index.append(record::<64>(row)).unwrap();
                }
                assert!(
                    matches!(index.append(record::<64>(PAGE / 64)), Err(Error::Io(error))
                    if error.kind() == io::ErrorKind::NotFound)
                );
                assert!(matches!(index.read(0), Err(Error::Poisoned)));
                assert!(matches!(
                    index.append(record::<64>(0)),
                    Err(Error::Poisoned)
                ));
                index.close().unwrap();
                let released = observer.snapshot();
                assert!(
                    !released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed
                );
            }
            #[cfg(feature = "paged-line-compose")]
            "reader" => {
                // Final handoff needs an actual file even for private records; failed creation
                // returns the consumed writer's resources without delivering another owner.
                assert!(matches!(index.into_reader(), Err(Error::Io(error))
                    if error.kind() == io::ErrorKind::NotFound));
                let released = observer.snapshot();
                assert!(
                    !released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed
                );
            }
            #[cfg(feature = "paged-shaping-frontier")]
            "snapshot" => {
                // Prefix publication starts with append authority. Actual creation failure must
                // return the reserved reader slot and revoke all later writer/publication access.
                let mut writer = PagedLayoutIndex::<64>::new().unwrap();
                writer.append(record::<64>(0)).unwrap();
                assert!(matches!(writer.snapshot_reader(1),
                    Err(LayoutIndexSnapshotError::Index(Error::Io(error)))
                    if error.kind() == io::ErrorKind::NotFound));
                assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
                assert!(matches!(writer.read(0), Err(Error::Poisoned)));
                assert!(matches!(
                    writer.snapshot_reader(1),
                    Err(LayoutIndexSnapshotError::Index(Error::Poisoned))
                ));
                let observer = writer.resource_observer();
                writer.close().unwrap();
                let released = observer.snapshot();
                assert!(
                    !released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed
                );
            }
            other => panic!("未知的真实创建失败入口：{other}"),
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-parent");
    // Reinvoke only this exact test in a joined child per entry. The parent's private temporary
    // directory owns the absent path; no shared environment or unrelated files are changed.
    let test = format!(
        "{}::actual_file_creation_failure_is_deferred_to_the_first_required_io_and_never_retried",
        module_path!().split_once("::").unwrap().1
    );
    for mode in [
        "rollover",
        #[cfg(feature = "paged-line-compose")]
        "reader",
        #[cfg(feature = "paged-shaping-frontier")]
        "snapshot",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(CHILD, mode)
            .env("TMPDIR", &missing)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "真实创建失败入口 {mode} 未通过：{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[test]
fn repeated_old_and_new_binary_searches_preserve_the_single_dirty_append_buffer() {
    fn check<const N: usize>() {
        // Keep the current page dirty while binary search visits both flushed and newly appended
        // records. The pre-existing private counters below inspect buffer reuse, not product RSS.
        let per_page = PAGE / N;
        let initial = per_page * 2 + 17;
        let mut index = PagedLayoutIndex::<N>::new().unwrap();
        let observer = index.resource_observer();
        for row in 0..initial {
            index.append(record::<N>(row)).unwrap();
        }
        let identity = buffer_identity(&index);
        let page_number = index.number;
        assert_eq!(observer.snapshot().peak_buffer_bytes, PAGE);
        assert_eq!(index.loads, 3);

        for round in 0..32 {
            let row = initial + round;
            index.append(record::<N>(row)).unwrap();
            for target in [0, per_page - 1, per_page, initial - 1, row] {
                binary_search(&mut index, target);
            }
            let page = index.page.as_ref().unwrap();
            assert_eq!(buffer_identity(&index), identity);
            assert_eq!(index.number, page_number);
            assert!(page.used > page.flushed);
            assert_eq!(index.read(row).unwrap(), record::<N>(row));
            assert_eq!(observer.snapshot().peak_buffer_bytes, PAGE);
            assert_eq!(index.loads, 3);
            assert_eq!(observer.snapshot().mapped_bytes, 0);
            assert_eq!(observer.snapshot().buffer_bytes, PAGE);
        }

        // Private sealing revokes append authority only. The original buffer serves this page;
        // old-page lookups still read exact records from the existing file.
        index.seal().unwrap();
        assert_eq!(buffer_identity(&index), identity);
        for target in [0, per_page, initial - 1, initial + 31] {
            binary_search(&mut index, target);
        }
        assert_eq!(buffer_identity(&index), identity);
        assert_eq!(observer.snapshot().mapped_bytes, 0);
        index.close().unwrap();
        let closed = observer.snapshot();
        assert!(
            !closed.file_open
                && closed.mapped_bytes == 0
                && closed.buffer_bytes == 0
                && !closed.cleanup_failed
        );
    }
    check::<16>();
    check::<32>();
}

#[test]
fn private_seal_and_close_do_not_write_an_unpublished_tail() {
    // A real read-only file detects any attempted write. Since there is no file consumer,
    // private seal/close must discard the unpublished buffer without altering existing bytes.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unpublished-index");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut index = PagedLayoutIndex::<32>::with_file(File::open(&path).unwrap()).unwrap();
    let observer = index.resource_observer();
    index.append(record::<32>(0)).unwrap();
    index.seal().unwrap(); // No file consumer exists, so sealing must not write through the read-only descriptor.
    assert_eq!(index.read(0).unwrap(), record::<32>(0));
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    index.close().unwrap();
    let closed = observer.snapshot();
    assert!(!closed.file_open && closed.buffer_bytes == 0 && !closed.cleanup_failed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[test]
fn actual_short_read_preserves_partial_bytes_and_poisons_the_writer_without_mapping() {
    // Force an old-page pread to return a real prefix followed by EOF. The helper's raw output
    // may contain that prefix, but the error must keep the complete record unavailable.
    let mut index = PagedLayoutIndex::<32>::new().unwrap();
    for row in 0..PAGE / 32 + 1 {
        index.append(record::<32>(row)).unwrap();
    }
    index.seal().unwrap(); // The first page was actually written; only old-page pread sees the truncation, not the buffered tail.
    let observer = index.resource_observer();
    let file = index.file.as_ref().unwrap();
    file.set_len(48).unwrap();
    let mut output = [0xab; 32];
    let error = read_exact_at(&mut output, 32, |output, position| {
        file.read_at(output, position)
    })
    .unwrap_err();
    assert!(matches!(error, Error::Io(cause) if cause.kind() == io::ErrorKind::UnexpectedEof));
    assert_eq!(&output[..16], &record::<32>(1)[..16]);
    assert_eq!(&output[16..], &[0xab; 16]);
    // The same failure through the owner poisons future reads while retaining its buffer until
    // explicit close; successful descriptor release is independent of the failed data operation.
    assert!(
        matches!(index.read(1), Err(Error::Io(cause)) if cause.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    index.close().unwrap(); // Data failure does not fabricate a kernel close error; all actual resources must still return.
    let closed = observer.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && closed.buffer_bytes == 0
            && !closed.cleanup_failed
    );
}

#[test]
fn actual_directory_read_errno_poisons_before_any_mapping_and_preserves_close_receipt() {
    // The existing test-only file constructor receives a real directory descriptor. The count
    // mutation below admits the attempted read; EISDIR itself comes from the actual filesystem.
    let directory = tempfile::tempdir().unwrap();
    let file = File::open(directory.path()).unwrap();
    let mut index = PagedLayoutIndex::<16>::with_file(file).unwrap();
    let observer = index.resource_observer();
    index.count = 1; // Test-only range admission for this unreadable descriptor; production creates its own anonymous file.
    let error = index.read(0).unwrap_err();
    assert!(matches!(error, Error::Io(cause) if cause.raw_os_error() == Some(libc::EISDIR)));
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    assert_eq!(observer.snapshot().peak_buffer_bytes, 0);
    index.close().unwrap();
    let closed = observer.snapshot();
    assert!(
        !closed.file_open
            && closed.mapped_bytes == 0
            && closed.buffer_bytes == 0
            && !closed.cleanup_failed
    );
}

#[test]
fn finite_pread_retries_eintr_and_short_reads_only_at_the_original_remaining_coordinates() {
    // This existing injected syscall sequence checks the production exact-read helper's offset
    // arithmetic. It does not claim that the operating system actually delivered EINTR here.
    let mut output = [0xab; 8];
    let mut positions = [0; 4];
    let mut lengths = [0; 4];
    let mut calls = 0;
    read_exact_at(&mut output, 37, |remaining, position| {
        positions[calls] = position;
        lengths[calls] = remaining.len();
        calls += 1;
        match calls {
            1 | 3 => Err(io::Error::from_raw_os_error(libc::EINTR)),
            2 => {
                // Three returned bytes advance the next original offset; an interrupt consumes none.
                remaining[..3].copy_from_slice(&[10, 11, 12]);
                Ok(3)
            }
            4 => {
                // Complete only the five remaining bytes, without replaying the already written prefix.
                remaining.copy_from_slice(&[13, 14, 15, 16, 17]);
                Ok(5)
            }
            _ => panic!("有限区间已完成后不得继续读取"),
        }
    })
    .unwrap();
    assert_eq!(calls, 4);
    assert_eq!(positions, [37, 37, 40, 40]);
    assert_eq!(lengths, [8, 8, 5, 5]);
    assert_eq!(output, [10, 11, 12, 13, 14, 15, 16, 17]);
}

#[test]
fn finite_pread_returns_the_first_eio_after_partial_output_without_replay() {
    // The retained injected partial-read/EIO sequence separates unchanged prefix bytes from the
    // failed remainder. No retry may turn this first hard error into a successful complete record.
    let mut output = [0xab; 8];
    let mut calls = 0;
    let error = read_exact_at(&mut output, 11, |remaining, position| {
        calls += 1;
        if calls == 1 {
            assert_eq!(position, 11);
            remaining[..3].copy_from_slice(&[4, 5, 6]);
            Ok(3)
        } else {
            assert_eq!((calls, position, remaining.len()), (2, 14, 5));
            Err(io::Error::from_raw_os_error(libc::EIO))
        }
    })
    .unwrap_err();
    assert!(matches!(error, Error::Io(cause) if cause.raw_os_error() == Some(libc::EIO)));
    assert_eq!(calls, 2);
    assert_eq!(output, [4, 5, 6, 0xab, 0xab, 0xab, 0xab, 0xab]);
}

#[test]
fn cancellation_range_and_offset_overflow_keep_the_original_poison_gate() {
    // This direct failure-gate mutation inspects the existing owner's implementation contract;
    // it is not an observation of a real cancellation callback arriving from another subsystem.
    let mut index = PagedLayoutIndex::<16>::new().unwrap();
    for row in 0..PAGE / 16 + 1 {
        index.append(record::<16>(row)).unwrap();
    }
    let observer = index.resource_observer();
    let identity = buffer_identity(&index);
    index.mark_failed(); // The original failure gate precedes same-page and cross-page I/O; old records gain no renewed read authority.
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    assert!(matches!(index.read(PAGE / 16), Err(Error::Poisoned)));
    assert_eq!(buffer_identity(&index), identity);
    index.close().unwrap();
    assert_eq!(observer.snapshot().buffer_bytes, 0);
    assert!(!observer.snapshot().file_open && observer.snapshot().mapped_bytes == 0);

    // Reject a nonexistent record before allocating storage, then keep that same owner poisoned.
    let mut range = PagedLayoutIndex::<16>::new().unwrap();
    assert!(matches!(range.read(0), Err(Error::InvalidRecord)));
    assert!(matches!(range.read(0), Err(Error::Poisoned)));
    assert_eq!(range.resource_observer().snapshot().peak_buffer_bytes, 0);
    range.close().unwrap();

    // The existing corrupted-count fixture exposes checked byte-offset overflow before I/O;
    // no actual file offset or descriptor is manufactured to mimic successful access.
    let mut overflow = PagedLayoutIndex::<16>::new().unwrap();
    let record = usize::MAX / 16 + 1;
    overflow.count = record + 1; // Test-only corrupted count; checked multiplication must still precede every system call.
    assert!(matches!(overflow.read(record), Err(Error::Overflow)));
    assert!(matches!(overflow.read(0), Err(Error::Poisoned)));
    assert_eq!(overflow.resource_observer().snapshot().peak_buffer_bytes, 0);
    overflow.close().unwrap();
}

#[test]
fn finite_pread_rejects_oversized_or_overflowing_intervals_before_io() {
    // Panic closures are the existing no-I/O witnesses for invalid helper arguments. These pure
    // interval checks neither perform real filesystem calls nor establish external cleanup behavior.
    let mut oversized = [0; PAGE + 1];
    assert!(matches!(
        read_exact_at(&mut oversized, 0, |_, _| panic!("超预算不得进入IO")),
        Err(Error::InvalidRecord)
    ));
    let mut record = [0; 16];
    assert!(matches!(
        read_exact_at(&mut record, u64::MAX - 7, |_, _| panic!("溢出不得进入IO")),
        Err(Error::Overflow)
    ));
}

#[cfg(feature = "paged-shaping-frontier")]
#[test]
fn half_page_publication_writes_only_each_new_suffix_and_keeps_one_buffer() {
    // Publish a prefix, then retain both readers while adding records to the same writer page.
    // Independent file length and byte witnesses distinguish suffix writes from full-page rewrites.
    let mut index = PagedLayoutIndex::<32>::new().unwrap();
    for row in 0..4 {
        index.append(record::<32>(row)).unwrap();
    }
    let observer = index.resource_observer();
    let identity = buffer_identity(&index);
    let prefix = index.snapshot_reader(2).unwrap();
    assert_eq!(index.file.as_ref().unwrap().metadata().unwrap().len(), 64);
    assert_eq!(index.page.as_ref().unwrap().flushed, 64);
    assert_eq!(prefix.read(1).unwrap(), record::<32>(1));
    // This test writes a witness directly into the published region. The next publication must
    // write only 64..160 and leave it intact; this is not a product entry permitting file mutation.
    index
        .file
        .as_ref()
        .unwrap()
        .write_all_at(&[0x7d], 0)
        .unwrap();
    index.append(record::<32>(4)).unwrap();
    let second = index.snapshot_reader(5).unwrap();
    assert_eq!(index.file.as_ref().unwrap().metadata().unwrap().len(), 160);
    assert_eq!(index.page.as_ref().unwrap().flushed, 160);
    let mut witness = [0];
    index
        .file
        .as_ref()
        .unwrap()
        .read_at(&mut witness, 0)
        .unwrap();
    assert_eq!(witness, [0x7d]);
    assert_eq!(second.read(4).unwrap(), record::<32>(4));
    assert_eq!(buffer_identity(&index), identity);
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    assert_eq!(observer.snapshot().mapped_bytes, 0);
    // The old reader's logical EOF remains fixed despite new bytes in the same inode. Its invalid
    // request revokes itself; the independent successor reader and writer still close normally.
    assert!(matches!(prefix.read(2), Err(Error::InvalidRecord)));
    assert!(matches!(prefix.close(), Err(Error::Poisoned)));
    second.close().unwrap();
    index.close().unwrap();
    let closed = observer.snapshot();
    assert!(
        !closed.file_open
            && closed.buffer_bytes == 0
            && closed.mapped_bytes == 0
            && closed.buffer_bytes == 0
            && !closed.cleanup_failed
    );
}
