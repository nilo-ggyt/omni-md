//! 精确字体face与轴位置；解析只消费不可变Blob，文件输入先冻结独立匿名inode。
//!
//! 平台owner必须先保证payload的全部字节在所有Font使用期内稳定；普通可被外部截断或
//! 改写的文件映射不满足此合同。resolver不复制字体、不猜集合index；安全捕获子模块
//! 可以按固定64KiB scratch流式创建快照。两者均不接触Native或Markdown正文。

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

/// 显式轴tag使用原OpenType四字节，不把显示名称或浮点hash当字体权限。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkdownLayoutFontAxis {
    pub tag: [u8; 4],
    pub value: f32,
}

/// 解析实际同一字节payload的结果；axes为完整有效轴位，硬上限为32个。
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

/// Clone只共享同一个Font实例payload；重新resolve相同字节也不取得原实例权限。
#[derive(Clone)]
pub struct MarkdownLayoutFont(Arc<Payload>);

struct Payload {
    // 按声明顺序实际析构Font与有限metadata后才发布drop事实。observer不保活它们。
    font: Font,
    metadata: MarkdownLayoutFontMetadata,
    // 只有显式字号构造采用高精度定位；旧通用字体仍保留HarfBuzz的原26.6合同。
    positioning_points: Option<f64>,
    // 平台位图/UI字体的实际水平推进只保纯值，不保Native对象或回调；原Font最后一个owner归还时一起释放。
    horizontal_advances: Option<horizontal::HorizontalAdvances>,
    dropped: PayloadDrop,
}
struct PayloadDrop(Arc<AtomicBool>);
impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// 原Font allocation的弱权限；名字、轴值相同不等于同一个owner。
#[derive(Clone)]
pub struct MarkdownLayoutFontAuthority {
    font: Weak<Payload>,
}

#[cfg(feature = "paged-shaping-frontier")]
impl MarkdownLayoutFontAuthority {
    /// 只比较原allocation身份；不升级弱引用，也不把相同名称/轴值或已经释放当作可用权限。
    pub(crate) fn same_payload(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.font, &other.font)
    }
}

/// 只证明本payload及其Font字段已析构，不证明其他Arc持有的字体字节或映射也已释放。
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
    /// 从真实PSName与完整字节确定唯一face/实例；显式轴覆盖该实例的对应坐标。
    ///
    /// 调用方须交付在所有共享使用期内不变的payload。此层只保原Arc，不把可变文件
    /// mmap当作安全快照；字体字节生命周期和Native CTFont对应关系由平台owner负责。
    ///
    /// # Errors
    /// 容器、名称、轴元数据不完整、超出固定预算、歧义或未知名称均明确拒绝，不回退face0。
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

    /// 原生UIFont别名/变量合成PSName不一定存在于文件名称表；以原Native完整name表匹配唯一face。
    /// 仍由固定目录/name/fvar预算和显式轴位构造，不解析合成名字、不猜集合索引或默认face。
    ///
    /// # Errors
    /// 表缺失、超预算、face歧义或轴位无效均拒绝；不以同名字体或默认face补齐来源。
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

    /// 以完整名称表、实际轴位和原字号绑定一个新字体权限，内部定位只在最终出口量化。
    /// 此构造不改变已有字体或其弱权限；平台须交付与实际绘制CTFont相同的字号和来源。
    ///
    /// # Errors
    /// 字号不在原26.6可表示范围，或face/轴解析失败时拒绝，不降低已有通用接口的范围。
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

    /// 绑定同一个实际平台字体在原双精度字号下的完整水平推进，单位为point。
    /// 仅平台实际度量不同于文件hmtx时使用；调用方须交付每个真实glyph恰好一项，
    /// 不能交正文出现字形的抽样。纯值表转入本payload，既不保平台对象也不按正文增长。
    ///
    /// # Errors
    /// face/轴/字号解析失败，表项数量不等于真实glyph域，或任一推进非有限、超坐标范围时拒绝；
    /// 不以默认hmtx补缺项。真实负值照常保留，由原前沿非负证明决定能否提前发布。
    pub fn resolve_name_table_at_size_with_horizontal_advances(
        bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
        native_name_table: &[u8],
        axes: &[MarkdownLayoutFontAxis],
        points: f64,
        horizontal_advances: Box<[f64]>,
    ) -> Result<Self> {
        positioning::validate_points(points)?;
        let (font, metadata) = resolve::resolve_name_table(bytes, native_name_table, axes)?;
        let horizontal_advances =
            horizontal::HorizontalAdvances::new(points, metadata.glyph_count, horizontal_advances)?;
        Ok(Self(Arc::new(Payload {
            font,
            metadata,
            positioning_points: Some(points),
            horizontal_advances: Some(horizontal_advances),
            dropped: PayloadDrop(Arc::new(AtomicBool::new(false))),
        })))
    }

    /// 返回本实例构造时绑定的实际字号；None明确表示通用HarfBuzz定位合同。
    #[must_use]
    pub fn shaping_point_size(&self) -> Option<f64> {
        self.0.positioning_points
    }

    /// 核原双精度设计坐标经真实fvar/avar转换后，与本Font采用的归一化位置完全相同。
    /// 设计空间的f32舍入可以跨一个16.16单位，只有最终实际位置相等才允许接受。
    ///
    /// # Errors
    /// 超预算、重复/未知轴、越界、无效表或归一化位置不同均拒绝，不按epsilon猜等价。
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
    /// 只核同一个不可变字体payload；同名、相同face或相同轴值的另一次capture不冒充原owner。
    /// 此身份判断不授予Source、字形Ready或Native绘制权限，各调用仍须核自己的原票。
    #[must_use]
    pub fn permits(&self, authority: &MarkdownLayoutFontAuthority) -> bool {
        Weak::ptr_eq(&authority.font, &Arc::downgrade(&self.0))
    }
    /// 只供同crate的原shaping任务借用；不得把克隆出的底层Font绕过本owner长期保留。
    pub(crate) fn font(&self) -> &Font {
        &self.0.font
    }

    /// 整形与证明统一借同一原Font和原水平推进；绑定平台度量的实例不能借另一个尺度查询。
    /// 不含平台度量的通用字体仍直接使用文件实际hmtx/变体表，不建立另一份默认表。
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
