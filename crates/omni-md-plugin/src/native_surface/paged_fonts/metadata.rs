//! 有限CF元数据读取；不创建正文字符串、CTLine或无界字典Vec。
use std::{ffi::c_void, os::unix::ffi::OsStrExt, path::PathBuf, ptr::NonNull};

use objc2_core_foundation::{
    CFData, CFNumber, CFRetained, CFString, CFStringBuiltInEncodings, CFType, CFURL, CGSize,
};
use objc2_core_graphics::CGFont;
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontOrientation, CTFontSymbolicTraits, kCTFontURLAttribute,
};
use omni_markdown_core::{MarkdownLayoutFont, MarkdownLayoutFontAxis};

use super::{NativeMarkdownPagedFontError as Error, Result, step};

pub(super) struct Metadata {
    pub graphics: CFRetained<CGFont>,
    pub path: PathBuf,
    pub name_table: CFRetained<CFData>,
    pub axes: Vec<MarkdownLayoutFontAxis>,
    design_coordinates: Vec<([u8; 4], f64)>,
    pub point_size: f64,
    units_per_em: u32,
    pub(super) glyph_count: usize,
}
impl Metadata {
    pub(super) fn name_bytes(&self) -> Result<&[u8]> {
        name_bytes(&self.name_table)
    }
    /// 系统 CFEqual 必须先认证实际 CTFont 相同，再核原文件/face/尺寸/轴/矩阵事实；不使用 PSName。
    pub(super) fn matches_capture(
        &self,
        font: &CTFont,
        captured: &super::NativeMarkdownPagedFont,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> Result<bool> {
        // CTFont 的 PartialEq 沿公开 CFEqual；阴性结果只表示不可复用，继续原真实捕获。
        if !step(current, || font == captured.native.as_ref())?
            || self.path != captured.source_path
            || self.point_size.to_bits() != captured.point_size.to_bits()
            || self.name_bytes()? != name_bytes(&captured.name_table)?
            || !step(current, || *self.graphics == *captured.constructor_graphics)?
        {
            return Ok(false);
        }
        // SAFETY: 两个强持的不可变 CTFont 都来自原主线程；只读取 Copy 标量，不进入正文布局。
        let matrix = step(current, || unsafe { font.matrix() })?;
        // SAFETY: captured 持原 CTFont 到本次比较完成，公开 getter 不产生外部指针借用。
        let original_matrix = step(current, || unsafe { captured.native.matrix() })?;
        // SAFETY: traits 来自同一实际候选，不能把普通字体复用给有粗体/斜体/等宽属性的请求。
        let traits = step(current, || unsafe { font.symbolic_traits() })?;
        // SAFETY: 原 owner 保活实际 Native face，比较全 trait 位而非语义期望或字体名称。
        let original_traits = step(current, || unsafe { captured.native.symbolic_traits() })?;
        if matrix != original_matrix
            || traits != original_traits
            || self.design_coordinates != read_axes(&captured.native, current)?
        {
            return Ok(false);
        }
        // 原 f64 轴位须仍对应这份 Core 实际归一化位置，完整 name 表与 glyph 数也继续核对。
        self.verify_core(&captured.core)?;
        step(current, || true)
    }

    pub(super) fn verify_core(&self, core: &MarkdownLayoutFont) -> Result<()> {
        let metadata = core.metadata();
        if self.units_per_em != u32::from(metadata.units_per_em)
            || self.glyph_count != metadata.glyph_count as usize
        {
            return Err(Error::CoreFaceMismatch); // 真实Native scalar交叉检查不能由同名或自报face替代。
        }
        core.verify_design_coordinates(&self.design_coordinates)?;
        Ok(())
    }
}

/// 彩色字体与含默认 tracking 的轮廓字体，其实际推进由原平台实例决定。
/// 文稿字体不能套用文件的 UI tracking 默认策略，也不能只按 hmtx 的 em 缩放替代。
/// 这里只捕获与原Font同寿命的固定glyph域元数据；不接收文本、Source或排版结果。
pub(super) fn horizontal_advances(
    font: &CTFont,
    graphics: &CGFont,
    glyph_count: usize,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<Option<Box<[f64]>>> {
    // SAFETY: 调用方在原主线程持有此不可变CTFont；只读取实际trait，不按名称猜字体种类。
    let traits = step(current, || unsafe { font.symbolic_traits() })?;
    if !traits.contains(CTFontSymbolicTraits::TraitColorGlyphs) {
        // 只对原实际 CGFont 同时提供 STAT/trak 的轮廓 face 建完整平台表；这与原
        // HarfRust 默认 tracking 准入一致。没有这些表的普通字体继续原文件/变体路径，
        // 不为每个字体增加无收益的全 glyph 复制，也不按名称、UI role 或正文字符猜测。
        for tag in [*b"STAT", *b"trak"] {
            let table = step(current, || {
                CGFont::table_for_tag(Some(graphics), u32::from_be_bytes(tag))
            })?;
            let present = table.is_some();
            step(current, || drop(table))?; // 临时表的 Native 强边在逐 glyph 读取前归还。
            if !present {
                return Ok(None);
            }
        }
    }
    let count = glyph_count;
    if !(1..=usize::from(u16::MAX) + 1).contains(&count) {
        return Err(Error::MetadataUnavailable); // CoreGraphics的真实glyph输入是u16，不能截断编号域。
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| Error::HorizontalMetricsAllocation)?;
    let mut glyphs = [0_u16; 256];
    let mut advances = [CGSize::ZERO; 256];
    for start in (0..count).step_by(glyphs.len()) {
        let used = glyphs.len().min(count - start);
        for (offset, glyph) in glyphs[..used].iter_mut().enumerate() {
            *glyph = u16::try_from(start + offset).map_err(|_| Error::MetadataUnavailable)?;
        }
        // SAFETY: 两个固定栈数组各有used个完整槽，所有glyph编号已在同Font真实域内。
        // 一批至多256个glyph；原current在每次Native返回后复核，撤权立即丢弃整个候选表。
        step(current, || unsafe {
            font.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::new(glyphs.as_mut_ptr()).expect("固定glyph数组非空"),
                advances.as_mut_ptr(),
                used as _,
            )
        })?;
        for advance in &advances[..used] {
            if !advance.width.is_finite() || !advance.height.is_finite() || advance.height != 0.0 {
                return Err(Error::MetadataUnavailable); // 非水平或无效平台度量不能伪成文件默认值。
            }
            result.push(advance.width);
        }
    }
    step(current, || Some(result.into_boxed_slice())) // 原Font已失权时，完整表仍只在本栈归还，不交给后继capture。
}

/// CGFont公开返回不可变表的+1引用；借用仅活在本metadata owner内，不复制整个字体。
pub(super) fn name_bytes(data: &CFData) -> Result<&[u8]> {
    let count = usize::try_from(data.length()).map_err(|_| Error::MetadataUnavailable)?;
    let pointer = data.byte_ptr();
    if !(6..=1_048_576).contains(&count) || pointer.is_null() {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: CopyTable的不可变CFData保活完整count字节；上方已核有限长度和非空指针，
    // 返回slice生命周期受data借用约束，不允许跨其实际CFRelease保存裸地址。
    Ok(unsafe { std::slice::from_raw_parts(pointer, count) })
}

/// 把原系统字体的真实 CG 实例用于文稿，不把 UI usage 描述符带入正文级联。
/// 这只创建尚未发布的不可变字体；没有 Source、输入权限、正文或全局资源注册。
pub(super) fn document_instance(font: &CTFont) -> Result<CFRetained<CTFont>> {
    // SAFETY: 调用者保活原 NSFont/CTFont，公开 getter 的返回值均由当前栈独占接管。
    let (graphics, points, matrix, traits) = unsafe {
        (
            font.graphics_font(std::ptr::null_mut()),
            font.size(),
            font.matrix(),
            font.symbolic_traits(),
        )
    };
    if !points.is_finite() || points <= 0.0 {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: 原 CGFont 强边贯穿构造，matrix 是本栈完整 Copy 值；None 明确不传 UI descriptor。
    // 每个实际字号/字重先由 AppKit 原工厂决定，本层不按名字匹配或继承别的字号光学轴。
    let document = unsafe { CTFont::with_graphics_font(&graphics, points, &matrix, None) };
    // SAFETY: 新字体为上方真实 +1 结果；取得自己的 CG 实例只用于有限来源后验。
    let document_graphics = unsafe { document.graphics_font(std::ptr::null_mut()) };
    let name = CGFont::table_for_tag(Some(&graphics), u32::from_be_bytes(*b"name"))
        .ok_or(Error::MetadataUnavailable)?;
    let document_name =
        CGFont::table_for_tag(Some(&document_graphics), u32::from_be_bytes(*b"name"))
            .ok_or(Error::MetadataUnavailable)?;
    // 原 UI 包装与文稿实例的 CGFont CFEqual 可以为 false；完整名称表、真实 glyph 域、
    // units 与原轴/矩阵才是此处应核的实际轮廓事实，不能假称两个包装对象相同。
    if name_bytes(&name)? != name_bytes(&document_name)?
        || CGFont::number_of_glyphs(Some(&graphics))
            != CGFont::number_of_glyphs(Some(&document_graphics))
        || CGFont::units_per_em(Some(&graphics)) != CGFont::units_per_em(Some(&document_graphics))
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    // 此纯字体构造尚无 Source/事件票；沿既有有界读取复用完整轴与 file URL 校验，
    // 临时字典/URL 全归本调用释放，不创建假请求，也不读取或冻结任何文稿文件。
    if read_axes(font, &|| true)? != read_axes(&document, &|| true)? {
        return Err(Error::GraphicsSourceMismatch);
    }
    let original_path = file_path(font, &|| true)?;
    let document_path = file_path(&document, &|| true)?.ok_or(Error::FileSourceUnavailable)?;
    if original_path.is_some_and(|original| original != document_path) {
        return Err(Error::GraphicsSourceMismatch);
    }
    // SAFETY: 两个不可变对象仍由本栈强持；只核公开标量，不触发布局或访问正文。
    let (actual_points, actual_matrix, actual_traits, units, actual_units) = unsafe {
        (
            document.size(),
            document.matrix(),
            document.symbolic_traits(),
            font.units_per_em(),
            document.units_per_em(),
        )
    };
    let semantics = CTFontSymbolicTraits::TraitBold
        | CTFontSymbolicTraits::TraitItalic
        | CTFontSymbolicTraits::TraitMonoSpace;
    if actual_points != points
        || actual_matrix != matrix
        || units != actual_units
        || (traits & semantics) != (actual_traits & semantics)
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    Ok(document) // 返回唯一新字体强边；原图形、名称表、轴字典和 URL 已随栈归还。
}

/// 样式转换可以换同 family 的真实 italic face，但不能丢字重、等宽或原矩阵。
/// 缩字号允许平台重算 optical axis；不是用旧 UIFont 的隐藏轴补写新文稿实例。
pub(super) fn require_derived_style(
    original: &CTFont,
    derived: &CTFont,
    points: f64,
    italic: bool,
) -> Result<()> {
    // SAFETY: 调用方在原主线程同时保活两个不可变字体；这里只读取有限公开事实。
    let (
        family,
        derived_family,
        before_points,
        actual_points,
        matrix,
        actual_matrix,
        before,
        after,
    ) = unsafe {
        (
            original.family_name(),
            derived.family_name(),
            original.size(),
            derived.size(),
            original.matrix(),
            derived.matrix(),
            original.symbolic_traits(),
            derived.symbolic_traits(),
        )
    };
    let unchanged = CTFontSymbolicTraits::TraitBold
        | CTFontSymbolicTraits::TraitMonoSpace
        | CTFontSymbolicTraits::TraitExpanded
        | CTFontSymbolicTraits::TraitCondensed;
    let expected_italic = italic || before.contains(CTFontSymbolicTraits::TraitItalic);
    if *family != *derived_family
        || !points.is_finite()
        || points <= 0.0
        || actual_points != points
        || matrix != actual_matrix
        || (before & unchanged) != (after & unchanged)
        || after.contains(CTFontSymbolicTraits::TraitItalic) != expected_italic
    {
        return Err(Error::GraphicsSourceMismatch);
    }
    // 新 italic face 可以额外声明 YAXS 等轴；原 wght/wdth/其他已指定轴必须仍保持。
    // 只有本次请求修改的 italic/slant 与缩字号时的 optical size 不作原数值相等要求。
    let original_axes = read_axes(original, &|| true)?;
    let actual_axes = read_axes(derived, &|| true)?;
    for (tag, value) in original_axes {
        if (italic && matches!(&tag, b"ital" | b"slnt"))
            || (points != before_points && tag == *b"opsz")
        {
            continue;
        }
        if !actual_axes
            .iter()
            .any(|(actual_tag, actual)| *actual_tag == tag && *actual == value)
        {
            return Err(Error::GraphicsSourceMismatch);
        }
    }
    Ok(()) // family 与轴的 Copy 强边只属于这次后验，不进入 palette、Source 或全局缓存。
}

fn string(value: &CFString, current: &(impl Fn() -> bool + ?Sized)) -> Result<String> {
    let units = step(current, || value.length())?;
    if !(1..=1024).contains(&units) {
        return Err(Error::MetadataUnavailable);
    }
    let mut bytes = [0u8; 1025];
    // SAFETY: 固定栈数组完整可写，容量包含末尾NUL；失败不能使用被截断的PSName。
    let copied = step(current, || unsafe {
        value.c_string(
            bytes.as_mut_ptr().cast(),
            bytes.len() as _,
            CFStringBuiltInEncodings::EncodingUTF8.0,
        )
    })?;
    if !copied {
        return Err(Error::MetadataUnavailable);
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::MetadataUnavailable)?;
    let value = std::str::from_utf8(&bytes[..end]).map_err(|_| Error::MetadataUnavailable)?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::MetadataUnavailable);
    }
    Ok(value.to_owned())
}

fn file_path(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Option<PathBuf>> {
    // SAFETY: 常量是公开CTFont文件URL键；font仍由调用方原owner保活。
    let Some(raw_url) = step(current, || unsafe { font.attribute(kCTFontURLAttribute) })? else {
        return Ok(None);
    }; // UIFont代理可以没有URL；唯一后继来源只允许其原CGFont公开往返，不能扫描或猜系统路径。
    let url =
        step(current, || raw_url.downcast::<CFURL>())?.map_err(|_| Error::FileSourceUnavailable)?;
    let scheme = step(current, || url.scheme())?.ok_or(Error::FileSourceUnavailable)?;
    let file_scheme = string(&scheme, current)? == "file";
    step(current, || drop(scheme))?;
    if !file_scheme {
        return Err(Error::FileSourceUnavailable); // 不把http等URL的path部分当本地字体来源。
    }
    let mut path = [0u8; 4096];
    // SAFETY: 输出是完整4096字节可写栈缓冲；公开函数成功时保证NUL终止。
    if !step(current, || unsafe {
        url.file_system_representation(true, path.as_mut_ptr(), path.len() as _)
    })? {
        return Err(Error::FileSourceUnavailable);
    }
    let end = path
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::FileSourceUnavailable)?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end]));
    if !path.is_absolute() {
        return Err(Error::FileSourceUnavailable); // 不让工作目录决定真实Font文件身份。
    }
    step(current, || drop(url))?;
    Ok(Some(path))
}

