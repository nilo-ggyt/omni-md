//! 排版只借原Font的真实度量与GDEF插入点；不创建CTLine、正文副本或第二个字体快照。

use omni_markdown_core::MarkdownLayoutFontAuthority;

use super::paged_fonts::NativeMarkdownPagedFont;

/// OpenType GDEF的LigCaretList使用16位计数；该上限约束字体元数据，不截断用户文字。
const MAX_LIGATURE_CARETS: usize = u16::MAX as usize;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedFontGeometryError {
    #[error("原字体几何请求已经失权")]
    Revoked,
    #[error("字体度量不属于原字体实例")]
    FontMismatch,
    #[error("字形不属于原字体")]
    InvalidGlyph,
    #[error("原字体变换尚未由分页字形坐标建模")]
    UnsupportedMatrix,
    #[error("原字体返回非有限或无效的度量")]
    InvalidMetrics,
    #[error("原字体连字插入点数量或坐标无效")]
    InvalidCarets,
    #[error("无法分配有限字体插入点缓冲")]
    Allocation,
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedFontGeometryError>;

/// 全部使用原CTFont真实字号的点单位；行合成者再取各run的最大ascent/descent，不能固定行高。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeMarkdownPagedFontMetricValues {
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub cap_height: f64,
    pub x_height: f64,
    pub underline_position: f64,
    pub underline_thickness: f64,
}

/// 弱身份不会延长字体/映射生命周期；相同PSName或字号的新capture不能消费旧度量。
pub struct NativeMarkdownPagedFontMetrics {
    font: MarkdownLayoutFontAuthority,
    values: NativeMarkdownPagedFontMetricValues,
}
impl NativeMarkdownPagedFontMetrics {
    pub fn values_for(
        &self,
        font: &NativeMarkdownPagedFont,
    ) -> Result<NativeMarkdownPagedFontMetricValues> {
        if !font.core_font().permits(&self.font) {
            return Err(NativeMarkdownPagedFontGeometryError::FontMismatch);
        } // 数值相同不替代原构造身份；这里不重读Native，也不借新字体补签。
        Ok(self.values)
    }
}

/// 原glyph的全部真实GDEF插入点；无GDEF时为空，不能把空列表解释成该连字没有内部字素。
/// 文字/GCB端点和缺少GDEF时的编辑策略仍由实际行几何负责，本类型不授予正文范围。
pub struct NativeMarkdownPagedLigatureCarets {
    font: MarkdownLayoutFontAuthority,
    glyph: u16,
    positions: Vec<f64>,
}
impl NativeMarkdownPagedLigatureCarets {
    pub fn positions_for(&self, font: &NativeMarkdownPagedFont, glyph: u32) -> Result<&[f64]> {
        if !font.core_font().permits(&self.font) {
            return Err(NativeMarkdownPagedFontGeometryError::FontMismatch);
        } // 旧Font已释放后另一次同名capture也不能恢复弱票。
        if glyph != u32::from(self.glyph) {
            return Err(NativeMarkdownPagedFontGeometryError::InvalidGlyph);
        } // 一个字形的GDEF位置不能被套到同Font的另一个glyph。
        Ok(&self.positions)
    }
}

/// 每个同步Native调用前后都核原票；中途撤权即丢弃候选，不发起重新捕获或重试。
fn step<T>(current: &dyn Fn() -> bool, operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedFontGeometryError::Revoked);
    }
    let value = operation();
    if !current() {
        return Err(NativeMarkdownPagedFontGeometryError::Revoked);
    }
    Ok(value)
}

/// shaper输出尚未应用任意字体矩阵；与glyph painter同样拒绝未经建模的变换。
fn require_matrix(font: &NativeMarkdownPagedFont, current: &dyn Fn() -> bool) -> Result<()> {
    // SAFETY: 原主线程Font owner贯穿同步调用，getter只返回Copy矩阵，无外部指针。
    let matrix = step(current, || unsafe { font.native_font().matrix() })?;
    if matrix.a != 1.0
        || matrix.b != 0.0
        || matrix.c != 0.0
        || matrix.d != 1.0
        || matrix.tx != 0.0
        || matrix.ty != 0.0
    {
        return Err(NativeMarkdownPagedFontGeometryError::UnsupportedMatrix);
    } // 不能让Native metrics使用变换后的空间而glyph仍用未变换的26.6坐标。
    Ok(())
}

