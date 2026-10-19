//! 从crate外部使用根公开API完成真实合成恢复流程，防止内部单测掩盖导出缺失。

#![cfg(feature = "native-markdown-candidate")]

use std::fs;

use omni_markdown_core::{
    MarkdownRecoveryHandoff, MarkdownRecoveryPreparation, MarkdownRecoveryRecording,
    MarkdownRecoveryResolutionOutcome, MarkdownWorkspace, MarkdownWorkspaceError,
};

#[test]
/// 外部消费者可命名四个根类型；准备无marker，释放纯buffer后才能完成唯一目标交接。
fn root_exports_support_a_complete_external_recovery_consumer() {
    // 所有读写只在本case独占TempDir，旧格式由真正公开writer生成，不接触个人目录。
    let temporary = tempfile::tempdir().expect("isolated external recovery consumer");
    let root = temporary.path().join("vault");
    let appdata = temporary.path().join("appdata");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("Note.md"), b"disk version\n").unwrap();
    let workspace = MarkdownWorkspace::new(root.clone(), appdata.clone());
    let session = workspace.open_session().unwrap();
    session
        .update_conflict_draft("Note.md", "待恢复😀")
        .unwrap();
    let recovery_directory = appdata.join("markdown-conflict-recovery");
    let record_path = fs::read_dir(&recovery_directory)
        .unwrap()
        .next()
        .expect("one actual legacy record")
        .unwrap()
        .path();
    let record_before = fs::read(&record_path).unwrap();

    // 未知旧base仍由公开保存入口拒绝，不能因为调用方能命名ticket就获得原文件覆盖权限。
    let mut document = session.open_document_buffer("Note.md").unwrap();
    assert!(document.has_pending_legacy_recovery());
    assert_eq!(document.original_revision(), None);
    assert_eq!(
        session.save_document_buffer(&mut document).err(),
        Some(MarkdownWorkspaceError::PendingLegacyRecovery)
    );
    let preparation: MarkdownRecoveryPreparation = session
        .prepare_legacy_recovery_after_reload(&document)
        .unwrap();
    let ticket: MarkdownRecoveryHandoff = match preparation {
        // 真正的pending fixture必须走具体ticket，而不是仅靠存在类型名称通过编译。
        MarkdownRecoveryPreparation::Prepared(ticket) => ticket,
        // 外部API若错误地把恢复模型降为普通模型，本测试立即停止，不执行假交接。
        MarkdownRecoveryPreparation::NotPending => panic!("legacy record must remain pending"),
    };
    assert_eq!(ticket.target_relative_path(), "Note.md");
    session
        .validate_legacy_recovery_handoff(&document, &ticket)
        .unwrap();
    assert_eq!(fs::read_dir(&recovery_directory).unwrap().count(), 1);
    // 这里只证明纯document owner可先drop；真正AppKit strict barrier仍由原生适配层验证。
    drop(document);

    let outcome: MarkdownRecoveryResolutionOutcome =
        session.open_resolved_legacy_recovery(ticket).unwrap();
    let recording: MarkdownRecoveryRecording = outcome.recording;
    assert_eq!(recording, MarkdownRecoveryRecording::Recorded);
    assert!(!outcome.document.has_pending_legacy_recovery());
    assert!(!outcome.document.is_dirty());
    // 仅把十几字节合成目标流入测试oracle；生产恢复和交接不构造整篇正文镜像。
    let mut actual = Vec::new();
    outcome.document.write_to(&mut actual).unwrap();
    assert_eq!(actual, b"disk version\n");
    assert_eq!(fs::read(root.join("Note.md")).unwrap(), b"disk version\n");
    assert_eq!(fs::read(&record_path).unwrap(), record_before);
    assert_eq!(fs::read_dir(&recovery_directory).unwrap().count(), 2);
    assert!(
        !session
            .open_document_buffer("Note.md")
            .unwrap()
            .has_pending_legacy_recovery()
    );
}
