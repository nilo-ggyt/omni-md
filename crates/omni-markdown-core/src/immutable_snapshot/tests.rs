//! Snapshot safety, fallback cancellation, and process-isolated external mutation regressions.

use super::*;
use std::fs;

/// An empty snapshot owns no temporary namespace, mapping, or retained descriptor.
#[test]
fn empty_source_needs_no_scratch_storage() {
    // A caller-provided scratch parent makes any accidental temporary acquisition observable.
    let directory = tempfile::tempdir().expect("empty snapshot fixture");
    let source = tempfile::tempfile().expect("empty source");
    let snapshot = snapshot_file_inner(
        &source,
        0,
        &|| {
            // This isolated synchronous fixture has no foreground cancellation owner.
            true
        },
        true,
        Some(directory.path()),
    )
    .expect("empty snapshot");
    assert!(snapshot.is_none());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

/// 新建与截成零长的独占输出都不建立映射，已写长度来自真实文件操作。
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn canonical_output_empty_and_truncated_empty_freeze_without_mapping() {
    let empty = CanonicalSnapshotOutput::new().unwrap();
    assert_eq!(empty.len(), 0);
    assert!(empty.freeze().unwrap().is_none());

    // 清洗结果可在最后一个非空白位置之前全部被截去，不能留下旧 extent 或零长 mmap。
    let mut output = CanonicalSnapshotOutput::new().unwrap();
    output.write_all(" \t\r\n".as_bytes()).unwrap();
    output.truncate(0).unwrap();
    assert_eq!(output.len(), 0);
    assert_eq!(output.file.metadata().unwrap().len(), 0);
    assert!(output.freeze().unwrap().is_none());
}

/// 多块写入与截尾后的追加保持字节顺序，只有最后映射 owner Drop 才报告真实释放。
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn canonical_output_chunks_truncate_and_freeze_retain_exact_bytes_until_last_drop() {
    use std::sync::Arc;

    // 第一块恰为 4096 字节且末尾是完整中文标量，第二块从 emoji 开始。
    let first = format!("{}中", "a".repeat(4093));
    let second = "😀文档\\|管道\r\n跨块";
    let mut output = CanonicalSnapshotOutput::new().unwrap();
    output.write_all(first.as_bytes()).unwrap();
    output.write_all(second.as_bytes()).unwrap();
    let kept = output.len();
    output.write_all("应丢弃的尾部 \t\r\n".as_bytes()).unwrap();
    output.truncate(kept).unwrap();
    output.write_all("终点".as_bytes()).unwrap();
    let expected = format!("{first}{second}终点");
    let metadata = output.file.metadata().unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.nlink(), 0);
    assert_eq!(metadata.len(), expected.len() as u64);
    assert_eq!(output.len(), expected.len());

    // 封存直接消费同一独占文件；有限 lease 结束并不意味着映射本身已被释放。
    let backing = Arc::new(output.freeze().unwrap().unwrap());
    let observer = backing.observer();
    for start in (0..expected.len()).step_by(crate::original_backing::ORIGINAL_READ_BYTES) {
        // 逐个实际窗口比对，包括跨写入块的窗口；每次都显式处理归还 pin 的真实结果。
        let end = (start + crate::original_backing::ORIGINAL_READ_BYTES).min(expected.len());
        let lease = backing.lease(start..end).unwrap();
        assert_eq!(&*lease, &expected.as_bytes()[start..end]);
        lease.finish().unwrap();
    }
    assert!(!observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), expected.len());
    let retained = Arc::clone(&backing);
    drop(backing);
    assert!(!observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), expected.len());

    // 最后 owner 确实执行 munmap，观察者只保留固定事实，不保活正文或文件。
    drop(retained);
    assert!(observer.unmap_succeeded());
    assert_eq!(observer.mapped_bytes(), 0);
    assert!(!observer.failed());
}

