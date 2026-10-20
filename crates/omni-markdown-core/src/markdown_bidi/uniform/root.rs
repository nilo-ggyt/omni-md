//! 仅复用原完整冻结 root 的精确 UniformZero 证书；范围不完整或属性不满足时不扫描正文。

use super::*;

/// 原 prepare 的同一 Source/Gate 前后门由调用者持有；这里只消费完整 root 已维护的属性。
pub(in crate::markdown_bidi) fn certify(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: &Range<usize>,
    base: BidiBaseDirection,
) -> Result<Option<UniformCertificate>, BidiError> {
    if base == BidiBaseDirection::RightToLeft
        || source.start != 0
        || source.end != snapshot.utf16_len()
        || bytes.start != 0
        || bytes.end != snapshot.byte_len()
    {
        return Ok(None);
    }
    if snapshot.byte_len() == snapshot.utf16_len()
        && snapshot.is_printable_ascii_range(source.clone())?
    {
        u32::try_from(bytes.end).map_err(|_| BidiError::CoordinateOverflow)?;
        return Ok(Some(UniformCertificate::clear()));
    }
    // 白名单完全沿原 UBA root 快路；段落 B 只重启仍为零的 base，不签隐区 GCB 或脚本事实。
    let properties = snapshot.unicode_properties()?;
    let eligible = [
        BidiClass::L,
        BidiClass::EN,
        BidiClass::ES,
        BidiClass::ET,
        BidiClass::CS,
        BidiClass::ON,
        BidiClass::WS,
        BidiClass::BN,
        BidiClass::NSM,
        BidiClass::S,
        BidiClass::B,
    ]
    .into_iter()
    .fold(0, |mask, class| {
        mask | crate::markdown_document_buffer::MarkdownUnicodeProperties::class_mask(class)
    });
    if properties.bidi_classes() & !eligible != 0 {
        return Ok(None);
    }
    u32::try_from(bytes.end).map_err(|_| BidiError::CoordinateOverflow)?;
    Ok(Some(UniformCertificate::from_properties(properties)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_layout_source::folds::FoldPlan;
    use crate::{MarkdownDocumentBuffer, MarkdownLayoutTextSource};

    #[test]
    // 真完整 root 聚合可以保原 UniformZero；巨隐区既不扫正文，也不建立多余 UBA 文件。
    fn folded_ltr_root_reuses_original_exact_zero_certificate_without_hidden_source_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("folded-zero.md");
        let header = "# H\r\n";
        let body = "中🙂e\u{301}".repeat(65_536);
        let text = format!("{header}{body}\r\nnext word");
        std::fs::write(&path, text.as_bytes()).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let end = header.len() + body.encode_utf16().count() + 2;
        let plan = FoldPlan::new(&source, std::slice::from_ref(&(3..end))).unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (job, receipt) = MarkdownBidiPrepareJob::new_layout_source_folded_observed(
            source,
            &authority,
            plan,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65_536).unwrap(),
            &cancel,
        );
        let mut job = job.unwrap();
        let progress = job
            .step(
                &authority,
                MarkdownBidiPrepareBudget {
                    bytes: 1,
                    scalars: 1,
                    metadata_ops: 1,
                },
            )
            .unwrap();
        assert_eq!(progress.status, MarkdownBidiPrepareStatus::Ready);
        assert_eq!((progress.bytes, progress.scalars), (0, 0));
        let paragraph = job.take_ready(&authority).unwrap().unwrap();
        assert!(paragraph.whole_source_uniform_zero(&authority).unwrap());
        assert!(matches!(paragraph.resolution, Resolution::UniformZero(_)));
        drop(paragraph);
        drop(job);
        assert!(receipt.is_released());
        assert!(!receipt.snapshot().file_created);
    }
}
