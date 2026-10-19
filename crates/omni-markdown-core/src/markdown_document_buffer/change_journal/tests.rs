use super::*;
use std::convert::Infallible;
use std::os::fd::AsRawFd;

fn record(start: usize, removed: usize, inserted: usize) -> Record {
    Record {
        start,
        removed,
        inserted,
    }
}
fn entry(journal: &mut ChangeJournal, count: usize) -> Entry {
    journal.begin(count + 10).unwrap();
    for start in 0..count {
        journal.push(record(start, 1, 2)).unwrap();
    }
    journal.finish(count * 2 + 10).unwrap()
}
fn values(journal: &ChangeJournal, entry: &Entry, inverse: bool) -> Vec<Record> {
    let mut result = Vec::new();
    let returned = journal.visit(entry, inverse, |value| {
        result.push(value);
        Ok::<_, Infallible>(())
    });
    assert!(returned.is_ok());
    result
}

#[test]
fn one_or_two_changes_never_create_file_or_page_directory() {
    let mut journal = ChangeJournal::default();
    for count in [1, 2] {
        let entry = entry(&mut journal, count);
        assert!(journal.file.is_none() && journal.owners.is_none());
        assert_eq!(values(&journal, &entry, false).len(), count);
        journal.release(entry);
    }
}

