//! 单个LTR段的完整证书；细byte/scalar预算暂停不被解释成真实EOF。
use super::*;
use prepare::{cooperate::Gate, source::Reader};
use std::rc::Rc;
mod certificate;
#[cfg(feature = "paged-shaping-frontier")]
mod paragraph;
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
    // 只消费整Snapshot根的精确聚合。先核两套完整端点，部分范围不进入可能借边叶的
    // ASCII查询；非RTL与等长条件保留原UniformZero所需的方向和坐标不变量。
    #[cfg(feature = "paged-shaping-frontier")]
    if source.start == 0
        && source.end == snapshot.utf16_len()
        && bytes.start == 0
        && bytes.end == snapshot.byte_len()
        && snapshot.byte_len() == snapshot.utf16_len()
        && snapshot.is_printable_ascii_range(source.clone())?
    {
        // 原UTF-8打开校验和每次真实splice已维护同一冻结root的精确聚合。完整
        // U+0020..U+007E域没有RTL/显式控制/段落B，Auto或LTR下UBA全部为level0；
        // 这里消费原root事实，不抽样、不按generation推断，也不再次借整源正文。
        u32::try_from(bytes.end).map_err(|_| BidiError::CoordinateOverflow)?;
        snapshot.original_backing_health()?;
        gate.health()?;
        return Ok(Some(UniformCertificate::clear()));
    }
    #[cfg(feature = "paged-shaping-frontier")]
    if source.start == 0
        && source.end == snapshot.utf16_len()
        && bytes.start == 0
        && bytes.end == snapshot.byte_len()
    {
        // 只由UBA消费同一完整冻结root的通用属性。B只重启仍为0的段base；
        // 它的存在性不授予任何段端点，多段随后由同root有界游标按需定位。
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
        if properties.bidi_classes() & !eligible == 0 {
            // 坐标域仍取真实UTF-8范围；Unicode的byte/UTF16不等长不影响完整范围证明。
            // 摘要不授予部分段落或GCB边界，控制事实也必须保留原六字符的精确存在性。
            u32::try_from(bytes.end).map_err(|_| BidiError::CoordinateOverflow)?;
            snapshot.original_backing_health()?;
            gate.health()?;
            return Ok(Some(UniformCertificate::from_properties(properties)));
        }
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
        certificate.observe(scalar); // 只附记本次原Reader已实际解码的scalar，不另借正文。
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
