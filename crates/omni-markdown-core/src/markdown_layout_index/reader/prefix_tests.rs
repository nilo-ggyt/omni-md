//! 同匿名inode持续追加时的实际前缀权限、FD配额与错误归还；不复制布局record到常驻缓存。

use super::*;
use crate::markdown_layout_index::LayoutIndexSnapshotError;
use std::{
    os::unix::fs::MetadataExt,
    sync::{Arc, Barrier, atomic::Ordering},
};

#[test]
fn immutable_prefix_has_a_distinct_fd_and_hard_logical_end_while_writer_continues() {
    let mut writer = index::<64>(2);
    assert!(!writer.resource_observer().snapshot().file_open);
    let mut first = writer.snapshot_reader(1).unwrap(); // 首次发布才创建原FD并flush已提交后缀。
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
    let mut second = writer.snapshot_reader(2).unwrap();
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 2);
    assert!(matches!(
        writer.snapshot_reader(2),
        Err(LayoutIndexSnapshotError::ReadersBusy)
    ));
    writer.append(record::<64>(2)).unwrap(); // quota只约束交付，不得毒化原producer或阻止写新记录。
    assert_eq!(first.read(0).unwrap(), record::<64>(0));
    assert_eq!(second.read(1).unwrap(), record::<64>(1));
    assert!(matches!(first.read(1), Err(Error::InvalidRecord))); // 同inode已有该row也不能越旧snapshot逻辑EOF。
    assert!(matches!(first.close(), Err(Error::Poisoned)));
    assert!(!first_observer.snapshot().file_open);
    assert!(!first_observer.snapshot().cleanup_failed);
    writer.append(record::<64>(3)).unwrap(); // 无效Reader请求只撤自己，不能让旧回调毁掉后继worker。
    let mut third = writer.snapshot_reader(4).unwrap();
    assert_eq!(third.read(3).unwrap(), record::<64>(3));
    second.close().unwrap();
    third.close().unwrap();
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
    writer.close().unwrap();
}

#[test]
fn snapshot_reads_on_another_thread_while_the_original_writer_appends_across_pages() {
    let mut writer = index::<64>(3);
    let mut reader = writer.snapshot_reader(3).unwrap();
    let observer = reader.resource_observer();
    let ready = Arc::new(Barrier::new(2));
    let other = Arc::clone(&ready);
    let thread = std::thread::spawn(move || {
        other.wait();
        for round in 0..256 {
            let row = round % 3;
            assert_eq!(reader.read(row).unwrap(), record::<64>(row));
        } // 固定读次数并发检查旧行，无sleep、成功轮询或共享可写页。
        reader.close().unwrap();
    });
    ready.wait();
    for row in 3..1_027 {
        writer.append(record::<64>(row)).unwrap();
    }
    thread.join().unwrap();
    assert!(!observer.snapshot().file_open && !observer.snapshot().cleanup_failed);
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
    let mut last = writer.snapshot_reader(1_027).unwrap();
    assert_eq!(last.read(1_026).unwrap(), record::<64>(1_026));
    last.close().unwrap();
    writer.close().unwrap();
}

#[test]
fn normal_writer_drop_keeps_existing_snapshot_readable_until_its_own_actual_close() {
    let mut writer = index::<64>(2);
    let original = writer.resource_observer();
    let mut reader = writer.snapshot_reader(1).unwrap();
    let snapshot = reader.resource_observer();
    writer.append(record::<64>(2)).unwrap();
    assert_eq!(writer.file.as_ref().unwrap().metadata().unwrap().len(), 64);
    drop(writer); // 原count已有3条，但只发布1条；未发布的2条直接随buffer归还。
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
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly-prefix");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut writer = PagedLayoutIndex::<64>::with_file(File::open(&path).unwrap()).unwrap();
    writer.append(record::<64>(0)).unwrap();
    let original = writer.resource_observer();
    assert!(matches!(writer.snapshot_reader(1),
        Err(LayoutIndexSnapshotError::Index(Error::Io(error))) if error.raw_os_error() == Some(libc::EBADF)));
    // 真实write失败发生在dup之前；不能泄漏尚未交付的Reader名额，也不能给旧buffer重签权限。
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
    let mut writer = index::<64>(2);
    let mut reader = writer.snapshot_reader(2).unwrap();
    reader.file.as_ref().unwrap().set_len(96).unwrap(); // 测试截短同inode；writer原buffer不能掩盖Reader真实pread失败。
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
    writer.close().unwrap(); // 数据错误与真实FD清理分别观察；本次close没有内核错误。
    assert_eq!(writer.resources.prefix_readers.load(Ordering::Acquire), 0);
}

#[test]
fn final_same_fd_handoff_and_prior_prefix_share_real_io_failure_without_copying_records() {
    let mut writer = index::<64>(2);
    let mut prefix = writer.snapshot_reader(1).unwrap();
    let prefix_observer = prefix.resource_observer();
    let mut final_reader = writer.into_reader().unwrap();
    final_reader.file.as_ref().unwrap().set_len(0).unwrap();
    assert!(matches!(final_reader.read(0), Err(Error::Io(_))));
    assert!(matches!(prefix.read(0), Err(Error::Poisoned)));
    assert!(matches!(final_reader.close(), Err(Error::Poisoned)));
    assert!(matches!(prefix.close(), Err(Error::Poisoned)));
    assert!(!prefix_observer.snapshot().file_open);
    assert!(!prefix_observer.snapshot().cleanup_failed);
}
