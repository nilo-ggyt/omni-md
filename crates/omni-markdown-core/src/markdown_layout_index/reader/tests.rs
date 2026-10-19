use super::*;
use crate::markdown_layout_index::PagedLayoutIndex;
use std::os::fd::AsRawFd;
#[cfg(feature = "paged-shaping-frontier")]
#[path = "prefix_tests.rs"]
mod prefix_tests;

fn record<const N: usize>(row: usize) -> [u8; N] {
    let mut bytes = [0; N];
    bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
    bytes[N - 1] = (row % 251) as u8;
    bytes
}

fn index<const N: usize>(count: usize) -> PagedLayoutIndex<N> {
    let mut index = PagedLayoutIndex::new().unwrap();
    for row in 0..count {
        index.append(record(row)).unwrap();
    }
    index
}

#[test]
fn empty_final_handoff_creates_one_real_file_without_allocating_a_record_buffer() {
    let index = index::<64>(0);
    let observer = index.resource_observer();
    assert!(!observer.snapshot().file_open);
    assert_eq!(observer.snapshot().peak_buffer_bytes, 0);
    let mut reader = index.into_reader().unwrap();
    // 空输出也移交真实File，Reader的现有close和逻辑EOF协议不需要空实现或备用owner。
    assert!(observer.snapshot().file_open);
    #[cfg(feature = "paged-graphemes")]
    assert!(observer.snapshot().owner_closed); // Writer实际移交已结束，但File仍由下方Reader独占。
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

    // 构建器未跨线程；只有封存后的原File与原子收据被真实移交，且读取不再建立映射。
    std::thread::spawn(move || {
        let mut reader = reader;
        assert_eq!(reader.len(), 2051);
        assert_eq!(reader.read(2050).unwrap(), record::<64>(2050));
        let mut output = [[0; 64]; 256];
        reader.read_batch(250..506, &mut output).unwrap();
        for (offset, actual) in output.iter().enumerate() {
            assert_eq!(*actual, record::<64>(250 + offset));
        }
        reader.read_batch(2051..2051, &mut []).unwrap();
        assert_eq!(reader.resource_observer().snapshot().mapped_bytes, 0);
        reader.close().unwrap();
    })
    .join()
    .unwrap();
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(!released.reader_failed && !released.cleanup_failed);
    assert_eq!(released.peak_mapped_bytes, 0);
    assert_eq!(released.peak_buffer_bytes, PAGE);
}

#[test]
fn each_batch_limit_and_exact_output_length_revoke_without_modifying_output() {
    fn rejects<const N: usize>(range: Range<usize>, output_count: usize) {
        let mut reader = index::<N>(300).into_reader().unwrap();
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
    // 记录数、实际字节数、输出长度、逆向与EOF分别独立拒绝，不允许默默截短。
    rejects::<64>(0..257, 257);
    rejects::<128>(0..129, 129);
    rejects::<64>(0..2, 1);
    let reversed = Range { start: 2, end: 1 };
    rejects::<64>(reversed, 0);
    rejects::<64>(300..301, 1);
}

#[test]
fn actual_short_read_keeps_partial_output_negative_and_returns_the_file() {
    let mut reader = index::<64>(2).into_reader().unwrap();
    let observer = reader.resource_observer();
    // 只在测试内截短原唯一File，制造真实pread短读后EOF；不复制FD或提前关闭可复用descriptor。
    reader.file.as_ref().unwrap().set_len(96).unwrap();
    let mut output = [[0xab; 64]; 2];
    let error = reader.read_batch(0..2, &mut output).unwrap_err();
    assert!(
        matches!(error, Error::Io(ref cause) if cause.kind() == std::io::ErrorKind::UnexpectedEof)
    );
    assert_eq!(output[0], record::<64>(0));
    assert_eq!(&output[1][..32], &record::<64>(1)[..32]);
    assert_eq!(&output[1][32..], &[0xab; 32]);
    assert!(matches!(reader.read(0), Err(Error::Poisoned)));
    assert!(matches!(reader.close(), Err(Error::Poisoned)));
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(released.reader_failed && !released.cleanup_failed);
}

#[test]
fn actual_readonly_publish_failure_never_delivers_a_reader_and_returns_its_buffer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("readonly-index");
    std::fs::write(&path, b"unchanged").unwrap();
    let mut index = PagedLayoutIndex::<64>::with_file(File::open(&path).unwrap()).unwrap();
    for row in 0..3 {
        index.append(record::<64>(row)).unwrap();
    }
    let observer = index.resource_observer();
    index.seal().unwrap(); // 私有封口不写；真正into_reader必须先把全部记录交给原File。
    assert_eq!(index.read(2).unwrap(), record::<64>(2));
    assert!(
        matches!(index.into_reader(), Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(released.cleanup_failed && !released.reader_failed);
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[test]
fn dropping_a_transferred_reader_releases_the_original_file_without_a_second_owner() {
    let reader = index::<64>(2).into_reader().unwrap();
    let observer = reader.resource_observer();
    drop(reader);
    let released = observer.snapshot();
    assert!(!released.file_open && released.mapped_bytes == 0 && released.buffer_bytes == 0);
    assert!(!released.reader_failed && !released.cleanup_failed);
}
