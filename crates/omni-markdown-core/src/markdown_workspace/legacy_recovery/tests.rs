//! 真实旧header/文件系统/映射与marker回归；完整String只作为这些有限合成fixture的断言oracle。

use super::*;
use crate::markdown_document_buffer::MarkdownTextSelection;

/// 每个case独占临时vault/appdata，不访问用户目录或产品进程。
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    recovery: PathBuf,
    access: MarkdownSessionAccess,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated recovery fixture");
        let root = directory.path().join("vault");
        let appdata = directory.path().join("appdata");
        fs::create_dir(&root).expect("fixture vault");
        fs::write(root.join("Note.md"), b"external disk\n").expect("fixture disk");
        let workspace = MarkdownWorkspace::new(root.clone(), appdata.clone());
        let access = workspace.open_session().expect("fixture session");
        Self {
            _directory: directory,
            root,
            recovery: appdata.join(CONFLICT_RECOVERY_DIRECTORY_NAME),
            access,
        }
    }

    /// 使用旧写入接口产生实际OMNIMD1格式，后续只走新的原生映射入口。
    fn draft(&self, content: &str) -> PathBuf {
        self.access
            .update_conflict_draft("Note.md", content)
            .expect("legacy draft bytes");
        conflict_draft_path(&self.recovery, "Note.md")
    }

    fn open(&self) -> MarkdownDocumentBuffer {
        self.access
            .open_document_buffer("Note.md")
            .expect("open mapped recovery")
    }

    /// 纯core回归按prepare→绑定核验→finish顺序完成Reload；不模拟AppKit释放结论。
    fn resolve_reload(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<Option<MarkdownRecoveryRecording>, MarkdownWorkspaceError> {
        self.finish(
            document,
            self.access.prepare_legacy_recovery_after_reload(document)?,
        )
    }

    /// 使用实际已保存receipt完成相同的两阶段协议，确保marker不是从准备函数提前写出的。
    fn resolve_copy(
        &self,
        document: &MarkdownDocumentBuffer,
        saved: &MarkdownNoteMutationReceipt,
    ) -> Result<Option<MarkdownRecoveryRecording>, MarkdownWorkspaceError> {
        self.finish(
            document,
            self.access
                .prepare_legacy_recovery_after_save_as_new(document, saved)?,
        )
    }

    fn finish(
        &self,
        document: &MarkdownDocumentBuffer,
        preparation: MarkdownRecoveryPreparation,
    ) -> Result<Option<MarkdownRecoveryRecording>, MarkdownWorkspaceError> {
        match preparation {
            // 普通模型没有恢复决定，也不能顺带创建marker。
            MarkdownRecoveryPreparation::NotPending => Ok(None),
            MarkdownRecoveryPreparation::Prepared(handoff) => {
                // 测试只验证core事实；生产适配层必须在核验后先通过旧editor strict barrier。
                self.access
                    .validate_legacy_recovery_handoff(document, &handoff)?;
                let outcome = self.access.open_resolved_legacy_recovery(handoff)?;
                Ok(Some(outcome.recording))
            }
        }
    }
}

/// 合成小文档只在测试oracle中展平，生产恢复和保存不会调用此函数。
fn text(document: &MarkdownDocumentBuffer) -> String {
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).expect("stream fixture");
    String::from_utf8(bytes).expect("validated fixture UTF8")
}

