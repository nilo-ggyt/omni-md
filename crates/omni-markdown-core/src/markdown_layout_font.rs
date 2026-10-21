//! Resolve exact font faces and axis positions from immutable blobs; file capture first freezes a separate anonymous inode.
//!
//! The platform owner must keep every payload byte stable throughout all Font uses; mappings of externally mutable files do not qualify.
//! Resolution shares original bytes without copying fonts or guessing collection indices. Safe capture streams a snapshot with fixed 64-KiB scratch.
//! Neither path owns Native objects or Markdown text; a complete frozen-font blob grants no whole-document text access.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use harfrust::Font;
use thiserror::Error;

mod bytes;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) mod frontier;
mod horizontal;
pub(crate) mod positioning;
mod resolve;
pub use bytes::{
    MarkdownLayoutFontBytes, MarkdownLayoutFontBytesError, MarkdownLayoutFontBytesResourceObserver,
};

/// Use the original four-byte OpenType axis tag; display names or floating-point hashes cannot establish font authority.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkdownLayoutFontAxis {
    pub tag: [u8; 4],
    pub value: f32,
}

/// Metadata resolved from the same actual byte payload; axes contains all effective positions, with a hard 32-axis limit.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownLayoutFontMetadata {
    pub face_index: u32,
    pub face_count: u32,
    pub named_instance: Option<u16>,
    pub units_per_em: u16,
    pub glyph_count: u32,
    pub axes: Vec<MarkdownLayoutFontAxis>,
    pub postscript_name: String,
    pub source_bytes: usize,
}

#[derive(Debug, Error)]
pub enum MarkdownLayoutFontError {
    #[error("字体PSName为空、过长或包含控制字符")]
    InvalidPostScriptName,
    #[error("字体容器或SFNT头不完整")]
    InvalidContainer,
    #[error("字体集合face数量超出固定预算")]
    FaceBudgetExceeded,
    #[error("字体表目录超出固定预算或不完整")]
    InvalidDirectory,
    #[error("字体名称表不完整或不存在")]
    InvalidNameTable,
    #[error("字体名称元数据超出固定预算")]
    NameBudgetExceeded,
    #[error("字体face不是受支持的有效字体")]
    InvalidFace,
    #[error("字体变体表不完整")]
    InvalidVariationTable,
    #[error("字体变体元数据超出固定预算")]
    VariationBudgetExceeded,
    #[error("字体PSName在多个face中重复")]
    AmbiguousFace,
    #[error("字体PSName在多个命名实例中重复")]
    AmbiguousNamedInstance,
    #[error("字体PSName没有对应的真实face或命名实例")]
    PostScriptNotFound,
    #[error("字体显式轴tag重复")]
    DuplicateAxis,
    #[error("字体显式轴不属于实际face")]
    UnknownAxis,
    #[error("字体轴范围或请求值无效")]
    InvalidAxisValue,
    #[error("字体单位尺寸无效")]
    InvalidUnitsPerEm,
    #[error("字体元数据坐标溢出")]
    MetadataOverflow,
    #[error("字体绑定字号无效或超出可表示坐标")]
    InvalidPointSize,
    #[error("字体设计轴转换后不再是同一实际归一化位置")]
    DesignCoordinatesMismatch,
    #[error("字体水平推进表与实际glyph域、字号或可表示尺度不一致")]
    InvalidHorizontalAdvances,
    #[error("字体水平推进快照分配失败")]
    HorizontalAdvanceAllocation,
}
type Result<T> = std::result::Result<T, MarkdownLayoutFontError>;

/// Clone shares this Font instance's payload; resolving identical bytes again does not acquire its original authority.
#[derive(Clone)]
pub struct MarkdownLayoutFont(Arc<Payload>);

struct Payload {
    // Declaration order drops Font and finite metadata before publishing payload-drop completion; observers keep neither alive.
    font: Font,
    metadata: MarkdownLayoutFontMetadata,
    // Explicit-size construction uses precise positioning; the general Font path retains HarfBuzz's original 26.6 contract.
    positioning_points: Option<f64>,
    // Bitmap/UI font advances retain pure values, not Native objects/callbacks, and leave with this Font payload's last owner.
    horizontal_advances: Option<horizontal::HorizontalAdvances>,
    dropped: PayloadDrop,
}
struct PayloadDrop(Arc<AtomicBool>);
impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Weak authority for the original Font allocation; equal names or axis values do not identify the same owner.
#[derive(Clone)]
pub struct MarkdownLayoutFontAuthority {
    font: Weak<Payload>,
}

