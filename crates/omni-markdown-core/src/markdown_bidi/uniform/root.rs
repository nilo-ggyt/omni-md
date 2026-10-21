//! Reuse exact UniformZero proof from the complete original frozen root. Incomplete scopes
//! or ineligible properties decline this path without scanning text.

use super::*;

/// The caller owns the original preparation's Source/Gate checks around this function;
/// this path consumes only properties already maintained for the complete frozen root.
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
    // Use the original UBA-root whitelist. Paragraph separators restart a base that is
    // still zero; this proof grants no hidden-region GCB or script authority.
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
    // Exact whole-root aggregation can retain UniformZero without scanning the giant
    // hidden body or creating an otherwise unnecessary UBA backing file.
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
        // A one-unit quantum can finish from existing complete-root proof with zero
        // source reads; this is distinct from treating an unfinished scan as Ready.
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