#[test]
/// Unicode/CRLF旧record从payload映射恢复，header不进入正文，初值不复制进append segments。
fn actual_legacy_header_recovers_unicode_with_no_flat_edit_copy() {
    let fixture = Fixture::new();
    let payload = "# 待恢复 😀e\u{301}\r\n正文\n";
    let record = fixture.draft(payload);
    let bytes = fs::read(&record).unwrap();
    assert_eq!(&bytes[..8], b"OMNIMD1\0");
    assert_eq!(u32::from_be_bytes(bytes[8..12].try_into().unwrap()), 7);
    let mut document = fixture.open();
    assert_eq!(text(&document), payload);
    assert_eq!(document.byte_len(), payload.len());
    assert_eq!(document.utf16_len(), payload.encode_utf16().count());
    assert!(document.has_pending_legacy_recovery());
    assert!(document.is_dirty());
    assert_eq!(document.original_revision(), None);
    assert!(!document.can_undo());
    let storage = document.storage_profile();
    assert_eq!(storage.mapped_original_bytes, bytes.len());
    assert_eq!(storage.appended_edit_bytes, 0);
    assert_eq!(storage.allocated_edit_segment_bytes, 0);
    assert_eq!(
        fixture.access.save_document_buffer(&mut document).err(),
        Some(MarkdownWorkspaceError::PendingLegacyRecovery)
    );
    assert_eq!(
        fs::read(fixture.root.join("Note.md")).unwrap(),
        b"external disk\n"
    );
    assert_eq!(fs::read(record).unwrap(), bytes);
}

#[test]
/// empty或恰等于当前disk也没有旧base证据，普通保存必须在任何磁盘操作之前拒绝。
fn empty_and_equal_disk_recovery_never_become_clean_or_writable() {
    for payload in ["", "external disk\n"] {
        // 每种边界都有独立真实文件与mapping，不共享marker状态。
        let fixture = Fixture::new();
        fixture.draft(payload);
        let mut document = fixture.open();
        assert!(document.is_dirty());
        assert_eq!(document.original_revision(), None);
        assert_eq!(text(&document), payload);
        fs::rename(fixture.root.join("Note.md"), fixture.root.join("Moved.md")).unwrap();
        // 连原路径已不存在也优先返回typed Pending，而不是把它当作一次普通NoteNotFound/save。
        assert_eq!(
            fixture.access.save_document_buffer(&mut document).err(),
            Some(MarkdownWorkspaceError::PendingLegacyRecovery)
        );
        assert!(document.is_dirty());
        assert_eq!(text(&document), payload);
    }
}

#[test]
/// 恢复后的真实编辑history沿用piece树；Undo回初值仍pending，记录源被截断也不会破坏映射。
fn recovery_history_and_snapshot_survive_mutable_record_replacement() {
    let fixture = Fixture::new();
    let record = fixture.draft("旧草稿😀");
    let mut document = fixture.open();
    document
        .replace_utf16_range(0..0, "new ", MarkdownTextSelection::caret(4))
        .unwrap();
    assert_eq!(text(&document), "new 旧草稿😀");
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&record)
        .unwrap();
    document.undo().unwrap();
    assert_eq!(text(&document), "旧草稿😀");
    assert!(document.is_dirty());
    document.redo().unwrap();
    assert_eq!(text(&document), "new 旧草稿😀");
    assert_eq!(document.original_revision(), None);
}

#[test]
/// SaveAsNew的当前live bytes持久化后才写marker，原file/record/旧history始终保留。
fn save_as_new_resolves_only_after_live_copy_and_keeps_record() {
    let fixture = Fixture::new();
    let record = fixture.draft("旧草稿😀");
    let record_before = fs::read(&record).unwrap();
    let mut document = fixture.open();
    document
        .replace_utf16_range(0..0, "typed ", MarkdownTextSelection::caret(6))
        .unwrap();
    let saved = fixture
        .access
        .save_document_buffer_as_new(&document, "", "Recovered")
        .unwrap();
    assert_eq!(
        fs::read(fixture.root.join(&saved.relative_path)).unwrap(),
        "typed 旧草稿😀".as_bytes()
    );
    assert_eq!(
        fixture.resolve_copy(&document, &saved).unwrap(),
        Some(MarkdownRecoveryRecording::Recorded)
    );
    assert_eq!(
        fixture.resolve_copy(&document, &saved).unwrap(),
        Some(MarkdownRecoveryRecording::Recorded)
    );
    // marker不解除仍活动的旧模型保护；原生需先完成原有release/reopen事务。
    assert_eq!(
        fixture.access.save_document_buffer(&mut document).err(),
        Some(MarkdownWorkspaceError::PendingLegacyRecovery)
    );
    assert!(document.can_undo());
    assert_eq!(fs::read(&record).unwrap(), record_before);
    assert_eq!(text(&fixture.open()), "external disk\n");
    assert!(!fixture.open().has_pending_legacy_recovery());
    assert_eq!(
        fs::read(fixture.root.join("Note.md")).unwrap(),
        b"external disk\n"
    );
}