#[cfg(feature = "paged-shaping-frontier")]
impl MarkdownLayoutFontAuthority {
    /// Compare original allocation identity without upgrading weak references; equality alone neither proves liveness nor grants usable font authority.
    pub(crate) fn same_payload(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.font, &other.font)
    }
}

/// Prove only that this payload and its Font field were dropped, not that font bytes or mappings retained by another Arc were released.
#[derive(Clone)]
pub struct MarkdownLayoutFontResourceObserver {
    dropped: Arc<AtomicBool>,
}
impl MarkdownLayoutFontResourceObserver {
    #[must_use]
    pub fn payload_released(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
    }
}

impl MarkdownLayoutFont {
    /// Resolve a unique face/instance from actual PSName and complete stable bytes; explicit axes override corresponding instance positions.
    ///
    /// The caller supplies payload immutable throughout shared use. This layer retains the original Arc, without treating
    /// mutable-file mmap as a safe snapshot; the platform owner guarantees byte lifetime and correspondence with its actual CTFont.
    ///
    /// # Errors
    /// Reject incomplete/over-budget container, name, or axis metadata, ambiguous matches, and unknown names; never fall back to face zero.
    pub fn resolve(
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
        postscript: &str,
        axes: &[MarkdownLayoutFontAxis],
    ) -> Result<Self> {
        let (font, metadata) = resolve::resolve(bytes, postscript, axes)?;
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: None,
            horizontal_advances: None,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// A Native UI alias or synthesized variable PSName may be absent from file names; match its complete original name table to one actual face.
    /// Retain fixed directory/name/fvar limits and explicit axis validation, without parsing synthesized names or guessing an index/default face.
    ///
    /// # Errors
    /// Reject missing/over-budget tables, ambiguous faces, or invalid axes without substituting a same-name font or default face.
    pub fn resolve_name_table(
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
    ) -> Result<Self> {
        let (font, metadata) = resolve::resolve_name_table(bytes, native_name_table, axes)?;
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: None,
            horizontal_advances: None,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// Bind a new font authority to the full name table, actual axes, and original point size; quantize precise positioning only at output.
    /// Existing fonts/weak authorities are unchanged; the platform supplies the same size/source as its actual drawing CTFont.
    ///
    /// # Errors
    /// Reject sizes outside the original representable 26.6 range or failed face/axis resolution, without narrowing the general interface's valid range.
    pub fn resolve_name_table_at_size(
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
        points: f64,
    ) -> Result<Self> {
        positioning::validate_points(points)?;
        let (font, metadata) = resolve::resolve_name_table(bytes, native_name_table, axes)?;
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: Some(points),
            horizontal_advances: None,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// Bind complete horizontal advances in points from the same actual platform font at its original double-precision size.
    /// Use only when platform metrics differ from file hmtx, supplying exactly one value per actual glyph rather than a sample seen in text.
    /// Transfer the pure-value table to this payload, without retaining platform objects or growing with document content.
    ///
    /// # Errors
    /// Reject face/axis/size errors, incomplete glyph domains, or nonfinite/unrepresentable advances; never fill missing values from default hmtx.
    /// Preserve actual negative values; the original frontier's nonnegative proof decides whether early publication is possible.
    pub fn resolve_name_table_at_size_with_horizontal_advances(
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
        points: f64,
        horizontal_advances: Box<[f64]>,
    ) -> Result<Self> {
        positioning::validate_points(points)?;
        let (font, metadata) = resolve::resolve_name_table(bytes, native_name_table, axes)?;
        let horizontal_advances = horizontal::HorizontalAdvances::new(
            &font,
            points,
            metadata.glyph_count,
            horizontal_advances,
        )?;
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: Some(points),
            horizontal_advances: Some(horizontal_advances),
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// Derive new size/axes from this instance's same immutable source, requiring the original face's complete Native name table.
    /// Share underlying read-only face/layout caches while keeping the new payload, font authority, and bound size independent.
    ///
    /// # Errors
    /// Reject foreign/truncated name tables, invalid sizes, and duplicate/unknown/out-of-range axes without selecting another face.
    pub fn derive_name_table_at_size(
        &self,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
        points: f64,
    ) -> Result<Self> {
        positioning::validate_points(points)?;
        let (font, metadata) =
            resolve::derive_name_table(&self.0.font, &self.0.metadata, native_name_table, axes)?;
        // Platform advances belong to the old instance's actual size; ordinary derivation inherits neither that table nor its metric authority.
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: Some(points),
            horizontal_advances: None,
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// Derive the same face with complete advances for this actual Native size, without sharing/reusing the previous size's table.
    /// Validate the real glyph domain and transfer the table exclusively to the new payload while retaining original immutable face data.
    ///
    /// # Errors
    /// Reject the complete new instance if name table, axes, size, or any actual advance violates the original capture contract.
    pub fn derive_name_table_at_size_with_horizontal_advances(
        &self,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
        points: f64,
        horizontal_advances: Box<[f64]>,
    ) -> Result<Self> {
        positioning::validate_points(points)?;
        let (font, metadata) =
            resolve::derive_name_table(&self.0.font, &self.0.metadata, native_name_table, axes)?;
        let horizontal_advances = horizontal::HorizontalAdvances::new(
            &font,
            points,
            metadata.glyph_count,
            horizontal_advances,
        )?;
        // Failed new-table conversion drops the candidate only; original instance, authority, and published glyphs remain unaffected.
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: Some(points),
            horizontal_advances: Some(horizontal_advances),
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// Return this instance's actual bound point size; None explicitly selects the general HarfBuzz positioning contract.
    #[must_use]
    pub fn shaping_point_size(&self) -> Option<f64> {
        self.0.positioning_points
    }

    /// Require original double-precision design coordinates to normalize through actual fvar/avar to this Font's exact positions.
    /// Design-space f32 rounding can cross a 16.16 unit; accept only equal final normalized coordinates.
    ///
    /// # Errors
    /// Reject over-budget/duplicate/unknown axes, out-of-range values, invalid tables, or unequal normalization; no epsilon equivalence.
    pub fn verify_design_coordinates(&self, coordinates: &[([u8; 4], f64)]) -> Result<()> {
        positioning::verify_design_coordinates(self, coordinates)
    }

    #[must_use]
    pub fn metadata(&self) -> &MarkdownLayoutFontMetadata {
        &self.0.metadata
    }
    #[must_use]
    pub fn authority(&self) -> MarkdownLayoutFontAuthority {
        MarkdownLayoutFontAuthority {
            font: Arc::downgrade(&self.0),
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLayoutFontResourceObserver {
        MarkdownLayoutFontResourceObserver {
            dropped: Arc::clone(&self.0.dropped.0),
        }
    }
    /// Check the same immutable font payload; another capture with equal name/face/axes cannot impersonate its original owner.
    /// Identity alone grants no Source, glyph readiness, or Native drawing authority; each operation checks its own original ticket.
    #[must_use]
    pub fn permits(&self, authority: &MarkdownLayoutFontAuthority) -> bool {
        Weak::ptr_eq(&authority.font, &Arc::downgrade(&self.0))
    }
    /// Lend only to this crate's original shaping tasks; cloned underlying Fonts must not outlive or bypass this owner contract.
    pub(crate) fn font(&self) -> &Font {
        &self.0.font
    }

    /// Shaping and proofs use the same original Font/advances; a platform-metric instance cannot be queried at another scale.
    /// General fonts without platform metrics use actual file hmtx/variation tables directly rather than another default advance table.
    pub(crate) fn shaper_at_scale(&self, scale: i32) -> Result<harfrust::ShaperFont<'_, '_>> {
        let shaper = harfrust::ShaperFont::new(self.font()).with_scale(scale);
        match &self.0.horizontal_advances {
            Some(advances) => {
                advances.require_scale(scale)?;
                Ok(shaper.with_font_funcs(Some(advances)))
            }
            None => Ok(shaper),
        }
    }
}

#[cfg(test)]
mod tests;