#[test]
fn forward_and_inverse_stream_preserve_original_adjacent_and_zero_width_order() {
    let mut journal = ChangeJournal::default();
    journal.begin(12).unwrap();
    let forward = [
        record(2, 2, 0),
        record(4, 3, 0),
        record(8, 0, 1),
        record(8, 0, 2),
    ];
    for value in forward {
        journal.push(value).unwrap();
    }
    let entry = journal.finish(10).unwrap();
    assert_eq!(values(&journal, &entry, false), forward);
    assert_eq!(
        values(&journal, &entry, true),
        [
            record(2, 0, 2),
            record(2, 0, 3),
            record(3, 1, 0),
            record(4, 2, 0)
        ]
    );
    assert_eq!(
        journal.file.as_ref().unwrap().metadata().unwrap().nlink(),
        0
    );
    assert_eq!(
        journal
            .file
            .as_ref()
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    journal.release(entry);
    assert!(journal.file.is_none() && journal.owners.is_none());
}

#[test]
fn many_pages_have_bounded_scratch_and_can_be_reused_after_branch_release() {
    let mut journal = ChangeJournal::default();
    let first = entry(&mut journal, RECORDS_PER_PAGE + 1);
    let second = entry(&mut journal, 3);
    let old_extent = journal.file.as_ref().unwrap().metadata().unwrap().len();
    journal.release(first);
    let third = entry(&mut journal, RECORDS_PER_PAGE + 1);
    assert_eq!(
        journal.file.as_ref().unwrap().metadata().unwrap().len(),
        old_extent
    );
    assert_eq!(values(&journal, &third, false).len(), RECORDS_PER_PAGE + 1);
    journal.release(second);
    journal.release(third);
    assert!(journal.file.is_none());
}

#[test]
fn creation_and_write_failures_leave_no_published_entry_or_named_file() {
    let mut journal = ChangeJournal::default();
    journal.begin(10).unwrap();
    journal.push(record(0, 0, 1)).unwrap();
    journal.push(record(1, 0, 1)).unwrap();
    journal.fault.set(Some(Fault::Create));
    assert_eq!(
        journal.push(record(2, 0, 1)),
        Err(MarkdownChangeJournalError::CreationUnavailable(
            io::Error::other("synthetic journal failure").into()
        ))
    );
    journal.abort().unwrap();
    assert!(journal.file.is_none() && journal.owners.is_none());
    journal.begin(10).unwrap();
    for at in 0..3 {
        journal.push(record(at, 0, 1)).unwrap();
    }
    journal.fault.set(Some(Fault::Write));
    assert!(matches!(
        journal.finish(13),
        Err(MarkdownChangeJournalError::WriteUnavailable(_))
    ));
    journal.abort().unwrap();
    assert!(journal.file.is_none() && journal.owners.is_none());
}

#[test]
fn read_failure_and_corruption_never_return_a_complete_stream() {
    let mut journal = ChangeJournal::default();
    let entry = entry(&mut journal, 4);
    journal.fault.set(Some(Fault::Read));
    assert!(matches!(
        journal.visit(&entry, true, |_| Ok::<_, Infallible>(())),
        Err(VisitError::Journal(
            MarkdownChangeJournalError::ReadUnavailable(_)
        ))
    ));
    // 只改一个仍处合法数值范围的记录字节；全流digest必须发现它，而非仅靠长度验证。
    journal
        .file
        .as_ref()
        .unwrap()
        .write_all_at(&1_u64.to_le_bytes(), HEADER_BYTES as u64)
        .unwrap();
    assert!(matches!(
        journal.visit(&entry, false, |_| Ok::<_, Infallible>(())),
        Err(VisitError::Journal(_))
    ));
    journal.release(entry);
}

#[test]
fn observer_failure_stops_read_without_releasing_history_entry() {
    let mut journal = ChangeJournal::default();
    let entry = entry(&mut journal, 5);
    assert!(matches!(
        journal.visit(&entry, true, |_| Err("mapper rejected")),
        Err(VisitError::Observer("mapper rejected"))
    ));
    assert_eq!(values(&journal, &entry, true).len(), 5);
    journal.release(entry);
}

#[test]
fn page_budget_is_checked_before_overwriting_any_live_history_page() {
    let mut journal = ChangeJournal {
        page_limit: 2,
        ..ChangeJournal::default()
    };
    let old = entry(&mut journal, 3);
    journal.begin(RECORDS_PER_PAGE + 20).unwrap();
    for at in 0..RECORDS_PER_PAGE {
        journal.push(record(at, 0, 1)).unwrap();
    }
    assert_eq!(
        journal.push(record(RECORDS_PER_PAGE, 0, 1)),
        Err(MarkdownChangeJournalError::BudgetExceeded)
    );
    journal.abort().unwrap();
    assert_eq!(values(&journal, &old, false).len(), 3);
    journal.release(old);
}

#[test]
fn truncate_failure_is_explicit_while_original_live_entry_remains_readable() {
    let mut journal = ChangeJournal::default();
    let old = entry(&mut journal, 3);
    journal.begin(RECORDS_PER_PAGE + 20).unwrap();
    for at in 0..=RECORDS_PER_PAGE {
        journal.push(record(at, 0, 1)).unwrap();
    }
    assert!(journal.file.as_ref().unwrap().metadata().unwrap().len() > PAGE_BYTES as u64);
    journal.fault.set(Some(Fault::Truncate));
    assert_eq!(
        journal.abort(),
        Err(MarkdownChangeJournalError::TruncateUnavailable(
            io::Error::other("synthetic journal failure").into()
        ))
    );
    assert!(journal.pending.is_none());
    assert_eq!(values(&journal, &old, false).len(), 3);
    assert!(
        journal
            .owners
            .as_ref()
            .unwrap()
            .iter()
            .filter(|id| **id != 0)
            .count()
            == 1
    );
    journal.release(old);
    assert!(journal.file.is_none());
}

#[test]
fn final_release_and_document_owner_drop_close_exact_descriptor() {
    for release in [false, true] {
        let mut journal = ChangeJournal::default();
        let entry = entry(&mut journal, 3);
        let fd = journal.file.as_ref().unwrap().as_raw_fd();
        let identity = journal.file.as_ref().unwrap().metadata().unwrap();
        if release {
            journal.release(entry);
        } else {
            let _unreleased_ticket = entry; // 票据本身没有FD；唯一owner析构必须负责关闭匿名文件。
        }
        drop(journal);
        let mut actual = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat只查询标量FD，并在成功时完整初始化实际stat缓冲，不读取文件正文。
        let status = unsafe { libc::fstat(fd, actual.as_mut_ptr()) };
        if status == 0 {
            // SAFETY: 上方fstat成功，全部字段已初始化；并行fixture可能已复用同一整数FD。
            let actual = unsafe { actual.assume_init() };
            assert!(actual.st_dev as u64 != identity.dev() || actual.st_ino != identity.ino());
        } else {
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        }
    }
}

#[test]
fn history_limit_eviction_and_redo_branch_discard_recycle_only_their_pages() {
    let mut journal = ChangeJournal::default();
    let mut history = std::collections::VecDeque::new();
    for _ in 0..140 {
        let next = entry(&mut journal, 3);
        if history.len() == 128 {
            journal.release(history.pop_front().unwrap());
        }
        history.push_back(next);
    }
    assert_eq!(
        journal
            .owners
            .as_ref()
            .unwrap()
            .iter()
            .filter(|id| **id != 0)
            .count(),
        128
    );
    let mut redo = Vec::new();
    for _ in 0..20 {
        redo.push(history.pop_back().unwrap());
    }
    for old in redo {
        journal.release(old);
    }
    let next = entry(&mut journal, 3);
    history.push_back(next);
    assert_eq!(
        journal
            .owners
            .as_ref()
            .unwrap()
            .iter()
            .filter(|id| **id != 0)
            .count(),
        109
    );
    for old in history {
        journal.release(old);
    }
    assert!(journal.file.is_none());
}

#[test]
fn actual_partial_page_write_is_truncated_without_touching_live_history() {
    let mut journal = ChangeJournal::default();
    let original = entry(&mut journal, 3);
    let before = journal.file.as_ref().unwrap().metadata().unwrap().len();
    journal.begin(10).unwrap();
    for start in 0..3 {
        journal
            .push(Record {
                start,
                removed: 1,
                inserted: 2,
            })
            .unwrap();
    }
    journal.fault.set(Some(Fault::PartialWrite));
    assert!(matches!(
        journal.finish(13),
        Err(MarkdownChangeJournalError::WriteUnavailable(_))
    ));
    assert!(journal.file.as_ref().unwrap().metadata().unwrap().len() > before);
    journal.abort().unwrap();
    assert_eq!(
        journal.file.as_ref().unwrap().metadata().unwrap().len(),
        before
    );
    assert_eq!(values(&journal, &original, false).len(), 3);
    journal.release(original);
    assert!(journal.file.is_none());
}

#[test]
fn short_read_and_ignored_push_error_cannot_become_successful_history() {
    let mut journal = ChangeJournal::default();
    journal.begin(10).unwrap();
    journal.push(record(5, 1, 1)).unwrap();
    assert_eq!(
        journal.push(record(4, 1, 1)),
        Err(MarkdownChangeJournalError::InvalidChange)
    );
    assert!(matches!(
        journal.finish(10),
        Err(MarkdownChangeJournalError::InvalidChange)
    ));
    journal.abort().unwrap();
    let entry = entry(&mut journal, 3);
    journal
        .file
        .as_ref()
        .unwrap()
        .set_len((PAGE_BYTES - 1) as u64)
        .unwrap();
    let result = journal.visit(&entry, false, |_| Ok::<_, Infallible>(()));
    assert!(matches!(
        result,
        Err(VisitError::Journal(
            MarkdownChangeJournalError::ReadUnavailable(_)
        ))
    ));
    journal.release(entry);
    assert!(journal.file.is_none());
}

#[test]
fn io_failures_keep_finite_kind_and_os_code_without_system_english_in_display() {
    let details = MarkdownChangeJournalIoFailure::from(io::Error::from_raw_os_error(libc::EACCES));
    assert_eq!(details.kind, io::ErrorKind::PermissionDenied);
    assert_eq!(details.raw_os_error, Some(libc::EACCES));
    let errors = [
        (
            MarkdownChangeJournalError::CreationUnavailable(details),
            "无法创建私有匿名变化日志",
        ),
        (
            MarkdownChangeJournalError::WriteUnavailable(details),
            "无法完整写入变化日志",
        ),
        (
            MarkdownChangeJournalError::ReadUnavailable(details),
            "无法完整读取变化日志",
        ),
        (
            MarkdownChangeJournalError::TruncateUnavailable(details),
            "变化日志失败后的截尾清理未确认",
        ),
    ];
    for (error, display) in errors {
        // Copy错误保留同一数值原因，中文展示与raw诊断职责分开，不透传系统英文。
        let copied = error;
        assert_eq!(copied, error);
        assert_eq!(copied.to_string(), display);
    }
}
