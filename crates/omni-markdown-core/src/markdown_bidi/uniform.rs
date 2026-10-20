//! 单个LTR段的完整证书；细byte/scalar预算暂停不被解释成真实EOF。
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
        // 折叠和普通 Prepare 共用同一原 root 白名单；原 Source/Gate 前后门仍覆盖这个零正文快路。
        snapshot.original_backing_health()?;
        gate.health()?;
        return Ok(Some(certificate));
    }
    // 未证明的Unicode类、含B正文和部分范围保持原预算Reader。尤其不对混合叶的局部范围
    // 新发ASCII查询，否则该查询本身可能读取边叶而绕过本轮byte/scalar配额。
    certify_scanned(snapshot, source, bytes, gate).await
}

async fn certify_scanned(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: Range<usize>,
    gate: &Rc<Gate>,
) -> Result<Option<UniformCertificate>, BidiError> {
    // 原扫描器作为未证明路径与独立差分基线保留；它不借整root摘要跳过任何正文。
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
            return Ok(None); // 仅撤快速证书，原全UBA仍在同一Future/Source继续处理合法正文。
        }
        u32::try_from(byte).map_err(|_| BidiError::CoordinateOverflow)?;
        certificate.observe(scalar, class); // 共用本次真实解码和类别，不另借正文或再查Unicode表。
        terminal_b = class == BidiClass::B;
        units += scalar.len_utf16();
    }
    snapshot.original_backing_health()?;
    gate.health()?;
    if units != source.len() || reader.position() != bytes.end {
        return Err(BidiError::CorruptMetadata);
    }
    // 只有真实完整范围与原白名单规则才得到UniformZero；不发布尚未确定的段落方向。
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
