//! 只读有限表目录、名称和fvar；不读取glyf正文或构造第二份字体字节。
use std::sync::Arc;

use harfrust::Font;
use read_fonts::{
    TableProvider,
    types::{NameId, Tag},
};

use super::{
    MarkdownLayoutFontAxis as Axis, MarkdownLayoutFontError as Error,
    MarkdownLayoutFontMetadata as Metadata, Result,
};

const MAX_FACES: usize = 256;
const MAX_TABLES: usize = 256;
const MAX_AXES: usize = 32;
const MAX_INSTANCES: usize = 128;
const MAX_NAME_RECORDS: usize = 4096;
const MAX_POSTSCRIPT_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 1024 * 1024;
const MAX_NAME_RECORD_VISITS: usize = 1_048_576;
const MAX_DECODED_NAME_SCALARS: usize = 1_048_576;

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = offset.checked_add(2).ok_or(Error::MetadataOverflow)?;
    let raw = bytes.get(offset..end).ok_or(Error::InvalidContainer)?;
    Ok(u16::from_be_bytes(raw.try_into().expect("已核固定2B")))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or(Error::MetadataOverflow)?;
    let raw = bytes.get(offset..end).ok_or(Error::InvalidContainer)?;
    Ok(u32::from_be_bytes(raw.try_into().expect("已核固定4B")))
}

/// 每个face只留几个目录事实；名称迭代前先限制真实record数，不靠take隐藏未知尾部。
struct Directory {
    name_records: usize,
    has_fvar: bool,
}
fn directory(bytes: &[u8], base: usize) -> Result<Directory> {
    let version_end = base.checked_add(4).ok_or(Error::MetadataOverflow)?;
    if !matches!(
        bytes.get(base..version_end),
        Some(b"\0\x01\0\0" | b"OTTO" | b"true")
    ) {
        return Err(Error::InvalidContainer); // 非SFNT格式没有可证明的同一集合face，不能交给宽容回退。
    }
    let count = usize::from(u16_at(bytes, base + 4)?);
    if count == 0 || count > MAX_TABLES {
        return Err(Error::InvalidDirectory);
    }
    let start = base.checked_add(12).ok_or(Error::MetadataOverflow)?;
    let end = start
        .checked_add(count.checked_mul(16).ok_or(Error::MetadataOverflow)?)
        .ok_or(Error::MetadataOverflow)?;
    let records = bytes.get(start..end).ok_or(Error::InvalidDirectory)?;
    let mut previous = None;
    let mut name_records = None;
    let mut has_fvar = false;
    for record in records.as_chunks::<16>().0 {
        let tag: [u8; 4] = record[..4].try_into().expect("固定tag");
        if previous.is_some_and(|old| old >= tag) {
            return Err(Error::InvalidDirectory); // 重复或无序tag不能由不同parser选择不同的同名表。
        }
        previous = Some(tag);
        let offset = usize::try_from(u32_at(record, 8)?).map_err(|_| Error::MetadataOverflow)?;
        let size = usize::try_from(u32_at(record, 12)?).map_err(|_| Error::MetadataOverflow)?;
        let end = offset.checked_add(size).ok_or(Error::MetadataOverflow)?;
        let table = bytes.get(offset..end).ok_or(Error::InvalidDirectory)?;
        if tag == *b"name" {
            if size > MAX_NAME_BYTES || table.len() < 6 {
                return Err(Error::NameBudgetExceeded);
            }
            let version = u16_at(table, 0)?;
            let count = usize::from(u16_at(table, 2)?);
            let record_end = 6_usize
                .checked_add(count.checked_mul(12).ok_or(Error::MetadataOverflow)?)
                .ok_or(Error::MetadataOverflow)?;
            let string_start = usize::from(u16_at(table, 4)?);
            if count > MAX_NAME_RECORDS {
                return Err(Error::NameBudgetExceeded);
            }
            if version > 1 || record_end > table.len() || string_start < record_end {
                return Err(Error::InvalidNameTable);
            }
            if version == 1 {
                let languages = usize::from(u16_at(table, record_end)?);
                let language_end = record_end
                    .checked_add(2)
                    .and_then(|start| start.checked_add(languages.checked_mul(4)?))
                    .ok_or(Error::MetadataOverflow)?;
                if languages > MAX_NAME_RECORDS {
                    return Err(Error::NameBudgetExceeded);
                }
                if language_end > table.len() || string_start < language_end {
                    return Err(Error::InvalidNameTable);
                }
            }
            if string_start > table.len() {
                return Err(Error::InvalidNameTable);
            }
            name_records = Some(count);
        }
        has_fvar |= tag == *b"fvar";
    }
    Ok(Directory {
        name_records: name_records.ok_or(Error::InvalidNameTable)?,
        has_fvar,
    })
}

