//! 真实VM/FD成功与失败收据；故障注入只改本测试的syscall参数，不伪造成功或泄漏测试资源。
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
        // 只对必然无效的-1做真实失败调用；不重复close真实FD，避免并发复用误关他人资源。
        close_descriptor(-1)
    });
    assert!(result.is_err());
    assert_eq!(facts.first_errno(), Some(libc::EBADF));
    assert!(!facts.backing_closed.load(Ordering::Acquire));
    assert!(facts.completed.load(Ordering::Acquire));
    assert!(facts.unmapped.load(Ordering::Acquire)); // 已取得的VM实际补偿归还，但不抹掉原close失败。
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), 0);
    // 注入没有实际关闭原fd；测试单独归还它，不把这一恢复写成生产owner的成功收据。
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
        // SAFETY: 故意传非页对齐地址，公开munmap合同返回EINVAL；不解除有效映射，也不借出该指针。
        unsafe { libc::munmap(address.add(1).cast(), length) }
    });
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EINVAL));
    assert_eq!(facts.first_errno(), Some(libc::EINVAL));
    assert!(facts.completed.load(Ordering::Acquire));
    assert!(!facts.unmapped.load(Ordering::Acquire));
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), length);
    drop(mapping); // 已尝试的owner不能重试地址；原负面事实永久保留。
    assert!(!facts.unmapped.load(Ordering::Acquire));
    // SAFETY: 本隔离测试仍独占知道的真实映射且没有借用；显式补偿测试注入，避免污染后续进程。
    assert_eq!(unsafe { libc::munmap(address.as_ptr().cast(), length) }, 0);
    assert_eq!(facts.mapped_bytes.load(Ordering::Acquire), length); // 不把外部恢复篡改成owner成功。
    assert_eq!(facts.first_errno(), Some(libc::EINVAL));
}
