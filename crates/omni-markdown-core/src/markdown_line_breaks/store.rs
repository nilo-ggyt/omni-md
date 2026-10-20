//! 断行专用32B机会编码；公共页IO只处理固定record，不理解本格式或正文。
use super::{
    MarkdownLineBreakBoundary as Boundary, MarkdownLineBreakError as Error,
    MarkdownLineBreakKind as Kind, Result, machine,
};
use crate::markdown_layout_index::{LayoutIndexObserver, PagedLayoutIndex};

pub(super) struct LineBreakIndex {
    page: PagedLayoutIndex<32>,
    last: Option<Boundary>,
}
impl LineBreakIndex {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            page: PagedLayoutIndex::new()?,
            last: None,
        })
    }
    #[cfg(test)]
    pub(super) fn test_with_file(file: std::fs::File) -> Result<Self> {
        Ok(Self {
            page: PagedLayoutIndex::test_with_file(file)?,
            last: None,
        })
    }
    pub(super) fn push(&mut self, event: machine::Break, dictionary: bool) -> Result<()> {
        let byte = usize::try_from(event.byte_offset).map_err(|_| Error::CoordinateOverflow)?;
        let utf16 = usize::try_from(event.utf16_offset).map_err(|_| Error::CoordinateOverflow)?;
        if self
            .last
            .is_some_and(|old| old.byte >= byte || old.utf16 >= utf16)
        {
            return Err(Error::CorruptMetadata);
        }
        let mut encoded = [0; 32];
        encoded[..8].copy_from_slice(&event.byte_offset.to_le_bytes());
        encoded[8..16].copy_from_slice(&event.utf16_offset.to_le_bytes());
        encoded[16] = match event.kind {
            Kind::Allowed => 0,
            Kind::Mandatory => 1,
        };
        encoded[17] = u8::from(dictionary);
        encoded[18] = 1; // 版本与固定保留字节防元数据被误读成另一格式。
        encoded[19] = match event.rule {
            "LB3" => 3,
            "LB4" => 4,
            "LB5" => 5,
            "LB8" => 8,
            "LB15c" => 15,
            "LB18" => 18,
            "LB20" => 20,
            "LB31" => 31,
            _ => return Err(Error::CorruptMetadata),
        };
        self.page.append(encoded)?;
        self.last = Some(Boundary {
            byte,
            utf16,
            kind: event.kind,
            needs_dictionary: dictionary,
        });
        Ok(())
    }
    pub(super) fn get(&mut self, index: usize) -> Result<Boundary> {
        let bytes = self.page.read(index)?;
        let kind = match bytes[16] {
            0 => Kind::Allowed,
            1 => Kind::Mandatory,
            _ => return Err(Error::CorruptMetadata),
        };
        if bytes[17] > 1
            || bytes[18] != 1
            || !matches!(bytes[19], 3 | 4 | 5 | 8 | 15 | 18 | 20 | 31)
            || bytes[20..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(Boundary {
            byte: usize::try_from(u64::from_le_bytes(
                bytes[..8].try_into().expect("固定8B字段"),
            ))
            .map_err(|_| Error::CoordinateOverflow)?,
            utf16: usize::try_from(u64::from_le_bytes(
                bytes[8..16].try_into().expect("固定8B字段"),
            ))
            .map_err(|_| Error::CoordinateOverflow)?,
            kind,
            needs_dictionary: bytes[17] == 1,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.page.len()
    }

    /// 只借原唯一追加页已写完的record范围；不复制页，也不把空余容量当作断行机会。
    /// 页内记录仍经get解码验证，旧页则继续由原FD及其首错、撤权规则负责。
    pub(super) fn append_page_records(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.page.append_page_records().map_err(Into::into)
    }

    pub(super) fn page_loads(&self) -> u64 {
        self.page.page_loads()
    }
    pub(super) fn poison(&mut self) {
        self.page.poison();
    }
    pub(super) fn seal(&mut self) -> Result<()> {
        self.page.seal().map_err(Into::into)
    }
    pub(super) fn close(&mut self) -> Result<()> {
        self.page.close().map_err(Into::into)
    }
    pub(super) fn resource_observer(&self) -> LayoutIndexObserver {
        self.page.resource_observer()
    }
}
