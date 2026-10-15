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
