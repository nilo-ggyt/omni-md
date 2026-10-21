//! Certify uniform LTR resolution for the original prepared scope. Byte/scalar quantum
//! suspension never substitutes for actual completion of that scope.
use super::*;
use prepare::{cooperate::Gate, source::Reader};
use std::rc::Rc;
mod certificate;
#[cfg(feature = "paged-shaping-frontier")]
mod paragraph;
#[cfg(feature = "paged-shaping-frontier")]
pub(super) mod root;
pub(in crate::markdown_bidi) use certificate::{UniformCertificate, UniformResolution};
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use paragraph::ParagraphPrepareStep;

pub(super) async fn certify_prepared(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: Range<usize>,
    base: BidiBaseDirection,
    gate: &Rc<Gate>,
) -> Result<Option<UniformCertificate>, BidiError> {
    snapshot.original_backing_health()?;
    gate.health()?;
    if base == BidiBaseDirection::RightToLeft {
        return Ok(None);
    }
    #[cfg(feature = "paged-shaping-frontier")]
    if let Some(certificate) = root::certify(snapshot, source, &bytes, base)? {
        // Folded and ordinary preparation share the exact original-root whitelist.
        // Source/Gate checks still surround this path even though it reads no text bytes.
        snapshot.original_backing_health()?;
        gate.health()?;
        return Ok(Some(certificate));
    }
    // Unproven classes and partial scopes retain the original budgeted Reader. Do not
    // issue a new ASCII query on a mixed leaf's partial range: that query could itself
    // read edge text outside this quantum's byte/scalar accounting.
    certify_scanned(snapshot, source, bytes, gate).await
}

async fn certify_scanned(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: Range<usize>,
    gate: &Rc<Gate>,
) -> Result<Option<UniformCertificate>, BidiError> {
    // The original scanner remains both the unproven path and an independent differential
    // baseline; it skips no text through a whole-root aggregate.
    snapshot.original_backing_health()?;
    gate.health()?;
    let mut reader = Reader::new(snapshot, bytes.clone());
    let mut units = 0usize;
    let mut terminal_b = false;
    let mut certificate = UniformCertificate::clear();
    while let Some((scalar, byte)) = reader.next(gate).await? {
        let class = UnicodeBidiData.bidi_class(scalar);
        if terminal_b || !eligible_class(class) {
            snapshot.original_backing_health()?;
            gate.health()?;
            return Ok(None); // Decline only this certificate; full UBA still handles valid text in the same Future/Source.
        }
        u32::try_from(byte).map_err(|_| BidiError::CoordinateOverflow)?;
        certificate.observe(scalar, class); // Reuse the actual decoded scalar/class without another borrow or property lookup.
        terminal_b = class == BidiClass::B;
        units += scalar.len_utf16();
    }
    snapshot.original_backing_health()?;
    gate.health()?;
    if units != source.len() || reader.position() != bytes.end {
        return Err(BidiError::CorruptMetadata);
    }
    // Grant UniformZero only after the actual complete range and original whitelist
    // agree; an unfinished paragraph cannot publish a guessed base direction.
    Ok(Some(certificate))
}

pub(super) fn eligible_class(class: BidiClass) -> bool {
    use BidiClass::*;
    matches!(class, L | EN | ES | ET | CS | ON | WS | BN | NSM | S | B)
}

#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod control_tests;
#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod multiparagraph_tests;
#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod printable_tests;
#[cfg(all(test, feature = "paged-shaping-frontier"))]
mod property_tests;
