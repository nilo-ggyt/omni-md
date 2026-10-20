//! 原匿名索引异页读取的实际buffer身份、有限IO与撤权回归，不建立替代元数据缓存。

use super::*;

fn record<const N: usize>(row: usize) -> [u8; N] {
    let mut bytes = [0; N];
    bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
    bytes[N - 1] = (row % 251) as u8;
    bytes
}

// 只比较实际Box的地址，确认异页pread及seal没有把原追加buffer换成另一份缓存。
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
    assert_eq!(low, target);
    assert_eq!(index.read(low).unwrap(), record::<N>(target));
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn explicit_record_batches_match_original_rows_across_file_and_unpublished_append_buffer() {
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
    } // 旧页跨界、FD与尚未落盘尾部跨界、纯buffer都读同一真实记录，不为查询先写脏页。
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
    let mut index = PagedLayoutIndex::<64>::new().unwrap();
    for ordinal in 0..PAGE / 64 + 1 {
        index.append(record::<64>(ordinal)).unwrap();
    }
    let observer = index.resource_observer();
    // 同一真实inode截到一条；请求的三条均在旧页，不能用当前追加buffer掩盖磁盘短读。
    index.file.as_ref().unwrap().set_len(64).unwrap();
    let mut rows = [[0; 64]; 3];
    assert!(
        matches!(index.read_batch(0..3, &mut rows), Err(Error::Io(error))
        if error.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    index.close().unwrap();
    let released = observer.snapshot();
    assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn explicit_batch_cannot_truncate_invalid_input_to_a_valid_record_prefix() {
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
    } // 长度不符、越EOF和反向范围均拒绝，不能交付其中看似正确的子范围。
}

#[test]
fn a_private_page_can_fill_seal_read_and_close_without_creating_a_file() {
    fn check<const N: usize>() {
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
        // 恰好装满并不需要后继页；私有seal仍只读原唯一buffer，不提前写出或分配FD。
        let identity = buffer_identity(&index);
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), record::<N>(0));
        assert_eq!(index.read(PAGE / N - 1).unwrap(), record::<N>(PAGE / N - 1));
        assert_eq!(buffer_identity(&index), identity);
        assert!(!observer.snapshot().file_open);
        index.close().unwrap();
        // close后的None不再代表未分配：同一个poison门必须阻止旧buffer和新File被重新创建。
        assert!(matches!(index.read(0), Err(Error::Poisoned)));
        assert!(matches!(index.append(record::<N>(0)), Err(Error::Poisoned)));
        assert!(matches!(index.seal(), Err(Error::Poisoned)));
        let released = observer.snapshot();
        assert!(!released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed);
        #[cfg(feature = "paged-graphemes")]
        assert!(released.owner_closed);
        assert_eq!(released.peak_buffer_bytes, PAGE);
    }
    check::<16>();
    check::<32>();
    check::<64>();
}