#[test]
/// 即使同record已有合法marker，也不能用不含当前live草稿的receipt跳过副本核验。
fn existing_resolution_does_not_bypass_new_live_copy_verification() {
    let fixture = Fixture::new();
    fixture.draft("draft A");
    let mut document = fixture.open();
    assert_eq!(
        fixture.resolve_reload(&document).unwrap(),
        Some(MarkdownRecoveryRecording::Recorded)
    );
    let saved = fixture
        .access
        .save_document_buffer_as_new(&document, "", "Copy")
        .unwrap();
    assert_eq!(
        fixture.resolve_copy(&document, &saved).unwrap(),
        Some(MarkdownRecoveryRecording::AlreadyResolved)
    );
    document
        .replace_utf16_range(0..0, "later ", MarkdownTextSelection::caret(6))
        .unwrap();
    assert_eq!(
        fixture.resolve_copy(&document, &saved).err(),
        Some(MarkdownWorkspaceError::RecoveryResolutionUnverified)
    );
    assert_eq!(text(&document), "later draft A");
}

#[test]
/// record bytes变化不会命中旧marker；相同bytes重写新inode仍是已解决的同一内容身份。
fn changed_record_is_pending_while_identical_new_inode_is_resolved() {
    let fixture = Fixture::new();
    fixture.draft("draft A");
    let old = fixture.open();
    fixture.draft("draft B");
    fixture.resolve_reload(&old).unwrap();
    let newer = fixture.open();
    assert_eq!(text(&newer), "draft B");
    assert!(newer.has_pending_legacy_recovery());
    fixture.draft("draft A");
    let reopened = fixture.open();
    assert_eq!(text(&reopened), "external disk\n");
    assert!(!reopened.has_pending_legacy_recovery());
}

#[test]
/// 坏marker既不抑制旧草稿也不被覆盖；显式操作分别返回可交接事实与记录失败。
fn corrupt_marker_preserves_recovery_and_is_never_overwritten() {
    let fixture = Fixture::new();
    fixture.draft("draft A");
    let document = fixture.open();
    let digest = document.legacy_recovery_record_digest().unwrap();
    let path = marker_path(&fixture.recovery, "Note.md", &digest);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(b"broken marker").unwrap();
    drop(file);
    assert_eq!(text(&fixture.open()), "draft A");
    assert!(fixture.open().has_pending_legacy_recovery());
    assert_eq!(
        fixture.resolve_reload(&document).unwrap(),
        Some(MarkdownRecoveryRecording::NotRecorded(
            MarkdownWorkspaceError::RecoveryResolutionCorrupted
        ))
    );
    assert_eq!(fs::read(path).unwrap(), b"broken marker");
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 2);
}

