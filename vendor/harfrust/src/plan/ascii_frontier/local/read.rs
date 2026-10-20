//! 证书只读真实OpenType记录；不缓存字体表副本，也不另写整形算法。
use super::Rejection;
use read_fonts::{
    tables::layout::{ClassDef, CoverageTable},
    types::GlyphId,
    FontData, FontRead,
};

pub(super) struct Read<'a>(pub(super) &'a [u8]);
impl Read<'_> {
    pub(super) fn u16(&self, at: usize) -> Result<u16, Rejection> {
        let bytes = self
            .0
            .get(at..at.checked_add(2).ok_or(Rejection::LookupUnavailable)?)
            .ok_or(Rejection::LookupUnavailable)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }
    pub(super) fn offset(&self, base: usize, at: usize) -> Result<usize, Rejection> {
        base.checked_add(usize::from(self.u16(at)?))
            .ok_or(Rejection::LookupUnavailable)
    }
    pub(super) fn coverage(&self, at: usize, glyph: u16) -> Result<Option<usize>, Rejection> {
        let data = self.0.get(at..).ok_or(Rejection::LookupUnavailable)?;
        Ok(CoverageTable::read(FontData::new(data))
            .map_err(|_| Rejection::LookupUnavailable)?
            .get(GlyphId::from(glyph))
            .map(usize::from))
    }
    pub(super) fn class(&self, at: usize, glyph: u16) -> Result<u16, Rejection> {
        let data = self.0.get(at..).ok_or(Rejection::LookupUnavailable)?;
        Ok(ClassDef::read(FontData::new(data))
            .map_err(|_| Rejection::LookupUnavailable)?
            .get(GlyphId::from(glyph)))
    }
    pub(super) fn single(&self, at: usize, glyph: u16) -> Result<Option<u16>, Rejection> {
        let Some(index) = self.coverage(self.offset(at, at + 2)?, glyph)? else {
            return Ok(None);
        };
        match self.u16(at)? {
            1 => Ok(Some(glyph.wrapping_add(self.u16(at + 4)?))),
            2 => {
                if index >= usize::from(self.u16(at + 4)?) {
                    return Err(Rejection::LookupUnavailable);
                }
                Ok(Some(self.u16(at + 6 + index * 2)?))
            }
            _ => Err(Rejection::CrossInputSubtable),
        }
    }
}
