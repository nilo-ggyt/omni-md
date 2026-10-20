//! 分页排版候选的真实Native字体来源；不创建窗口、正文、CTLine或全局字体缓存。
use std::path::PathBuf;

use objc2_core_foundation::{CFData, CFRetained, Type};
use objc2_core_graphics::CGFont;
use objc2_core_text::CTFont;
use objc2_foundation::MainThreadMarker;
use omni_markdown_core::{
    MarkdownLayoutFont, MarkdownLayoutFontAxis, MarkdownLayoutFontBytes,
    MarkdownLayoutFontBytesError, MarkdownLayoutFontBytesResourceObserver, MarkdownLayoutFontError,
};
use thiserror::Error;

mod metadata;

#[derive(Debug, Error)]
pub enum NativeMarkdownPagedFontError {
    #[error("原字体构造权限已撤销")]
    Revoked,
    #[error("原生字体来源元数据不可用或超出固定预算")]
    MetadataUnavailable,
    #[error("原生字体没有可安全捕获的本地文件来源")]
    FileSourceUnavailable,
    #[error("原生字体与CGFont往返来源不一致")]
    GraphicsSourceMismatch,
    #[error("原生字体与Core精确face或轴位置不一致")]
    CoreFaceMismatch,
    #[error("原生字体轴请求重复、过多或没有被实际接受")]
    InvalidVariation,
    #[error("无法为原生字体的有界水平度量分配存储")]
    HorizontalMetricsAllocation,
    #[error("无法捕获原生字体的不可变字节")]
    Bytes(#[from] MarkdownLayoutFontBytesError),
    #[error("无法解析原生字体的精确Core face")]
    Core(#[from] MarkdownLayoutFontError),
}
type Result<T> = std::result::Result<T, NativeMarkdownPagedFontError>;

/// 所有Native primitive复用原调用方权限；无内部重捕、重试或较新Font替补路径。
fn step<T>(current: &(impl Fn() -> bool + ?Sized), operation: impl FnOnce() -> T) -> Result<T> {
    if !current() {
        return Err(NativeMarkdownPagedFontError::Revoked);
    }
    let value = operation();
    if !current() {
        drop(value); // 临时Native claim在原栈结束；拒绝后不交给后继owner。
        return Err(NativeMarkdownPagedFontError::Revoked);
    }
    Ok(value)
}

/// 一个显式run持有原Native字体与同源Core字体；只有真实构造边可以产生派生轴实例。
///
/// CTFont/CGFont是框架共享的不可变字体值，其系统缓存销毁尚未由本模块证明。自有
/// snapshot的observer只证明本项目实际close/munmap结果；共享字体缓存和整产品验收仍独立验证。
#[derive(Clone)]
pub struct NativeMarkdownPagedFont {
    native: CFRetained<CTFont>,
    constructor_graphics: CFRetained<CGFont>,
    // 同原CGFont的有限不可变name表用于派生轴位，避免重新解释UIFont/合成PSName。
    name_table: CFRetained<CFData>,
    bytes: MarkdownLayoutFontBytes,
    core: MarkdownLayoutFont,
    constructor_postscript: String,
    source_path: PathBuf,
    point_size: f64,
    _main_thread: MainThreadMarker,
}

/// 本次候选的新快照与已有活动 owner 的借用具有不同释放义务；不以共享映射尚活误报泄漏。
pub(crate) enum NativeMarkdownFontCapture {
    Fresh(NativeMarkdownPagedFont),
    Reused(NativeMarkdownPagedFont),
}
impl NativeMarkdownFontCapture {
    /// 两类候选都使用同一个不可变 Native/Core 字体，原 GCB 仍必须完成真实整形试次。
    pub(crate) fn font(&self) -> &NativeMarkdownPagedFont {
        match self {
            Self::Fresh(font) | Self::Reused(font) => font,
        }
    }

    /// Selected 把这一真实强边移交给 Body；复用结果保留原 Core payload 注册身份。
    pub(crate) fn into_font(self) -> NativeMarkdownPagedFont {
        match self {
            Self::Fresh(font) | Self::Reused(font) => font,
        }
    }

    /// 只有本次新建的映射须在拒绝/失败时立即归还；复用映射由原 Body 的最终关闭结算。
    pub(crate) fn fresh_bytes_observer(&self) -> Option<MarkdownLayoutFontBytesResourceObserver> {
        match self {
            Self::Fresh(font) => Some(font.bytes_observer()),
            Self::Reused(_) => None,
        }
    }
}

impl NativeMarkdownPagedFont {
    /// 捕获已有CTFont的原CGFont名称表与实际轴位；完整表匹配唯一文件face，不解释UIFont别名。
    ///
    /// # Errors
    /// 失权、来源缺失、合成PSName未知、往返不一致或Core解析失败均拒绝，不猜face0。
    pub fn capture(
        font: &CTFont,
        maximum_font_bytes: u64,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        let original = metadata::read(font, current)?;
        Self::capture_metadata(font, original, maximum_font_bytes, mtm, current)
    }

    /// 只在系统已选出的实际候选内复用；不重排级联、不按名字猜字体，也不跳过后续原 GCB 试次。
    pub(crate) fn capture_reusing(
        font: &CTFont,
        palette: &[std::rc::Rc<super::paged_line_fonts::NativeMarkdownLineFont>],
        maximum_font_bytes: u64,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<NativeMarkdownFontCapture> {
        let original = metadata::read(font, current)?;
        for registered in palette {
            let captured = registered.native();
            if original.matches_capture(font, captured, current)? {
                // Clone 只保同一 CTFont/Core payload/匿名映射；不再次打开文件、resolve 或占注册槽。
                return step(current, || {
                    NativeMarkdownFontCapture::Reused(captured.clone())
                });
            }
        }
        Self::capture_metadata(font, original, maximum_font_bytes, mtm, current)
            .map(NativeMarkdownFontCapture::Fresh)
    }

    /// 只有未命中原实际字体时进入真实文件快照边界；精确 face/轴/尺寸仍由原解析器核验。
    fn capture_metadata(
        font: &CTFont,
        original: metadata::Metadata,
        maximum_font_bytes: u64,
        mtm: MainThreadMarker,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        let bytes = step(current, || {
            MarkdownLayoutFontBytes::capture_file(&original.path, maximum_font_bytes)
        })??;
        let name_table = step(current, || original.name_bytes())??;
        let horizontal = metadata::horizontal_advances(font, original.glyph_count, current)?;
        let core = step(current, || {
            // 同一个实际字体实例的彩色字形推进随Core payload封存，排版与Native绘制不再使用两套宽度。
            match horizontal {
                Some(advances) => {
                    MarkdownLayoutFont::resolve_name_table_at_size_with_horizontal_advances(
                        bytes.blob(),
                        name_table,
                        &original.axes,
                        original.point_size,
                        advances,
                    )
                }
                None => MarkdownLayoutFont::resolve_name_table_at_size(
                    bytes.blob(),
                    name_table,
                    &original.axes,
                    original.point_size,
                ),
            }
        })??;
        original.verify_core(&core)?;
        let native = step(current, || font.retain())?;
        step(current, || ())?; // 所有临时Native值的归还后仍按同票发布，不能补领较新权限。
        let constructor_postscript = core.metadata().postscript_name.clone();
        Ok(Self {
            native,
            constructor_graphics: original.graphics,
            name_table: original.name_table,
            bytes,
            core,
            constructor_postscript,
            source_path: original.path,
            point_size: original.point_size,
            _main_thread: mtm,
        })
    }

    /// 使用本owner保存的原CGFont构造新轴实例；不从外来合成PSName反推其来源。
    ///
    /// # Errors
    /// 重复/未知/越界轴、实际Native未接受请求、来源改变或原票失效均明确失败。
    pub fn derive_axes(
        &self,
        overrides: &[MarkdownLayoutFontAxis],
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<Self> {
        if overrides.len() > 32 {
            return Err(NativeMarkdownPagedFontError::InvalidVariation);
        }
        let mut requested = overrides.to_vec();
        requested.sort_unstable_by_key(|axis| axis.tag);
        if requested.windows(2).any(|pair| pair[0].tag == pair[1].tag) {
            return Err(NativeMarkdownPagedFontError::InvalidVariation);
        }
        let mut complete = self.core.metadata().axes.clone();
        for axis in requested {
            if let Some(existing) = complete
                .iter_mut()
                .find(|existing| existing.tag == axis.tag)
            {
                existing.value = axis.value;
            } else {
                complete.push(axis); // 未知轴交原Core resolver明确拒绝，不能静默忽略。
            }
        }
        let name_table = step(current, || metadata::name_bytes(&self.name_table))??;
        let expected = step(current, || {
            MarkdownLayoutFont::resolve_name_table(self.bytes.blob(), name_table, &complete)
        })??;
        // SAFETY: self.native由原主线程owner持有；公开getter返回独立+1描述符。
        let mut descriptor = step(current, || unsafe { self.native.font_descriptor() })?;
        for axis in &expected.metadata().axes {
            let identifier = step(current, || {
                objc2_core_foundation::CFNumber::new_i64(i64::from(u32::from_be_bytes(axis.tag)))
            })?;
            // SAFETY: identifier是公开OpenType tag数字，value已由Core实际face校验为有限范围。
            let next = step(current, || unsafe {
                descriptor.copy_with_variation(&identifier, f64::from(axis.value))
            })?;
            step(current, || drop(std::mem::replace(&mut descriptor, next)))?;
            step(current, || drop(identifier))?; // CFRelease也在原票内结束，不跨任何RefCell借用。
        }
        // SAFETY: 原CGFont强claim贯穿构造；没有矩阵指针，描述符来自同一owner完整轴位。
        let native = step(current, || unsafe {
            CTFont::with_graphics_font(
                &self.constructor_graphics,
                self.point_size,
                std::ptr::null(),
                Some(&descriptor),
            )
        })?;
        step(current, || drop(descriptor))?;
        let actual = metadata::read(&native, current)?;
        if actual.path != self.source_path {
            return Err(NativeMarkdownPagedFontError::GraphicsSourceMismatch); // 派生不能重新匹配另一字体文件。
        }
        let name_table = step(current, || actual.name_bytes())??;
        let horizontal = metadata::horizontal_advances(&native, actual.glyph_count, current)?;
        let core = step(current, || {
            // 轴实例的真实Native度量重新捕获；不能借旧轴快照或继承原字形宽度。
            match horizontal {
                Some(advances) => {
                    MarkdownLayoutFont::resolve_name_table_at_size_with_horizontal_advances(
                        self.bytes.blob(),
                        name_table,
                        &actual.axes,
                        actual.point_size,
                        advances,
                    )
                }
                None => MarkdownLayoutFont::resolve_name_table_at_size(
                    self.bytes.blob(),
                    name_table,
                    &actual.axes,
                    actual.point_size,
                ),
            }
        })??;
        actual.verify_core(&core)?;
        if core.metadata().face_index != self.core.metadata().face_index
            || core.metadata().axes != expected.metadata().axes
        {
            return Err(NativeMarkdownPagedFontError::InvalidVariation); // 请求过轴不等于真实Native已经采用。
        }
        let constructor_graphics = step(current, || self.constructor_graphics.clone())?;
        let name_table = step(current, || actual.name_table.clone())?;
        step(current, || drop(actual))?;
        step(current, || drop(expected))?;
        Ok(Self {
            native,
            constructor_graphics,
            name_table,
            bytes: self.bytes.clone(),
            core,
            constructor_postscript: self.constructor_postscript.clone(),
            source_path: self.source_path.clone(),
            point_size: self.point_size,
            _main_thread: self._main_thread,
        })
    }

    #[must_use]
    pub fn core_font(&self) -> &MarkdownLayoutFont {
        &self.core
    }
    #[must_use]
    pub fn native_font(&self) -> &CTFont {
        &self.native
    }
    /// 捕获时已与实际CTFont核对的字号；用于拒绝同face但尺寸不一致的字形绘制。
    #[must_use]
    pub fn point_size(&self) -> f64 {
        self.point_size
    }
    /// 公开shaping选项保留f32协议；原CTFont的双精度字号由同一个Core实例绑定并用于内部尺度。
    /// 只接受其唯一f32编码，同时核绑定值，避免0.1等合法尺寸被f64回转误拒或同face换字号。
    #[must_use]
    pub fn accepts_shaping_size(&self, points: f32) -> bool {
        self.core.shaping_point_size() == Some(self.point_size)
            && (self.point_size as f32).to_bits() == points.to_bits()
    }
    #[must_use]
    pub fn bytes_observer(&self) -> MarkdownLayoutFontBytesResourceObserver {
        self.bytes.resource_observer()
    }
    #[must_use]
    pub fn source_face_postscript(&self) -> &str {
        &self.constructor_postscript
    }

    /// 返回捕获时与同一CGFont/name表校验的文件身份，供有限来源诊断与可复现控制使用。
    /// 此路径不授予另开文件或用同名字体替换当前Core/Native payload的权限。
    #[must_use]
    pub fn source_path(&self) -> &std::path::Path {
        &self.source_path
    }
}