/// 捕获原字号/轴位的真实度量；不读取正文，也不把度量升级为软行或可见区证明。
pub fn capture_paged_font_metrics(
    font: &NativeMarkdownPagedFont,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownPagedFontMetrics> {
    require_matrix(font, current)?;
    // SAFETY: 以下getter只读原主线程CTFont；每项独立配原票，避免重入后拼接新旧结果。
    let ascent = step(current, || unsafe { font.native_font().ascent() })?;
    // SAFETY: 原Font仍由调用者持有，返回值为点单位Copy标量。
    let descent = step(current, || unsafe { font.native_font().descent() })?;
    // SAFETY: 不传入缓冲或指针；原主线程Font在同步调用中保持存活。
    let leading = step(current, || unsafe { font.native_font().leading() })?;
    // SAFETY: 原Font精确轴位/字号已经由capture确认，本getter不改变Font。
    let cap_height = step(current, || unsafe { font.native_font().cap_height() })?;
    // SAFETY: 与前项同一原CTFont，getter无外部存储。
    let x_height = step(current, || unsafe { font.native_font().x_height() })?;
    // SAFETY: 下划线位置允许为负；getter只返回原Font点单位标量。
    let underline_position = step(current, || unsafe {
        font.native_font().underline_position()
    })?;
    // SAFETY: 原Font强claim贯穿最后一次Native读取，返回值不持资源。
    let underline_thickness = step(current, || unsafe {
        font.native_font().underline_thickness()
    })?;
    let all = [
        ascent,
        descent,
        leading,
        cap_height,
        x_height,
        underline_position,
        underline_thickness,
    ];
    if all.iter().any(|value| !value.is_finite())
        || ascent < 0.0
        || descent < 0.0
        || ascent + descent <= 0.0
        || !(ascent + descent).is_finite()
        || cap_height < 0.0
        || x_height < 0.0
        || underline_thickness < 0.0
    {
        return Err(NativeMarkdownPagedFontGeometryError::InvalidMetrics);
    } // 保留真实leading和underline位置的符号；不把异常度量静默夹成“正常”行高。
    step(current, || NativeMarkdownPagedFontMetrics {
        font: font.core_font().authority(),
        values: NativeMarkdownPagedFontMetricValues {
            ascent,
            descent,
            leading,
            cap_height,
            x_height,
            underline_position,
            underline_thickness,
        },
    })
}

/// 读取真实glyph的完整GDEF列表；最多512KiB临时元数据，与正文长度无关。
/// 调用者必须另核原spool/GCB，不能从这个字体原语推导任意文字的插入点。
pub fn capture_paged_ligature_carets(
    font: &NativeMarkdownPagedFont,
    glyph: u32,
    current: &dyn Fn() -> bool,
) -> Result<NativeMarkdownPagedLigatureCarets> {
    require_matrix(font, current)?;
    let glyph = u16::try_from(glyph)
        .ok()
        .filter(|value| u32::from(*value) < font.core_font().metadata().glyph_count)
        .ok_or(NativeMarkdownPagedFontGeometryError::InvalidGlyph)?;
    // SAFETY: 原Font与glyph已核；公开API允许null配0只查询实际列表长度。
    let count = step(current, || unsafe {
        font.native_font()
            .ligature_caret_positions(glyph, std::ptr::null_mut(), 0)
    })?;
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_LIGATURE_CARETS)
        .ok_or(NativeMarkdownPagedFontGeometryError::InvalidCarets)?;
    let mut positions = Vec::new();
    if count != 0 {
        positions
            .try_reserve_exact(count)
            .map_err(|_| NativeMarkdownPagedFontGeometryError::Allocation)?;
        positions.resize(count, f64::NAN); // 未被Native写入的槽保留NaN；不能把部分写入的0误当有效插入点。
        // SAFETY: Vec已初始化count个独占f64槽且调用期间不移动；原glyph/Font保持相同身份。
        let actual = step(current, || unsafe {
            font.native_font()
                .ligature_caret_positions(glyph, positions.as_mut_ptr(), count as _)
        })?;
        if usize::try_from(actual).ok() != Some(count)
            || positions.iter().any(|value| !value.is_finite())
        {
            return Err(NativeMarkdownPagedFontGeometryError::InvalidCarets);
        } // 字体中途给不同长度或未写满，不发布部分坐标，不用截短列表补救。
    } // 真正缺少GDEF时保持空；后继行几何根据实际GCB选择明确的插入策略。
    step(current, || NativeMarkdownPagedLigatureCarets {
        font: font.core_font().authority(),
        glyph,
        positions,
    })
}