/// 截尾不能扩展正文；封存也拒绝与完成写入计量不符的真实文件 extent。
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn canonical_output_rejects_growth_and_unaccounted_extent() {
    let mut output = CanonicalSnapshotOutput::new().unwrap();
    output.write_all("原文".as_bytes()).unwrap();
    let bytes = output.len();
    assert!(matches!(
        output.truncate(bytes + 1),
        Err(SnapshotError::Storage(error)) if error.kind() == io::ErrorKind::InvalidInput
    ));
    assert_eq!(output.len(), bytes);
    assert_eq!(output.file.metadata().unwrap().len(), bytes as u64);

    // 测试在工厂内部直接改变实际 extent；产品 API 没有取得或克隆此写句柄的入口。
    output.file.set_len(bytes as u64 + 1).unwrap();
    assert!(matches!(output.freeze(), Err(SnapshotError::SourceChanged)));
}

/// 真实只读descriptor上的write拒绝后，候选Drop确实关闭原FD；不以空字段或claim计数代替。
#[cfg(feature = "native-markdown-candidate")]
#[test]
fn failed_output_write_releases_its_actual_descriptor() {
    use std::os::fd::AsRawFd;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unchanged");
    fs::write(&path, b"unchanged").unwrap();
    // 只在工厂内部测试接管实际只读FD；产品没有File入口，业务层失败另由真实RLIMIT验证。
    let mut output = CanonicalSnapshotOutput {
        file: File::open(&path).unwrap(),
        bytes: 0,
    };
    let descriptor = output.file.as_raw_fd();
    assert!(
        matches!(output.write_all(b"candidate"), Err(SnapshotError::Storage(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    drop(output);
    // SAFETY: F_GETFD只查询刚归还的整数descriptor，不解引用指针或接管任何资源。
    assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    assert_eq!(fs::read(path).unwrap(), b"unchanged");
}

/// Streaming fallback freezes exact bytes without retaining any public scratch names.
#[test]
fn forced_streaming_snapshot_survives_truncation_and_non_utf8_source_writes() {
    // Force the unsupported-filesystem path independently from the machine's APFS capabilities.
    let directory = tempfile::tempdir().expect("streamed snapshot fixture");
    let source_path = directory.path().join("Source.md");
    let content = "immutable 中文😀\r\n".repeat(12_000);
    fs::write(&source_path, &content).unwrap();
    let source = File::open(&source_path).unwrap();
    let scratch = directory.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let snapshot = snapshot_file_inner(
        &source,
        content.len() as u64,
        &|| {
            // This isolated synchronous fixture has no foreground cancellation owner.
            true
        },
        false,
        Some(&scratch),
    )
    .unwrap()
    .unwrap();
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);

    // Mutate the original inode both to zero length and to invalid UTF-8 after the snapshot exists.
    fs::write(&source_path, []).unwrap();
    assert_eq!(&snapshot[..], content.as_bytes());
    fs::write(&source_path, vec![0xff; content.len()]).unwrap();
    assert_eq!(std::str::from_utf8(&snapshot).unwrap(), content);
    assert_eq!(source.metadata().unwrap().len(), content.len() as u64);
}

/// Fallback copying observes cancellation between fixed-size chunks and leaves no named partial file.
#[test]
fn streaming_cancellation_and_extent_changes_release_private_scratch() {
    use std::cell::Cell;

    // A deterministic predicate closes the owner after one copy chunk without timing-dependent threads.
    let directory = tempfile::tempdir().expect("cancelled snapshot fixture");
    let source_path = directory.path().join("Source.md");
    fs::write(&source_path, vec![b'x'; SNAPSHOT_COPY_BYTES * 3]).unwrap();
    let source = File::open(&source_path).unwrap();
    let scratch = directory.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let checks = Cell::new(0);
    let current = || {
        // Outer acquisition and capture validation plus one copy interval are allowed; the next interval cancels.
        let count = checks.get() + 1;
        checks.set(count);
        count <= 3
    };
    assert!(matches!(
        snapshot_file_inner(
            &source,
            source.metadata().unwrap().len(),
            &current,
            false,
            Some(&scratch)
        ),
        Err(SnapshotError::Cancelled)
    ));
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);

    // A stale larger source extent must fail safely at EOF and release the same private namespace.
    assert!(matches!(
        snapshot_file_inner(
            &source,
            source.metadata().unwrap().len() + 1,
            &|| {
                // This isolated synchronous fixture has no foreground cancellation owner.
                true
            },
            false,
            Some(&scratch)
        ),
        Err(SnapshotError::SourceChanged)
    ));
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
}

/// Verify the APFS fast path creates an independent unnamed inode, rather than a hard link or shared source handle.
#[cfg(target_os = "macos")]
#[test]
fn available_clone_has_independent_identity_and_no_filesystem_name() {
    use std::os::unix::fs::MetadataExt;

    // Keep the source and clone scratch on one temporary volume so supported APFS cloning is eligible.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Source.md");
    fs::write(&path, b"original").unwrap();
    let source = File::open(&path).unwrap();
    let scratch = SnapshotDirectory::in_parent(Some(directory.path())).unwrap();
    let Some(cloned) = clone_unlinked_source(&source, &scratch).unwrap() else {
        // Filesystems without cloning exercise the forced-streaming regression instead; no fast-path claim is made.
        return;
    };
    assert_ne!(
        source.metadata().unwrap().ino(),
        cloned.metadata().unwrap().ino()
    );
    assert_eq!(cloned.metadata().unwrap().nlink(), 0);

    // Truncating the original cannot change the distinct copy-on-write inode's extent or stored bytes.
    fs::write(&path, []).unwrap();
    let mut bytes = [0_u8; 8];
    assert_eq!(cloned.read_at(&mut bytes, 0).unwrap(), bytes.len());
    assert_eq!(&bytes, b"original");
}

/// Measure snapshot acquisition separately from indexing, parser work, and filesystem durability.
#[test]
#[ignore = "manual release-profile immutable snapshot benchmark"]
fn immutable_snapshot_acquisition_profile() {
    use std::hint::black_box;
    use std::time::Instant;

    // Use one prewritten MiB and fixed sample counts; this measures warm local storage rather than cold disk latency.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Source.md");
    let bytes = vec![b'x'; 1_024 * 1_024];
    fs::write(&path, &bytes).unwrap();
    let source = File::open(&path).unwrap();
    let scratch = SnapshotDirectory::in_parent(Some(directory.path())).unwrap();
    let clone_supported = clone_unlinked_source(&source, &scratch).unwrap().is_some();
    println!("immutable_snapshot_1_mib same_volume_clone_supported={clone_supported}");
    for (label, allow_clone) in [("clone_preferred", true), ("forced_streaming", false)] {
        // Warm each acquisition strategy independently before retaining per-call observations.
        for _ in 0..5 {
            // Consume and drop complete mappings so each timed sample repeats real acquisition and release.
            black_box(
                snapshot_file_inner(
                    &source,
                    bytes.len() as u64,
                    &|| {
                        // This isolated synchronous fixture has no foreground cancellation owner.
                        true
                    },
                    allow_clone,
                    None,
                )
                .unwrap(),
            );
        }
        let mut samples = Vec::with_capacity(50);
        for _ in 0..50 {
            // Keep the mapping alive through the observed endpoint; its subsequent Drop is outside acquisition latency.
            let started = Instant::now();
            let mapping = snapshot_file_inner(
                &source,
                bytes.len() as u64,
                &|| {
                    // This isolated synchronous fixture has no foreground cancellation owner.
                    true
                },
                allow_clone,
                None,
            )
            .unwrap();
            samples.push(started.elapsed());
            black_box(mapping);
        }
        samples.sort_unstable();
        println!(
            "immutable_snapshot_1_mib strategy={label} samples=50 p50_ms={:.3} p95_ms={:.3} p99_ms={:.3}",
            samples[24].as_secs_f64() * 1_000.0,
            samples[47].as_secs_f64() * 1_000.0,
            samples[49].as_secs_f64() * 1_000.0
        );
    }
}
