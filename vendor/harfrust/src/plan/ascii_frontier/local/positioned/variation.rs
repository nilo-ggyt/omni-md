//! 与read-fonts compute_delta相同的F48Dot16累加，按真实region/axis记录暂停。
use super::*;
use read_fonts::types::{F48Dot16, Fixed};

struct Scalar {
    index: usize,
    delta: i32,
    axis: usize,
    scalar: Fixed,
}
pub(super) struct Variation {
    outer: u16,
    inner: u16,
    region: usize,
    accum: F48Dot16,
    scalar: Option<Scalar>,
}
impl Variation {
    pub(super) fn new(outer: u16, inner: u16) -> Self {
        Self {
            outer,
            inner,
            region: 0,
            accum: F48Dot16::ZERO,
            scalar: None,
        }
    }
    pub(super) fn step(&mut self, layout: crate::LayoutData<'_>) -> Result<Option<f32>, Rejection> {
        let Some(store) = layout.ot.var_store.as_ref() else {
            return Ok(Some(0.0));
        };
        if layout.ot.coords.is_empty() || (self.outer == u16::MAX && self.inner == u16::MAX) {
            return Ok(Some(0.0)); // 与原compute_delta的无坐标/NO_VARIATION_INDEX短路相同。
        }
        let Some(data) = store.item_variation_data().get(usize::from(self.outer)) else {
            return Ok(Some(0.0));
        };
        let data = data.map_err(|_| Rejection::LookupUnavailable)?;
        if self.inner >= data.item_count() {
            return Err(Rejection::LookupUnavailable);
        }
        let regions = store
            .variation_region_list()
            .map_err(|_| Rejection::LookupUnavailable)?;
        if let Some(scalar) = &mut self.scalar {
            let region = regions
                .variation_regions()
                .get(scalar.index)
                .map_err(|_| Rejection::LookupUnavailable)?;
            if let Some(axis) = region.region_axes().get(scalar.axis) {
                // 一个axis是本量子的真实记录；逐操作保持上游Fixed分支、mul_div与早停。
                let peak = axis.peak_coord.get();
                let raw = layout
                    .ot
                    .coords
                    .get(scalar.axis)
                    .copied()
                    .unwrap_or_default();
                scalar.axis += 1;
                if peak != read_fonts::types::F2Dot14::ZERO && raw != peak {
                    let peak = peak.to_fixed();
                    let start = axis.start_coord.get().to_fixed();
                    let end = axis.end_coord.get().to_fixed();
                    if !(start > peak || peak > end || (start < Fixed::ZERO && end > Fixed::ZERO)) {
                        let coord = raw.to_fixed();
                        if coord < start || coord > end {
                            scalar.scalar = Fixed::ZERO;
                            scalar.axis = region.region_axes().len();
                        } else if coord < peak {
                            scalar.scalar = scalar.scalar.mul_div(coord - start, peak - start);
                        } else {
                            scalar.scalar = scalar.scalar.mul_div(end - coord, end - peak);
                        }
                    }
                }
                return Ok(None);
            }
            self.accum += scalar.scalar.mul_i32(scalar.delta);
            self.scalar = None;
            self.region += 1;
            return Ok(None); // 归并刚结束的region后再让下一量子读取下一个真实row。
        }
        if self.region >= usize::from(data.region_index_count()) {
            return Ok(Some(self.accum.to_f64() as f32));
        }
        let index = usize::from(
            data.region_indexes()
                .get(self.region)
                .ok_or(Rejection::LookupUnavailable)?
                .get(),
        );
        let region = regions
            .variation_regions()
            .get(index)
            .map_err(|_| Rejection::LookupUnavailable)?;
        // 不从行首重复nth扫描；按原word_delta_count编码直接读当前delta的唯一字节范围。
        let packed = data.word_delta_count();
        let words = usize::from(packed & 0x7fff);
        let long = packed & 0x8000 != 0;
        let word = if long { 4 } else { 2 };
        let short = if long { 2 } else { 1 };
        let base = data
            .get_delta_row_len()
            .checked_mul(usize::from(self.inner))
            .ok_or(Rejection::MetadataBudget)?;
        let (offset, width) = if self.region < words {
            (base + self.region * word, word)
        } else {
            (base + words * word + (self.region - words) * short, short)
        };
        let bytes = data
            .delta_sets()
            .get(offset..offset + width)
            .ok_or(Rejection::LookupUnavailable)?;
        let delta = match width {
            1 => i32::from(bytes[0] as i8),
            2 => i32::from(i16::from_be_bytes([bytes[0], bytes[1]])),
            4 => i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            _ => return Err(Rejection::LookupUnavailable),
        };
        if delta == 0 {
            self.region += 1;
            return Ok(None);
        } // 原算法仍解析region，但零项不计算scalar。
        if region.region_axes().len() > 32 {
            return Err(Rejection::MetadataBudget);
        }
        self.scalar = Some(Scalar {
            index,
            delta,
            axis: 0,
            scalar: Fixed::ONE,
        });
        Ok(None)
    }
}