#[test]
fn first_rollover_allocates_one_file_and_reuses_the_existing_buffer_and_fd() {
    use std::os::fd::AsRawFd;

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
        // 独立测试进程的TMPDIR指向不存在的目录，制造真实tempfile ENOENT；不修改并行测试的环境。
        let mut index = PagedLayoutIndex::<64>::new().unwrap();
        let observer = index.resource_observer();
        index.append(record::<64>(0)).unwrap();
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), record::<64>(0));
        assert!(!observer.snapshot().file_open);
        // 上方真实私有seal不应尝试创建File；以下各模式用原业务入口单独触发首次IO。
        match mode.to_str().unwrap() {
            "rollover" => {
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
                assert!(matches!(index.into_reader(), Err(Error::Io(error))
                    if error.kind() == io::ErrorKind::NotFound));
                let released = observer.snapshot();
                assert!(
                    !released.file_open && released.buffer_bytes == 0 && !released.cleanup_failed
                );
            }
            #[cfg(feature = "paged-shaping-frontier")]
            "snapshot" => {
                // 新writer保留追加权，真实发布失败须先归还预留名额并撤掉后续全部权限。
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

        // 私有seal只撤追加权限；原buffer继续服务本页，旧页仍按exactrecord读取。
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unpublished-index");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut index = PagedLayoutIndex::<32>::with_file(File::open(&path).unwrap()).unwrap();
    let observer = index.resource_observer();
    index.append(record::<32>(0)).unwrap();
    index.seal().unwrap(); // 该owner没有File消费者，不应触发只读FD上的写入。
    assert_eq!(index.read(0).unwrap(), record::<32>(0));
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    index.close().unwrap();
    let closed = observer.snapshot();
    assert!(!closed.file_open && closed.buffer_bytes == 0 && !closed.cleanup_failed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[test]
fn actual_short_read_preserves_partial_bytes_and_poisons_the_writer_without_mapping() {
    let mut index = PagedLayoutIndex::<32>::new().unwrap();
    for row in 0..PAGE / 32 + 1 {
        index.append(record::<32>(row)).unwrap();
    }
    index.seal().unwrap(); // 第一页已实际write；末页仍在buffer，截短只针对旧页pread。
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
    assert!(
        matches!(index.read(1), Err(Error::Io(cause)) if cause.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    assert_eq!(observer.snapshot().buffer_bytes, PAGE);
    index.close().unwrap(); // 数据失败不伪造成内核close失败；真实资源收据仍须全部归还。
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
    let directory = tempfile::tempdir().unwrap();
    let file = File::open(directory.path()).unwrap();
    let mut index = PagedLayoutIndex::<16>::with_file(file).unwrap();
    let observer = index.resource_observer();
    index.count = 1; // 仅测试构造不可读取的原FD范围；生产构造器只接受自己的匿名文件。
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
                remaining[..3].copy_from_slice(&[10, 11, 12]);
                Ok(3)
            }
            4 => {
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
    let mut index = PagedLayoutIndex::<16>::new().unwrap();
    for row in 0..PAGE / 16 + 1 {
        index.append(record::<16>(row)).unwrap();
    }
    let observer = index.resource_observer();
    let identity = buffer_identity(&index);
    index.mark_failed(); // 原取消/失败门先于同页或异页IO，不给旧record恢复读权。
    assert!(matches!(index.read(0), Err(Error::Poisoned)));
    assert!(matches!(index.read(PAGE / 16), Err(Error::Poisoned)));
    assert_eq!(buffer_identity(&index), identity);
    index.close().unwrap();
    assert_eq!(observer.snapshot().buffer_bytes, 0);
    assert!(!observer.snapshot().file_open && observer.snapshot().mapped_bytes == 0);

    let mut range = PagedLayoutIndex::<16>::new().unwrap();
    assert!(matches!(range.read(0), Err(Error::InvalidRecord)));
    assert!(matches!(range.read(0), Err(Error::Poisoned)));
    assert_eq!(range.resource_observer().snapshot().peak_buffer_bytes, 0);
    range.close().unwrap();

    let mut overflow = PagedLayoutIndex::<16>::new().unwrap();
    let record = usize::MAX / 16 + 1;
    overflow.count = record + 1; // 只构造损坏计数，验证旧checked乘法仍先于任何系统调用。
    assert!(matches!(overflow.read(record), Err(Error::Overflow)));
    assert!(matches!(overflow.read(0), Err(Error::Poisoned)));
    assert_eq!(overflow.resource_observer().snapshot().peak_buffer_bytes, 0);
    overflow.close().unwrap();
}

#[test]
fn finite_pread_rejects_oversized_or_overflowing_intervals_before_io() {
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
    // 测试直接在已发布区留下外部见证；第二次发布只写64..160，不得重写旧字节。
    // 这不是产品允许修改File的入口；用于精确区分dirty suffix与全页回写协议。
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