pub(super) fn read(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Metadata> {
    // 同一个原生操作立即接管两个Copy结果；即使外层失权也不会遗失out-parameter的+1。
    let (graphics, descriptor) = step(current, || {
        let mut raw_descriptor = std::ptr::null();
        // SAFETY: out指针指向有效栈槽，Copy规则赋予非空descriptor独立+1引用。
        let graphics = unsafe { font.graphics_font(&mut raw_descriptor) };
        let descriptor = NonNull::new(raw_descriptor.cast_mut()).map(|pointer| {
            // SAFETY: 非空值刚由公开Copy out参数交付，类型为CTFontDescriptor且尚未交还。
            unsafe { CFRetained::<CTFontDescriptor>::from_raw(pointer) }
        });
        (graphics, descriptor)
    })?;
    let name_table = step(current, || {
        CGFont::table_for_tag(Some(&graphics), u32::from_be_bytes(*b"name"))
    })?
    .ok_or(Error::MetadataUnavailable)?;
    name_bytes(&name_table)?; // 在任何匹配前限制真实表长；不能按合成PSName反推face。
    // SAFETY: size是同一CTFont的公开只读标量；不访问layoutManager或正文。
    let point_size = step(current, || unsafe { font.size() })?;
    if !point_size.is_finite() || point_size <= 0.0 {
        return Err(Error::MetadataUnavailable);
    }
    // SAFETY: 同一个原CGFont与Copy附加属性进行公开往返；矩阵None不访问外部指针。
    let recreated = step(current, || unsafe {
        CTFont::with_graphics_font(
            &graphics,
            point_size,
            std::ptr::null(),
            descriptor.as_deref(),
        )
    })?;
    let original_path = file_path(font, current)?;
    let path = file_path(&recreated, current)?.ok_or(Error::FileSourceUnavailable)?;
    if original_path
        .as_ref()
        .is_some_and(|original| original != &path)
    {
        return Err(Error::GraphicsSourceMismatch);
    } // 原URL存在则必须与同CG构造来源一致；缺失时也只用实际构造结果，绝不猜文件名。
    // SAFETY: 原Font和真实CG往返结果均由本栈强持有，getter只读Copy矩阵。
    let original_matrix = step(current, || unsafe { font.matrix() })?;
    // SAFETY: recreated来自上面的原CGFont和真实附加属性，没有外来指针或替补字体。
    let recreated_matrix = step(current, || unsafe { recreated.matrix() })?;
    if original_matrix != recreated_matrix {
        return Err(Error::GraphicsSourceMismatch);
    }
    // SAFETY: null out参数为公开可选值；返回的+1 CGFont由本栈接管。
    let recreated_graphics = step(current, || unsafe {
        recreated.graphics_font(std::ptr::null_mut())
    })?;
    let recreated_name = step(current, || {
        CGFont::table_for_tag(Some(&recreated_graphics), u32::from_be_bytes(*b"name"))
    })?
    .ok_or(Error::MetadataUnavailable)?;
    if name_bytes(&name_table)? != name_bytes(&recreated_name)? {
        return Err(Error::GraphicsSourceMismatch);
    } // UIFont的CFEqual允许因角色/合成描述符不同而失败；真实name表必须完整一致。
    step(current, || drop(recreated_name))?;
    step(current, || drop(recreated_graphics))?;
    let design_coordinates = read_axes(font, current)?;
    let axes = design_coordinates
        .iter()
        .map(|(tag, value)| MarkdownLayoutFontAxis {
            tag: *tag,
            value: *value as f32,
        })
        .collect();
    // 往返对象只提供原CGFont的真实文件定位；CoreText可能把其轴坐标再次格式化舍入。
    // 实际shaper轴位必须来自仍由调用者持有的原Font，不能用该临时对象覆盖或补签。
    step(current, || drop(recreated))?;
    step(current, || drop(descriptor))?;
    // SAFETY: 两个公开标量只描述同一原字体；调用前后仍保原票，不构造字符或字形数组。
    let units_per_em = step(current, || unsafe { font.units_per_em() })?;
    let cg_units = step(current, || CGFont::units_per_em(Some(&graphics)))?;
    if u32::try_from(cg_units).ok() != Some(units_per_em) {
        return Err(Error::GraphicsSourceMismatch);
    }
    let glyph_count = step(current, || CGFont::number_of_glyphs(Some(&graphics)))?;
    step(current, || ())?;
    Ok(Metadata {
        graphics,
        path,
        name_table,
        axes,
        design_coordinates,
        point_size,
        units_per_em,
        glyph_count,
    })
}

/// 只读取原Font的实际轴位；排序规范字典枚举顺序，不忽略重复项或换成往返对象的值。
fn read_axes(font: &CTFont, current: &(impl Fn() -> bool + ?Sized)) -> Result<Vec<([u8; 4], f64)>> {
    // SAFETY: CopyVariation返回不可变字典，数字tag和值均由真实原Font提供。
    let variation = step(current, || unsafe { font.variation() })?;
    let mut axes = Vec::new();
    if let Some(variation) = variation {
        let count = step(current, || variation.len())?;
        if count > 32 {
            return Err(Error::MetadataUnavailable);
        }
        let mut keys = [std::ptr::null::<c_void>(); 32];
        let mut values = [std::ptr::null::<c_void>(); 32];
        // SAFETY: 不可变Copy字典count已核≤32，两个数组各提供32槽且字典贯穿全部借用。
        step(current, || unsafe {
            variation.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr())
        })?;
        for (&key, &value) in keys[..count].iter().zip(&values[..count]) {
            let (Some(key), Some(value)) =
                (NonNull::new(key.cast_mut()), NonNull::new(value.cast_mut()))
            else {
                return Err(Error::MetadataUnavailable);
            };
            // SAFETY: CTFontCopyVariation公开规定数字CFType键值；字典强claim覆盖此栈借用。
            let (key, value) = unsafe {
                (
                    &*key.as_ptr().cast::<CFType>(),
                    &*value.as_ptr().cast::<CFType>(),
                )
            };
            let key = step(current, || key.downcast_ref::<CFNumber>())?
                .ok_or(Error::MetadataUnavailable)?;
            let value = step(current, || value.downcast_ref::<CFNumber>())?
                .ok_or(Error::MetadataUnavailable)?;
            let tag = step(current, || key.as_i64())?.ok_or(Error::MetadataUnavailable)?;
            let native_value =
                step(current, || value.as_f64())?.ok_or(Error::MetadataUnavailable)?;
            // CFNumber的Float32转换可能因普通舍入返回false。先保存原双精度设计坐标，
            // Core解析真实fvar/avar后必须核最终F2Dot14位置完全相等，不能按设计值epsilon猜。
            if !native_value.is_finite() || !(native_value as f32).is_finite() {
                return Err(Error::MetadataUnavailable);
            }
            let tag = u32::try_from(tag)
                .map_err(|_| Error::MetadataUnavailable)?
                .to_be_bytes();
            if axes.iter().any(|(prior, _): &([u8; 4], f64)| *prior == tag) {
                return Err(Error::MetadataUnavailable); // 不忽略重复/无穷轴，也不把负数tag截断。
            }
            axes.push((tag, native_value));
        }
        step(current, || drop(variation))?;
    }
    axes.sort_unstable_by_key(|(tag, _)| *tag);
    Ok(axes)
}