#[test]
/// 坏marker不锁住已核验的两种显式决定；普通打开仍pending，原件/副本/record/marker全部不变。
fn corrupt_marker_keeps_files_but_allows_only_verified_explicit_handoffs() {
    for reload in [true, false] {
        // 两个动作各用独立真实vault，不能借另一动作的合法marker绕过损坏分支。
        let fixture = Fixture::new();
        let record = fixture.draft("草稿😀");
        let record_before = fs::read(&record).unwrap();
        let mut document = fixture.open();
        document
            .replace_utf16_range(0..0, "live ", MarkdownTextSelection::caret(5))
            .unwrap();
        let digest = document.legacy_recovery_record_digest().unwrap();
        let marker = marker_path(&fixture.recovery, "Note.md", &digest);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker)
            .unwrap();
        file.write_all(b"broken marker").unwrap();
        drop(file);
        let (preparation, expected_target, expected_body) = if reload {
            // 明确Reload只准备当前已验证disk目标，尚无marker或replacement模型。
            (
                fixture
                    .access
                    .prepare_legacy_recovery_after_reload(&document)
                    .unwrap(),
                "Note.md".to_owned(),
                "external disk\n",
            )
        } else {
            // 真SaveAsNew已持久化live pieces；准备ticket仍不会尝试覆盖损坏marker。
            let saved = fixture
                .access
                .save_document_buffer_as_new(&document, "", "Copy")
                .unwrap();
            (
                fixture
                    .access
                    .prepare_legacy_recovery_after_save_as_new(&document, &saved)
                    .unwrap(),
                saved.relative_path,
                "live 草稿😀",
            )
        };
        let MarkdownRecoveryPreparation::Prepared(handoff) = preparation else {
            // 真实pending fixture应授予唯一目标，错误形态不能继续执行后面的文件断言。
            panic!("pending model must produce prepared handoff");
        };
        assert_eq!(handoff.target_relative_path(), expected_target);
        fixture
            .access
            .validate_legacy_recovery_handoff(&document, &handoff)
            .unwrap();
        // 纯core测试模拟适配层已完成strict barrier；真实AppKit释放只由Root原生验收。
        let outcome = fixture
            .access
            .open_resolved_legacy_recovery(handoff)
            .unwrap();
        assert_eq!(
            outcome.recording,
            MarkdownRecoveryRecording::NotRecorded(
                MarkdownWorkspaceError::RecoveryResolutionCorrupted
            )
        );
        assert!(!outcome.document.has_pending_legacy_recovery());
        assert_eq!(text(&outcome.document), expected_body);
        assert!(document.has_pending_legacy_recovery());
        assert!(document.can_undo());
        assert_eq!(text(&document), "live 草稿😀");
        assert!(fixture.open().has_pending_legacy_recovery());
        assert_eq!(text(&fixture.open()), "草稿😀");
        assert_eq!(fs::read(&record).unwrap(), record_before);
        assert_eq!(fs::read(&marker).unwrap(), b"broken marker");
        assert_eq!(
            fs::read(fixture.root.join("Note.md")).unwrap(),
            b"external disk\n"
        );
        assert_eq!(
            fs::read(fixture.root.join(expected_target)).unwrap(),
            expected_body.as_bytes()
        );
    }
}

#[test]
/// 旧editor辞任时的新输入拒绝ticket且无marker；目标在barrier后变化也不发布任何决定。
fn handoff_rejects_changed_live_model_or_changed_target_before_marker() {
    for source_changed in [true, false] {
        // 先准备实际Reload ticket，再改变其依赖的唯一事实；准备阶段不创建新DocumentBuffer。
        let fixture = Fixture::new();
        let record = fixture.draft("draft A");
        let record_before = fs::read(&record).unwrap();
        let mut document = fixture.open();
        let preparation = fixture
            .access
            .prepare_legacy_recovery_after_reload(&document)
            .unwrap();
        let MarkdownRecoveryPreparation::Prepared(handoff) = preparation else {
            // 此fixture有真实旧record，缺失ticket代表准备合同未满足。
            panic!("pending model must produce handoff");
        };
        let marker = marker_path(&fixture.recovery, "Note.md", &handoff.record_digest);
        assert!(!marker.exists());
        if source_changed {
            // 模拟resign期间的新输入；纯绑定预检拒绝，原生必须丢ticket并保留同一editor。
            document
                .replace_utf16_range(0..0, "later ", MarkdownTextSelection::caret(6))
                .unwrap();
            assert_eq!(
                fixture
                    .access
                    .validate_legacy_recovery_handoff(&document, &handoff)
                    .err(),
                Some(MarkdownWorkspaceError::RecoveryResolutionUnverified)
            );
            drop(handoff);
        } else {
            // 预检成功后目标发生外部写入，finish打开验证失败也必须早于marker发布。
            fixture
                .access
                .validate_legacy_recovery_handoff(&document, &handoff)
                .unwrap();
            fs::write(fixture.root.join("Note.md"), "new disk").unwrap();
            assert_eq!(
                fixture.access.open_resolved_legacy_recovery(handoff).err(),
                Some(MarkdownWorkspaceError::RecoveryResolutionUnverified)
            );
        }
        assert!(!marker.exists());
        assert!(document.has_pending_legacy_recovery());
        assert!(document.is_dirty());
        assert_eq!(
            text(&document),
            if source_changed {
                "later draft A"
            } else {
                "draft A"
            }
        );
        assert_eq!(fs::read(&record).unwrap(), record_before);
        assert!(fixture.open().has_pending_legacy_recovery());
    }
}

