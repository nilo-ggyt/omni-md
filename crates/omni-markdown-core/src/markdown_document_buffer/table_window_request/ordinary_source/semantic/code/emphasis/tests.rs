//! Actual anonymous-stack paging, history reads, and descriptor release through real file I/O.

use super::*;

#[test]
/// Read exact historical records through the original descriptor after paging, then verify close
/// against the actual inode rather than a reusable descriptor number.
fn stack_file_reads_real_history_and_closes_original_descriptor() {
    let quota = MarkdownTableWindowQuota::new(10000, 1024 * 1024);
    let mut file = StackFile::new();
    for index in 0..80 {
        let work = loop {
            let step = file
                .append(
                    Entry {
                        previous: index,
                        depth: index,
                        start: index * 11,
                        count: 2,
                        run_length: 2,
                        delimiter: b'*',
                        both: false,
                    },
                    &quota,
                    4096,
                )
                .unwrap();
            match step {
                RecordStep::Progress { work } => {
                    assert!(work.structural_transitions <= 4096 && work.metadata_bytes <= 4096);
                }
                RecordStep::Ready { work, .. } => break work,
            }
        };
        assert!(work.structural_transitions <= 4096);
        assert!(work.metadata_bytes <= 4096);
    }
    for index in [1, 32, 33, 64, 65, 80] {
        let entry = file.read(index).unwrap();
        assert_eq!(entry.start, (index - 1) * 11);
        assert_eq!(entry.previous, index - 1);
    }
    let descriptor = file.descriptor().unwrap();
    let identity = super::super::tests::descriptor_identity(descriptor).unwrap();
    file.close().unwrap();
    assert!(file.descriptor().is_none());
    // Close has succeeded. Another test may reuse this number, but it must refer to a different inode.
    assert_ne!(
        super::super::tests::descriptor_identity(descriptor),
        Some(identity)
    );
}