struct NameBudget {
    records: usize,
    scalars: usize,
}
impl NameBudget {
    fn matches(
        &mut self,
        font: &Font,
        directory: &Directory,
        id: NameId,
        target: &str,
    ) -> Result<bool> {
        self.records = self
            .records
            .checked_sub(directory.name_records)
            .ok_or(Error::NameBudgetExceeded)?;
        for name in font.names().get(id).iter() {
            let mut wanted = target.chars();
            let mut matches = true;
            for (index, scalar) in name.name().chars().enumerate() {
                self.scalars = self
                    .scalars
                    .checked_sub(1)
                    .ok_or(Error::NameBudgetExceeded)?;
                if index >= MAX_POSTSCRIPT_BYTES {
                    return Err(Error::NameBudgetExceeded); // 超长同前缀不能被截成看似精确命中。
                }
                if wanted.next() != Some(scalar) {
                    matches = false;
                    break; // 这一名称已知不同；不读取无关长后缀或保存它的String。
                }
            }
            if matches && wanted.next().is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

struct Candidate {
    face: u32,
    font: Font,
    named: Option<(u16, Vec<Axis>)>,
    has_fvar: bool,
}

/// 完整Native名称表必须逐字节属于唯一真实face；相同名称/字形数量不能代替这个来源核验。
pub(super) fn resolve_name_table(
    bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    native_name_table: &[u8],
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    if native_name_table.is_empty() || native_name_table.len() > MAX_NAME_BYTES {
        return Err(Error::InvalidNameTable);
    }
    if axes.len() > MAX_AXES {
        return Err(Error::VariationBudgetExceeded);
    }
    let raw: &[u8] = bytes.as_ref().as_ref();
    let collection = raw.get(..4) == Some(b"ttcf");
    let count = if collection {
        if !matches!(u32_at(raw, 4)?, 0x0001_0000 | 0x0002_0000) {
            return Err(Error::InvalidContainer);
        }
        usize::try_from(u32_at(raw, 8)?).map_err(|_| Error::MetadataOverflow)?
    } else {
        1
    }; // 单文件候选仍须走同样严格的SFNT目录验证，不能直接假定face0匹配。
    if count == 0 || count > MAX_FACES {
        return Err(Error::FaceBudgetExceeded);
    }
    let mut matched = None;
    for face in 0..count {
        let base = if collection {
            usize::try_from(u32_at(raw, 12 + face * 4)?).map_err(|_| Error::MetadataOverflow)?
        } else {
            0
        };
        let directory = directory(raw, base)?;
        let font = Font::new(Arc::clone(&bytes), face as u32).ok_or(Error::InvalidFace)?;
        if !read_fonts::FontRef::from_index(raw, face as u32)
            .map_err(|_| Error::InvalidFace)?
            .table_data(Tag::new(b"name"))
            .is_some_and(|table| table.as_bytes() == native_name_table)
        {
            continue;
        } // 只比较完整原表，不把前缀、hash碰巧相同或Native显示名当face匹配。
        if matched.is_some() {
            return Err(Error::AmbiguousFace);
        } // 多face共享完全相同name表时没有足够身份信息，不能用遍历顺序挑一个。
        let mut postscript = None;
        for record in font.names().get(NameId::POSTSCRIPT_NAME).iter() {
            let mut candidate = String::new();
            for scalar in record.name().chars() {
                if !scalar.is_ascii()
                    || scalar.is_ascii_control()
                    || candidate.len() >= MAX_POSTSCRIPT_BYTES
                {
                    return Err(Error::InvalidPostScriptName);
                }
                candidate.push(scalar);
            } // 只解码原表内这个有界名称；不读取正文、字形轮廓或Native合成PSName。
            if !candidate.is_empty() {
                postscript = Some(candidate);
                break;
            }
        }
        matched = Some((
            Candidate {
                face: face as u32,
                font,
                named: None,
                has_fvar: directory.has_fvar,
            },
            postscript.ok_or(Error::PostScriptNotFound)?,
        ));
    }
    let (matched, postscript) = matched.ok_or(Error::PostScriptNotFound)?;
    // face已经由完整原表唯一确定，轴位来自真实Native实例；不再按名字搜索所有命名实例。
    // 系统UIFont的369个实例无需创建/枚举，也不以提高无关预算来绕过实际来源证明。
    finish(matched, count, raw.len(), &postscript, axes)
}

pub(super) fn resolve(
    bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    postscript: &str,
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    if postscript.is_empty()
        || postscript.len() > MAX_POSTSCRIPT_BYTES
        || postscript.chars().any(char::is_control)
    {
        return Err(Error::InvalidPostScriptName);
    }
    if axes.len() > MAX_AXES {
        return Err(Error::VariationBudgetExceeded);
    }
    let raw: &[u8] = bytes.as_ref().as_ref();
    let count = if raw.get(..4) == Some(b"ttcf") {
        if !matches!(u32_at(raw, 4)?, 0x0001_0000 | 0x0002_0000) {
            return Err(Error::InvalidContainer);
        }
        usize::try_from(u32_at(raw, 8)?).map_err(|_| Error::MetadataOverflow)?
    } else {
        1 // 只表示单SFNT候选；其真实签名与目录仍在下方严格验证。
    };
    if count == 0 || count > MAX_FACES {
        return Err(Error::FaceBudgetExceeded);
    }
    let mut budget = NameBudget {
        records: MAX_NAME_RECORD_VISITS,
        scalars: MAX_DECODED_NAME_SCALARS,
    };
    let mut matched = None;
    for face in 0..count {
        let base = if count == 1 && raw.get(..4) != Some(b"ttcf") {
            0
        } else {
            usize::try_from(u32_at(raw, 12 + face * 4)?).map_err(|_| Error::MetadataOverflow)?
        };
        let directory = directory(raw, base)?;
        let font = Font::new(Arc::clone(&bytes), face as u32).ok_or(Error::InvalidFace)?;
        let direct = budget.matches(&font, &directory, NameId::POSTSCRIPT_NAME, postscript)?;
        let mut named: Option<(u16, Vec<Axis>)> = None;
        if directory.has_fvar {
            let fvar = font
                .tables()
                .fvar()
                .map_err(|_| Error::InvalidVariationTable)?;
            let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
            if records.len() > MAX_AXES || usize::from(fvar.instance_count()) > MAX_INSTANCES {
                return Err(Error::VariationBudgetExceeded);
            }
            for (index, instance) in fvar
                .instances()
                .map_err(|_| Error::InvalidVariationTable)?
                .iter()
                .enumerate()
            {
                let instance = instance.map_err(|_| Error::InvalidVariationTable)?;
                let Some(id) = instance.post_script_name_id else {
                    continue; // 没有PSNameID的实例不能用subfamily显示名替代精确PSName。
                };
                if !budget.matches(&font, &directory, id, postscript)? {
                    continue;
                }
                if instance.coordinates.len() != records.len() {
                    return Err(Error::InvalidVariationTable);
                }
                if named.is_some() {
                    return Err(Error::AmbiguousNamedInstance);
                }
                let coordinates = records
                    .iter()
                    .zip(instance.coordinates.iter())
                    .map(|(record, value)| Axis {
                        tag: record.axis_tag().to_be_bytes(),
                        value: value.get().to_f32(),
                    })
                    .collect();
                named = Some((index as u16, coordinates));
            }
        }
        if direct || named.is_some() {
            if direct && let Some((_, coordinates)) = &named {
                let fvar = font
                    .tables()
                    .fvar()
                    .map_err(|_| Error::InvalidVariationTable)?;
                let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
                if records.iter().zip(coordinates).any(|(record, axis)| {
                    record.axis_tag().to_be_bytes() != axis.tag
                        || record.default_value().to_f32() != axis.value
                }) {
                    // nameID6也指该face的default位置；同名实例只有全部轴相同才是同一值。
                    // 不同位置共用PSName时不能根据遍历顺序任取其中一个。
                    return Err(Error::AmbiguousNamedInstance);
                }
            }
            if matched.is_some() {
                return Err(Error::AmbiguousFace); // 不依赖目录顺序或上次命中缓存挑第一个face。
            }
            matched = Some(Candidate {
                face: face as u32,
                font,
                named,
                has_fvar: directory.has_fvar,
            });
        }
    }
    finish(
        matched.ok_or(Error::PostScriptNotFound)?,
        count,
        raw.len(),
        postscript,
        axes,
    )
}

/// 两个来源只共用已经确定face后的轴校验和实际instance构造，不混淆各自的匹配授权。
fn finish(
    matched: Candidate,
    count: usize,
    source_bytes: usize,
    postscript: &str,
    axes: &[Axis],
) -> Result<(Font, Metadata)> {
    let named_instance = matched.named.as_ref().map(|(index, _)| *index);
    let mut actual = matched.named.map_or_else(Vec::new, |(_, axes)| axes);
    let mut explicit = axes.to_vec();
    explicit.sort_unstable_by_key(|axis| axis.tag);
    if explicit.windows(2).any(|pair| pair[0].tag == pair[1].tag) {
        return Err(Error::DuplicateAxis);
    }
    if matched.has_fvar {
        let fvar = matched
            .font
            .tables()
            .fvar()
            .map_err(|_| Error::InvalidVariationTable)?;
        let records = fvar.axes().map_err(|_| Error::InvalidVariationTable)?;
        if records.len() > MAX_AXES {
            return Err(Error::VariationBudgetExceeded);
        } // Native表匹配路径同样保留真实轴数量上限，但不读取无关命名实例。
        for (index, record) in records.iter().enumerate() {
            let tag = record.axis_tag().to_be_bytes();
            let min = record.min_value().to_f32();
            let default = record.default_value().to_f32();
            let max = record.max_value().to_f32();
            if !min.is_finite()
                || !default.is_finite()
                || !max.is_finite()
                || min > default
                || default > max
                || records[..index]
                    .iter()
                    .any(|earlier| earlier.axis_tag() == record.axis_tag())
            {
                return Err(Error::InvalidAxisValue);
            }
            if actual.is_empty() && index == 0 {
                actual = records
                    .iter()
                    .map(|axis| Axis {
                        tag: axis.axis_tag().to_be_bytes(),
                        value: axis.default_value().to_f32(),
                    })
                    .collect(); // 非命名实例采用本face真实default，不把请求轴默认补0。
            }
            let value = actual
                .iter_mut()
                .find(|axis| axis.tag == tag)
                .ok_or(Error::InvalidVariationTable)?;
            if let Some(requested) = explicit.iter().find(|axis| axis.tag == tag) {
                value.value = requested.value;
            }
            if !value.value.is_finite() || value.value < min || value.value > max {
                return Err(Error::InvalidAxisValue); // 实例表本身的越界坐标同样不能由builder静默夹取。
            }
        }
        if explicit
            .iter()
            .any(|axis| !actual.iter().any(|known| known.tag == axis.tag))
        {
            return Err(Error::UnknownAxis);
        }
    } else if !explicit.is_empty() {
        return Err(Error::UnknownAxis); // 静态face没有合法显式变体轴，不忽略请求。
    }
    actual.sort_unstable_by_key(|axis| axis.tag);
    let font = matched
        .font
        .instance_builder()
        .variations(actual.iter().map(|axis| (Tag::new(&axis.tag), axis.value)))
        .build();
    let units_per_em = font.units_per_em();
    if units_per_em == 0 {
        return Err(Error::InvalidUnitsPerEm);
    }
    let glyph_count = font.num_glyphs();
    Ok((
        font,
        Metadata {
            face_index: matched.face,
            face_count: count as u32,
            named_instance,
            units_per_em,
            glyph_count,
            axes: actual,
            postscript_name: postscript.to_owned(),
            source_bytes,
        },
    ))
}