#[test]
/// 同路径后来出现的另一record不能借旧ticket交接；拒绝后两个版本都没有被“已解决”标记隐藏。
fn prepared_ticket_does_not_authorize_a_new_pending_record_identity() {
    let fixture = Fixture::new();
    fixture.draft("draft A");
    let old = fixture.open();
    let prepared = fixture
        .access
        .prepare_legacy_recovery_after_reload(&old)
        .unwrap();
    let MarkdownRecoveryPreparation::Prepared(ticket) = prepared else {
        // 真实pending的旧版本必须产生其专属ticket，不能跳过下面的身份反事实。
        panic!("old pending record must prepare a ticket");
    };
    let old_marker = marker_path(&fixture.recovery, "Note.md", &ticket.record_digest);
    fixture.draft("draft B");
    let current = fixture.open();
    assert_eq!(
        fixture
            .access
            .validate_legacy_recovery_handoff(&current, &ticket)
            .err(),
        Some(MarkdownWorkspaceError::RecoveryResolutionUnverified)
    );
    drop(ticket);
    assert!(!old_marker.exists());
    assert_eq!(text(&old), "draft A");
    assert_eq!(text(&current), "draft B");
    assert!(fixture.open().has_pending_legacy_recovery());
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 1);
}

#[test]
/// 真发布后dirsync失败留下完整marker，不能rollback；重试同步后精确幂等。
fn post_publish_sync_failure_preserves_marker_and_retry_is_idempotent() {
    let fixture = Fixture::new();
    let record = fixture.draft("draft A");
    let old_bytes = fs::read(&record).unwrap();
    let document = fixture.open();
    let digest = document.legacy_recovery_record_digest().unwrap();
    let actual = inspect_markdown_save_baseline(&fixture.root.join("Note.md")).unwrap();
    let receipt = ResolutionReceipt {
        kind: 1,
        target_path: "",
        revision: decode_revision(&actual.revision).unwrap(),
        byte_count: actual.byte_count,
    };
    let bytes = encode_resolution("Note.md", &digest, &receipt).unwrap();
    let outcome = publish_resolution_marker(
        &fixture.recovery,
        "Note.md",
        &digest,
        &bytes,
        &fixture.access.save_sequence,
        |_| {
            // 此seam位于真实rename之后，只制造持久化不确定，不跳过文件发布。
            Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable)
        },
    );
    assert_eq!(
        outcome.err(),
        Some(MarkdownWorkspaceError::RecoveryResolutionDurabilityUncertain)
    );
    assert_eq!(
        read_resolution_marker(&fixture.recovery, "Note.md", &digest).unwrap(),
        Some(bytes.clone())
    );
    assert_eq!(fs::read(&record).unwrap(), old_bytes);
    assert_eq!(
        publish_resolution_marker(
            &fixture.recovery,
            "Note.md",
            &digest,
            &bytes,
            &fixture.access.save_sequence,
            sync_conflict_recovery_directory
        )
        .unwrap(),
        MarkerPublication::Idempotent
    );
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 2);
}

