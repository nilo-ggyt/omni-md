//! 旧record的映射恢复与不可变内容身份marker；旧.draft始终原位保留，不执行删除或搬移。

use super::*;
use crate::immutable_snapshot::snapshot_canonical;
use crate::markdown_document_buffer::MarkdownDocumentRevision;

/// 使用原格式头与精确内嵌path验证同一冻结record；正文不借为完整slice。
fn bounded_conflict_payload_range(
    snapshot: &crate::original_backing::OriginalBacking,
    expected: &str,
) -> Result<std::ops::Range<usize>, MarkdownWorkspaceError> {
    if snapshot.len() < CONFLICT_DRAFT_HEADER_BYTES {
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    let payload = snapshot
        .with_bytes(0..CONFLICT_DRAFT_HEADER_BYTES, |header| {
            conflict_draft_payload_extent(header, snapshot.len())
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)??;
    if payload.start - CONFLICT_DRAFT_HEADER_BYTES != expected.len() {
        return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
    }
    let mut at = 0;
    while at < expected.len() {
        let end = at
            .saturating_add(crate::original_backing::ORIGINAL_READ_BYTES)
            .min(expected.len());
        let equal = snapshot
            .with_bytes(
                CONFLICT_DRAFT_HEADER_BYTES + at..CONFLICT_DRAFT_HEADER_BYTES + end,
                |bytes| bytes == &expected.as_bytes()[at..end],
            )
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        if !equal {
            return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
        }
        at = end;
    }
    Ok(payload)
}

const RESOLUTION_MAGIC: &[u8; 8] = b"OMNIMR1\0";
const RESOLUTION_HEADER_BYTES: usize = 89;
const RESOLUTION_CHECKSUM_BYTES: usize = 32;
const MAX_RESOLUTION_BYTES: usize = 16 * 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
/// marker的持久状态独立于已经核验的显式交接，失败不得锁死用户的Reload/SaveAsNew决定。
pub enum MarkdownRecoveryRecording {
    /// 本次决定或完全相同的既有receipt已确认持久化。
    Recorded,
    /// 同record已有另一完整合法决定，本次不覆盖它。
    AlreadyResolved,
    /// 决定未完整记录；原record与既有marker保留，普通重开可能再次提示恢复。
    NotRecorded(MarkdownWorkspaceError),
}

/// 旧editor仍活动时只准备小型ticket；不会发布marker或创建替代DocumentBuffer。
pub enum MarkdownRecoveryPreparation {
    /// 普通文档没有待解决的旧record，也不创建恢复存储。
    NotPending,
    /// 数据决定已经核验，调用方仍须在旧editor释放前复核绑定并通过原有strict barrier。
    Prepared(MarkdownRecoveryHandoff),
}

/// strict barrier成功后打开的唯一目标与独立记录事实；坏marker不会遮盖已验证的显式交接。
pub struct MarkdownRecoveryResolutionOutcome {
    /// 目标磁盘的普通模型；不包含旧editor/history或另一个长期owner。
    pub document: MarkdownDocumentBuffer,
    /// 记录失败必须单独提示，不能伪装成决定已经完整记录。
    pub recording: MarkdownRecoveryRecording,
}

/// 仅由已核验显式决定创建的线性ticket；不Clone、构造器私有，不持正文、FD或mapping。
pub struct MarkdownRecoveryHandoff {
    canonical_root: PathBuf,
    source_relative_path: String,
    record_digest: [u8; 32],
    source_revision: MarkdownDocumentRevision,
    target_relative_path: String,
    target_revision: [u8; 32],
    target_byte_count: u64,
    resolution_bytes: Vec<u8>,
}

impl MarkdownRecoveryHandoff {
    /// 返回本次唯一交接目标的相对标识符；它不是允许调用方替换目标的入口。
    pub fn target_relative_path(&self) -> &str {
        &self.target_relative_path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 底层不可变marker发布事实；与上层一次性数据交接分开。
enum MarkerPublication {
    /// 本次完整marker已经发布且目录同步完成。
    Written,
    /// 同一完整receipt已经存在，本次再次确认目录持久化。
    Idempotent,
    /// 同一record已有另一合法决定，本次不覆盖它。
    AlreadyResolved,
}

/// marker只包含有界元数据；revision/byte_count来自已验证的磁盘或已保存副本。
struct ResolutionReceipt<'a> {
    kind: u8,
    target_path: &'a str,
    revision: [u8; 32],
    byte_count: u64,
}

impl MarkdownSessionAccess {
    /// 精确查找一个旧record；marker无效不会抑制草稿，源bytes只在独立不可变snapshot中借用。
    pub(super) fn open_pending_legacy_recovery(
        &self,
        note_path: &Path,
        relative_path: &str,
    ) -> Result<Option<MarkdownDocumentBuffer>, MarkdownWorkspaceError> {
        validate_conflict_draft_relative_path(relative_path)?;
        if !existing_conflict_recovery_directory(&self.conflict_recovery_directory)? {
            // 正常无record的打开不创建目录、marker或任何长期恢复owner。
            return Ok(None);
        }
        let record_path = conflict_draft_path(&self.conflict_recovery_directory, relative_path);
        let metadata = match fs::symlink_metadata(&record_path) {
            // 精确键存在时再验证私有普通文件，不能因文件名正确就信任内容。
            Ok(metadata) => metadata,
            // 无该笔记record是普通磁盘打开，不扩大为目录扫描。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // 权限/I/O错误不能被伪装成没有待恢复的用户数据。
            Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
        };
        validate_conflict_draft_file(&metadata)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&record_path)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let before = file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        validate_conflict_draft_file(&before)?;
        let extent = usize::try_from(before.len())
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let mut header = [0_u8; CONFLICT_DRAFT_HEADER_BYTES];
        file.read_exact(&mut header)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        // 20B头只用于拒绝超限/坏长度；尚未借用可被外部截断的原inode。
        conflict_draft_payload_extent(&header, extent)?;
        let identity = FileIdentity::from(&before);
        let snapshot = snapshot_canonical(&file, before.len(), || {
            // 同步原生打开没有独立worker；snapshot自身仍以64KiB固定buffer处理跨卷fallback。
            true
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?
        .ok_or(MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        // snapshot完成后重新验证头和精确path；原头只能限预算，不能为不同snapshot授予解释权。
        let payload = bounded_conflict_payload_range(&snapshot, relative_path)?;
        let mut hasher = Sha256::new();
        let mut at = 0;
        while at < snapshot.len() {
            // record身份仍覆盖原header/path/payload全部精确字节，每片真实回收后才继续。
            let end = at
                .saturating_add(crate::original_backing::ORIGINAL_READ_BYTES)
                .min(snapshot.len());
            snapshot
                .with_bytes(at..end, |bytes| hasher.update(bytes))
                .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
            at = end;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        let after = file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        let path_after = fs::symlink_metadata(&record_path)
            .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
        if FileIdentity::from(&after) != identity
            || FileIdentity::from(&path_after) != identity
            || path_after.file_type().is_symlink()
        {
            // 源发生替换/修改时全部局部资源释放，不发布另一个版本的半模型。
            return Err(MarkdownWorkspaceError::ConflictDraftCorrupted);
        }
        drop(file);
        if read_resolution_marker(&self.conflict_recovery_directory, relative_path, &digest)
            .is_ok_and(|marker| marker.is_some())
        {
            // 完整内容身份已有合法决定；旧record保持原位，映射在本函数返回前释放。
            snapshot
                .close()
                .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)?;
            return Ok(None);
        }
        // 无marker或坏marker均保持pending；模型内部只借payload校验UTF-8和建立原有piece索引。
        MarkdownDocumentBuffer::from_legacy_recovery(
            note_path,
            snapshot,
            payload,
            digest,
            MAX_DOCUMENT_BYTES,
        )
        .map(Some)
        .map_err(|_| MarkdownWorkspaceError::ConflictDraftCorrupted)
    }

    /// 用户明确选择Reload后验证当前disk，只准备有界ticket，不发布marker或替代模型。
    ///
    /// # Errors
    /// 当前磁盘不可验证、路径越界或receipt预算不合法时返回错误并保留当前模型与所有文件。
    /// 调用方释放旧editor前必须再调用validate，并以原有resign前后generation检查拒绝晚到输入。
    pub fn prepare_legacy_recovery_after_reload(
        &self,
        document: &MarkdownDocumentBuffer,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let Some(digest) = document.legacy_recovery_record_digest() else {
            // 普通Reload不产生ticket、恢复元数据或存储目录。
            return Ok(MarkdownRecoveryPreparation::NotPending);
        };
        let relative_path = relative_markdown_path(&self.canonical_root, document.path())?;
        let path = resolve_existing_markdown(&self.canonical_root, &relative_path)?;
        let disk = inspect_markdown_save_baseline(&path)?;
        let receipt = ResolutionReceipt {
            kind: 1,
            target_path: "",
            revision: decode_revision(&disk.revision)?,
            byte_count: disk.byte_count,
        };
        // 这是已确认的当前disk，不是丢失的旧base；旧pending模型仍禁止普通保存。
        self.prepare_recovery_handoff(document, &relative_path, &digest, &relative_path, &receipt)
    }

    /// 核验已保存副本与当前live pieces，只准备ticket；旧editor释放失败时丢弃ticket无marker副作用。
    ///
    /// # Errors
    /// receipt与当前文档或实际副本不一致、路径越界或副本同步失败时返回错误并保留当前模型。
    /// 即使同record已有合法marker，也先证明本次live草稿确有已持久化副本；不授予原文件覆盖权限。
    pub fn prepare_legacy_recovery_after_save_as_new(
        &self,
        document: &MarkdownDocumentBuffer,
        saved: &MarkdownNoteMutationReceipt,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let Some(digest) = document.legacy_recovery_record_digest() else {
            // 普通另存保持既有API，不增加恢复ticket或marker。
            return Ok(MarkdownRecoveryPreparation::NotPending);
        };
        let relative_path = relative_markdown_path(&self.canonical_root, document.path())?;
        validate_conflict_draft_relative_path(&saved.relative_path)?;
        let target = resolve_existing_markdown(&self.canonical_root, &saved.relative_path)?;
        let actual = inspect_markdown_save_baseline(&target)?;
        if target == document.path()
            || actual.revision != saved.revision
            || actual.byte_count != saved.byte_count
            || saved.revision
                != document
                    .current_revision()
                    .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?
                    .encode_hex()
            || saved.byte_count != document.byte_len() as u64
        {
            // 现有但不含当前live pieces的文件不能证明草稿另存成功，不创建交接ticket。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        // 原SaveAsNew receipt已在file/dir fsync后返回；再同步核验目标，不能由伪造receipt借用未持久文件。
        let target_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&target)
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let target_before = target_file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        if !target_before.is_file() {
            // 路径验证后若被换成FIFO/device，也不能阻塞打开或把特殊FD当作可持久副本。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let synced_identity = FileIdentity::from(&target_before);
        target_file
            .sync_all()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let parent = target
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        sync_directory(parent)?;
        let rechecked = inspect_markdown_save_baseline(&target)?;
        let target_after = target_file
            .metadata()
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        let path_after = fs::symlink_metadata(&target)
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?;
        if rechecked.revision != actual.revision
            || rechecked.byte_count != actual.byte_count
            || FileIdentity::from(&target_after) != synced_identity
            || FileIdentity::from(&path_after) != synced_identity
            || path_after.file_type().is_symlink()
        {
            // 内容相同的新inode也未必是刚fsync的FD；同步期间观察到替换/修改时不授予ticket。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let receipt = ResolutionReceipt {
            kind: 2,
            target_path: &saved.relative_path,
            revision: decode_revision(&saved.revision)?,
            byte_count: saved.byte_count,
        };
        self.prepare_recovery_handoff(
            document,
            &relative_path,
            &digest,
            &saved.relative_path,
            &receipt,
        )
    }

    /// 在旧pending仍活时纯核验ticket绑定；不读取文件、不创建mapping、不发布marker。
    ///
    /// # Errors
    /// 会话root、源path、record内容身份或live revision不匹配时返回错误，调用方保留原editor与历史。
    /// 成功不代替原生resign前后generation检查；检查后出现输入必须拒绝释放并丢弃ticket。
    pub fn validate_legacy_recovery_handoff(
        &self,
        document: &MarkdownDocumentBuffer,
        handoff: &MarkdownRecoveryHandoff,
    ) -> Result<(), MarkdownWorkspaceError> {
        if self.canonical_root != handoff.canonical_root
            || document.path().strip_prefix(&handoff.canonical_root).ok()
                != Some(Path::new(&handoff.source_relative_path))
            || document.legacy_recovery_record_digest() != Some(handoff.record_digest)
            || document
                .current_revision()
                .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?
                != handoff.source_revision
        {
            // 后续输入、换库或另一record模型没有复用旧决定的权限，所有持久文件继续保持。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        Ok(())
    }

    /// 仅在旧editor strict barrier成功后消费ticket，先打开唯一目标，再尝试发布marker。
    ///
    /// # Errors
    /// root或目标版本不匹配、目标无法安全读取时返回错误且不发布marker；可通过普通打开再次恢复旧record。
    /// marker错误只产生NotRecorded并保留完整文件，返回的目标模型仍可交接。调用方必须准确显示警告，
    /// 不得将该入口当作通用忽略恢复开关，也不能在旧editor释放失败时调用它。
    pub fn open_resolved_legacy_recovery(
        &self,
        handoff: MarkdownRecoveryHandoff,
    ) -> Result<MarkdownRecoveryResolutionOutcome, MarkdownWorkspaceError> {
        self.open_recovery_handoff_with_sync(handoff, sync_conflict_recovery_directory)
    }

    /// 持久化seam只在真实目标打开与完整marker发布后执行，便于纯文件系统故障回归。
    fn open_recovery_handoff_with_sync(
        &self,
        handoff: MarkdownRecoveryHandoff,
        synchronize: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownRecoveryResolutionOutcome, MarkdownWorkspaceError> {
        if self.canonical_root != handoff.canonical_root {
            // ticket不能被另一vault消费；不触及目标或marker。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let target =
            resolve_existing_markdown(&self.canonical_root, &handoff.target_relative_path)?;
        // 旧editor已经释放才构建替代模型；普通record-aware open仍无bypass参数。
        let document = MarkdownDocumentBuffer::open(&target, MAX_DOCUMENT_BYTES)
            .map_err(markdown_document_buffer_open_error)?;
        let revision = document.original_revision().and_then(|revision| {
            // 只转换固定64B摘要表示，不复制正文；正常磁盘模型必须具有真实baseline。
            decode_revision(&revision.encode_hex()).ok()
        });
        if revision != Some(handoff.target_revision)
            || document.byte_len() as u64 != handoff.target_byte_count
        {
            // 目标变化时释放局部mapping，不发布marker；旧record与已保存副本均不删除。
            return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
        }
        let recording = match publish_resolution_marker(
            &self.conflict_recovery_directory,
            &handoff.source_relative_path,
            &handoff.record_digest,
            &handoff.resolution_bytes,
            &self.save_sequence,
            synchronize,
        ) {
            // 新发布与相同receipt均已确认目录持久化，界面共享同一准确记录事实。
            Ok(MarkerPublication::Written | MarkerPublication::Idempotent) => {
                MarkdownRecoveryRecording::Recorded
            }
            // 另一完整合法决定保持原样，不伪装本次receipt覆盖了它。
            Ok(MarkerPublication::AlreadyResolved) => MarkdownRecoveryRecording::AlreadyResolved,
            // 包含坏marker与post-rename同步不确定，保留原始typed原因与所有文件。
            Err(error) => MarkdownRecoveryRecording::NotRecorded(error),
        };
        Ok(MarkdownRecoveryResolutionOutcome {
            document,
            recording,
        })
    }

    /// 只保存有界receipt元数据；无正文/FD/mapping，丢弃ticket不会产生磁盘副作用。
    fn prepare_recovery_handoff(
        &self,
        document: &MarkdownDocumentBuffer,
        relative_path: &str,
        digest: &[u8; 32],
        target_relative_path: &str,
        receipt: &ResolutionReceipt<'_>,
    ) -> Result<MarkdownRecoveryPreparation, MarkdownWorkspaceError> {
        let resolution_bytes = encode_resolution(relative_path, digest, receipt)?;
        Ok(MarkdownRecoveryPreparation::Prepared(
            MarkdownRecoveryHandoff {
                canonical_root: self.canonical_root.clone(),
                source_relative_path: relative_path.to_owned(),
                record_digest: *digest,
                source_revision: document
                    .current_revision()
                    .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionUnverified)?,
                target_relative_path: target_relative_path.to_owned(),
                target_revision: receipt.revision,
                target_byte_count: receipt.byte_count,
                resolution_bytes,
            },
        ))
    }
}

/// 有界64字符小写revision只转为固定摘要，不接受任意长度或模糊编码。
fn decode_revision(revision: &str) -> Result<[u8; 32], MarkdownWorkspaceError> {
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        // 外来receipt必须与本项目canonical revision表示一致。
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in revision.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        // 长度与ASCII域已验证；每对只组成一个固定byte，不分配临时字符串。
        let nibble = |byte: u8| {
            if byte <= b'9' {
                // 已验证ASCII十进制数字直接映射到低半区。
                byte - b'0'
            } else {
                // 其余仅可能是已验证的小写a到f，不接受大写或任意编码。
                byte - b'a' + 10
            }
        };
        digest[index] = nibble(pair[0]) * 16 + nibble(pair[1]);
    }
    Ok(digest)
}

/// 路径与完整record内容共同形成独立命名域，不将后来的不同record误认作旧决定。
fn marker_path(directory: &Path, relative_path: &str, digest: &[u8; 32]) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"omni-md-recovery-resolution/v1\0");
    hasher.update((relative_path.len() as u64).to_be_bytes());
    hasher.update(relative_path.as_bytes());
    hasher.update(digest);
    let key = hasher.finalize();
    let mut name = String::with_capacity(64 + ".resolved".len());
    for byte in key {
        // 固定ASCII hex不包含目录分隔符，relative path不能注入marker文件名。
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".resolved");
    directory.join(name)
}

/// canonical marker完全有界，内容校验独立于文件名；不包含正文bytes。
fn encode_resolution(
    relative_path: &str,
    digest: &[u8; 32],
    receipt: &ResolutionReceipt<'_>,
) -> Result<Vec<u8>, MarkdownWorkspaceError> {
    validate_conflict_draft_relative_path(relative_path)?;
    if receipt.kind == 2 {
        // SaveAsNew的目标也必须是合法且有界的vault笔记标识符。
        validate_conflict_draft_relative_path(receipt.target_path)?;
    } else if receipt.kind != 1 || !receipt.target_path.is_empty() {
        // Reload不携带另一个目标；未知kind不能被编码成某种可接受决定。
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let length = RESOLUTION_HEADER_BYTES
        + relative_path.len()
        + receipt.target_path.len()
        + RESOLUTION_CHECKSUM_BYTES;
    if length > MAX_RESOLUTION_BYTES || receipt.byte_count > MAX_DOCUMENT_BYTES {
        // 元数据与字节数使用明确预算，不能依赖Vec增长或截断来保存决定。
        return Err(MarkdownWorkspaceError::RecoveryResolutionUnverified);
    }
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(RESOLUTION_MAGIC);
    bytes.extend_from_slice(&(relative_path.len() as u32).to_be_bytes());
    bytes.extend_from_slice(digest);
    bytes.push(receipt.kind);
    bytes.extend_from_slice(&(receipt.target_path.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&receipt.revision);
    bytes.extend_from_slice(&receipt.byte_count.to_be_bytes());
    bytes.extend_from_slice(relative_path.as_bytes());
    bytes.extend_from_slice(receipt.target_path.as_bytes());
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

/// 任何坏格式、错误binding或尾字节都不能抑制旧record；仅合法完整元数据有解决权限。
fn validate_resolution(
    bytes: &[u8],
    relative_path: &str,
    digest: &[u8; 32],
) -> Result<(), MarkdownWorkspaceError> {
    let corrupt = MarkdownWorkspaceError::RecoveryResolutionCorrupted;
    if bytes.len() < RESOLUTION_HEADER_BYTES + RESOLUTION_CHECKSUM_BYTES
        || bytes.len() > MAX_RESOLUTION_BYTES
        || &bytes[..8] != RESOLUTION_MAGIC
    {
        // 先检查最小固定头，后面的固定offset才可以被安全读取。
        return Err(corrupt);
    }
    let mut original_length = [0; 4];
    original_length.copy_from_slice(&bytes[8..12]);
    let original_length = u32::from_be_bytes(original_length) as usize;
    let mut target_length = [0; 4];
    target_length.copy_from_slice(&bytes[45..49]);
    let target_length = u32::from_be_bytes(target_length) as usize;
    let mut byte_count = [0; 8];
    byte_count.copy_from_slice(&bytes[81..89]);
    if original_length > MAX_RECOVERY_RELATIVE_PATH_BYTES
        || target_length > MAX_RECOVERY_RELATIVE_PATH_BYTES
        || u64::from_be_bytes(byte_count) > MAX_DOCUMENT_BYTES
        || RESOLUTION_HEADER_BYTES + original_length + target_length + RESOLUTION_CHECKSUM_BYTES
            != bytes.len()
    {
        // 两个path上限使算术有界；整文件必须恰好覆盖固定头、两个path与checksum。
        return Err(corrupt);
    }
    let original_end = RESOLUTION_HEADER_BYTES + original_length;
    let target_end = original_end + target_length;
    if &bytes[12..44] != digest
        || &bytes[RESOLUTION_HEADER_BYTES..original_end] != relative_path.as_bytes()
        || bytes[target_end..] != Sha256::digest(&bytes[..target_end])[..]
    {
        // hash文件名无法代替完整内容binding与校验；错误决定保留为不可信文件，不覆盖。
        return Err(corrupt);
    }
    match bytes[44] {
        // Reload只允许空目标，revision/bytes是选择当时已核验的磁盘元数据。
        1 if target_length == 0 => Ok(()),
        2 => {
            // SaveAsNew目标须UTF-8且confined的相对笔记语法；解析不打开或改写该路径。
            let target = std::str::from_utf8(&bytes[original_end..target_end])
                .map_err(|_| corrupt.clone())?;
            validate_conflict_draft_relative_path(target).map_err(|_| corrupt)
        }
        // 未知版本/种类不能被降级为Reload或“已经解决”。
        _ => Err(corrupt),
    }
}

/// 读取最多16KiB完整marker；同用户修改/替换被identity复核捕获，读失败不授予解决权限。
fn read_resolution_marker(
    directory: &Path,
    relative_path: &str,
    digest: &[u8; 32],
) -> Result<Option<Vec<u8>>, MarkdownWorkspaceError> {
    let path = marker_path(directory, relative_path, digest);
    let metadata = match fs::symlink_metadata(&path) {
        // 路径存在还不代表是安全私有marker，下面验证真实FD和完整内容。
        Ok(metadata) => metadata,
        // 缺失是正常pending状态。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // lookup错误不能升级为一个合法决定。
        Err(_) => return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted),
    };
    validate_conflict_draft_file(&metadata)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let before = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    validate_conflict_draft_file(&before)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    if before.len() > MAX_RESOLUTION_BYTES as u64 {
        // 超限文件不按其声明分配内存。
        return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted);
    }
    // 固定长度buffer加一个growth sentinel，读取中变大也不会触发Vec自动倍增容量。
    let mut bytes = vec![0_u8; MAX_RESOLUTION_BYTES + 1];
    let mut filled = 0;
    while filled < bytes.len() {
        // 每次正读取都推进有限端点；普通文件EOF结束，最多读取16KiB加一个拒绝用sentinel。
        let count = file
            .read(&mut bytes[filled..])
            .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
        if count == 0 {
            // 不把EOF补零伪装为marker内容，只提交本次真正读到的前缀给完整格式验证。
            break;
        }
        filled += count;
    }
    bytes.truncate(filled);
    let after = file
        .metadata()
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    let path_after = fs::symlink_metadata(&path)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
    if FileIdentity::from(&before) != FileIdentity::from(&after)
        || FileIdentity::from(&before) != FileIdentity::from(&path_after)
        || path_after.file_type().is_symlink()
    {
        // 不接受读取中变换的marker；旧record仍会作为pending显示。
        return Err(MarkdownWorkspaceError::RecoveryResolutionCorrupted);
    }
    validate_resolution(&bytes, relative_path, digest)?;
    Ok(Some(bytes))
}

/// 完整临时文件先fsync，再exclusive发布；最终名永不部分可见，发布后失败绝不rollback。
fn publish_resolution_marker(
    directory: &Path,
    relative_path: &str,
    digest: &[u8; 32],
    bytes: &[u8],
    sequence: &AtomicU64,
    synchronize: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
) -> Result<MarkerPublication, MarkdownWorkspaceError> {
    ensure_conflict_recovery_directory(directory)?;
    let mut temporary = create_temporary_conflict_draft(directory, sequence)?;
    temporary
        .file
        .write_all(bytes)
        .and_then(|()| {
            // 只有完整小marker已经写入并同步，才有资格成为最终可见决定。
            temporary.file.sync_all()
        })
        .map_err(|_| MarkdownWorkspaceError::ConflictRecoveryUnavailable)?;
    let destination = marker_path(directory, relative_path, digest);
    let outcome = match rename_without_replacement(&temporary.path, &destination) {
        Ok(()) => {
            // 先撤销temp rollback，后续dirsync失败仍必须保留已经发布的marker。
            temporary.disarm();
            MarkerPublication::Written
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_resolution_marker(directory, relative_path, digest)?
                .ok_or(MarkdownWorkspaceError::RecoveryResolutionCorrupted)?;
            if existing == bytes {
                // 同一完整receipt是幂等；仍在下面重新确认目录同步，覆盖早先uncertain提交。
                MarkerPublication::Idempotent
            } else {
                // 同record已有另一完整合法决定，不把它当作corrupt或覆盖；新草稿副本已在caller核验。
                MarkerPublication::AlreadyResolved
            }
        }
        // 非碰撞的发布错误仍由本次temp guard撤销自己的未发布文件，不触碰record或他人marker。
        Err(_) => return Err(MarkdownWorkspaceError::ConflictRecoveryUnavailable),
    };
    synchronize(directory)
        .map_err(|_| MarkdownWorkspaceError::RecoveryResolutionDurabilityUncertain)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests;