#[test]
/// 真rename后同步不确定单独报告，仍返回已验证唯一目标，不冒充记录成功或回滚最终文件。
fn uncertain_marker_publication_keeps_explicit_handoff_and_complete_record() {
    let fixture = Fixture::new();
    let record = fixture.draft("draft A");
    let record_before = fs::read(&record).unwrap();
    let document = fixture.open();
    let digest = document.legacy_recovery_record_digest().unwrap();
    let preparation = fixture
        .access
        .prepare_legacy_recovery_after_reload(&document)
        .unwrap();
    let MarkdownRecoveryPreparation::Prepared(handoff) = preparation else {
        // fixture真实pending，不能将缺失ticket当成故障注入通过。
        panic!("verified pending recovery needs one bounded handoff");
    };
    fixture
        .access
        .validate_legacy_recovery_handoff(&document, &handoff)
        .unwrap();
    let outcome = fixture
        .access
        .open_recovery_handoff_with_sync(handoff, |_| {
            // seam仅在真实目标打开、完整写入/fsync/exclusive rename后拒绝最终dirsync。
            Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable)
        })
        .unwrap();
    assert_eq!(
        outcome.recording,
        MarkdownRecoveryRecording::NotRecorded(
            MarkdownWorkspaceError::RecoveryResolutionDurabilityUncertain
        )
    );
    assert_eq!(text(&outcome.document), "external disk\n");
    assert!(!outcome.document.has_pending_legacy_recovery());
    assert!(document.has_pending_legacy_recovery());
    let marker_before = read_resolution_marker(&fixture.recovery, "Note.md", &digest)
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&record).unwrap(), record_before);
    assert_eq!(
        read_resolution_marker(&fixture.recovery, "Note.md", &digest).unwrap(),
        Some(marker_before)
    );
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 2);
}

#[test]
/// 坏旧header/path/UTF8不能构造部分恢复模型，也不能改变原disk或修复record。
fn malformed_record_variants_fail_without_touching_disk() {
    for mutation in 0..6 {
        // 每种损坏在独立真实record上执行，防止前一失败影响后一断言。
        let fixture = Fixture::new();
        let record = fixture.draft("draft A");
        let mut bytes = fs::read(&record).unwrap();
        match mutation {
            // 错误magic不能被误认成另一个版本。
            0 => bytes[0] = b'X',
            // 同长度错误内嵌path不能借正确hash文件名冒充目标。
            1 => bytes[20] = b'X',
            // 非法UTF8正文必须拒绝而不是lossy替换。
            2 => *bytes.last_mut().unwrap() = 0xff,
            // 额外尾字节不允许按前缀加载。
            3 => bytes.push(0),
            // 正文长度溢出/超限在固定头预检拒绝，不按不可信声明分配或建立source mapping。
            4 => bytes[12..20].copy_from_slice(&u64::MAX.to_be_bytes()),
            // 路径长度同样必须有界，不能让header把正文或超限空间解释成path。
            _ => bytes[8..12].copy_from_slice(&u32::MAX.to_be_bytes()),
        }
        fs::write(&record, &bytes).unwrap();
        assert_eq!(
            fixture.access.open_document_buffer("Note.md").err(),
            Some(MarkdownWorkspaceError::ConflictDraftCorrupted)
        );
        assert_eq!(fs::read(&record).unwrap(), bytes);
        assert_eq!(
            fs::read(fixture.root.join("Note.md")).unwrap(),
            b"external disk\n"
        );
    }
}

#[test]
/// 普通无record路径与非pending显式动作不创建额外持久存储。
fn ordinary_note_never_allocates_recovery_storage() {
    let fixture = Fixture::new();
    let document = fixture.open();
    assert!(!document.is_dirty());
    assert!(document.original_revision().is_some());
    assert_eq!(fixture.resolve_reload(&document).unwrap(), None);
    assert!(!fixture.recovery.exists());
}

#[test]
/// 真symlink与共享权限record均拒绝，不能因同样的header或内容就绕过私有文件边界。
fn unsafe_record_roles_never_supply_mapped_source() {
    for symlink in [false, true] {
        // 两条失败路径分别使用完整独立fixture，避免权限变化影响测试清理或后续case。
        let fixture = Fixture::new();
        let record = fixture.draft("draft A");
        if symlink {
            // 外部目标仍属于测试TempDir；原生入口不能跟随该最后组件链接。
            let outside = fixture._directory.path().join("outside-record");
            fs::rename(&record, &outside).unwrap();
            std::os::unix::fs::symlink(&outside, &record).unwrap();
        } else {
            // 共享权限违反旧record私有合同，不得自动chmod后继续接受。
            fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(
            fixture.access.open_document_buffer("Note.md").err(),
            Some(MarkdownWorkspaceError::ConflictDraftCorrupted)
        );
        assert_eq!(
            fs::read(fixture.root.join("Note.md")).unwrap(),
            b"external disk\n"
        );
    }
}

#[test]
/// 合法marker内容换到错误binding、超限或symlink路径都不能抑制真正草稿，也不被覆盖。
fn marker_binding_extent_and_link_are_checked_before_resolution() {
    for failure in 0..3 {
        // 先产生通过真实Reload核验的完整marker，再修改其存储事实，避免伪造一个成功receipt。
        let fixture = Fixture::new();
        fixture.draft("draft A");
        let document = fixture.open();
        let digest = document.legacy_recovery_record_digest().unwrap();
        fixture.resolve_reload(&document).unwrap();
        let path = marker_path(&fixture.recovery, "Note.md", &digest);
        match failure {
            0 => {
                // checksum也重新计算，证明错误record/path binding不能只靠完整性hash通过。
                let mut bytes = fs::read(&path).unwrap();
                bytes[12] ^= 1;
                let checksum_start = bytes.len() - RESOLUTION_CHECKSUM_BYTES;
                let checksum = Sha256::digest(&bytes[..checksum_start]);
                bytes[checksum_start..].copy_from_slice(&checksum);
                fs::write(&path, bytes).unwrap();
            }
            1 => {
                // 长度攻击在分配或解析前拒绝，不能因为filename命中就宣称resolved。
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(MAX_RESOLUTION_BYTES as u64 + 1)
                    .unwrap();
            }
            _ => {
                // 链接指向同样合法bytes也没有权限成为本private marker。
                let outside = fixture._directory.path().join("outside-marker");
                fs::rename(&path, &outside).unwrap();
                std::os::unix::fs::symlink(&outside, &path).unwrap();
            }
        }
        assert!(fixture.open().has_pending_legacy_recovery());
        assert_eq!(
            fixture.resolve_reload(&document).unwrap(),
            Some(MarkdownRecoveryRecording::NotRecorded(
                MarkdownWorkspaceError::RecoveryResolutionCorrupted
            ))
        );
    }
}

#[test]
/// 两个真实发布者竞争同record的不同合法决定时只保留一个完整marker，另一方明确AlreadyResolved。
fn concurrent_valid_resolutions_publish_one_immutable_marker() {
    let fixture = Fixture::new();
    let record = fixture.draft("draft A");
    let document = fixture.open();
    let digest = document.legacy_recovery_record_digest().unwrap();
    let copy = fixture
        .access
        .save_document_buffer_as_new(&document, "", "Copy")
        .unwrap();
    let disk = inspect_markdown_save_baseline(&fixture.root.join("Note.md")).unwrap();
    let first = encode_resolution(
        "Note.md",
        &digest,
        &ResolutionReceipt {
            kind: 1,
            target_path: "",
            revision: decode_revision(&disk.revision).unwrap(),
            byte_count: disk.byte_count,
        },
    )
    .unwrap();
    let second = encode_resolution(
        "Note.md",
        &digest,
        &ResolutionReceipt {
            kind: 2,
            target_path: &copy.relative_path,
            revision: decode_revision(&copy.revision).unwrap(),
            byte_count: copy.byte_count,
        },
    )
    .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for bytes in [first, second] {
        // 两个线程只操作自己有界的临时文件和同一最终key；没有产品线程、AppKit或时钟成功条件。
        let barrier = Arc::clone(&barrier);
        let directory = fixture.recovery.clone();
        let sequence = Arc::clone(&fixture.access.save_sequence);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            publish_resolution_marker(
                &directory,
                "Note.md",
                &digest,
                &bytes,
                &sequence,
                sync_conflict_recovery_directory,
            )
            .unwrap()
        }));
    }
    let mut outcomes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == MarkerPublication::Written)
            .count(),
        1
    );
    outcomes.retain(|outcome| *outcome != MarkerPublication::Written);
    assert_eq!(outcomes, [MarkerPublication::AlreadyResolved]);
    assert!(
        read_resolution_marker(&fixture.recovery, "Note.md", &digest)
            .unwrap()
            .is_some()
    );
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 2);
    assert!(record.exists());
    assert_eq!(
        fs::read(fixture.root.join("Note.md")).unwrap(),
        b"external disk\n"
    );
}
