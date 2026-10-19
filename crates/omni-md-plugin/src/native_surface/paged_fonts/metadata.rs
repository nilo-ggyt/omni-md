//! 有限CF元数据读取；不创建正文字符串、CTLine或无界字典Vec。
use std::{ffi::c_void, os::unix::ffi::OsStrExt, path::PathBuf, ptr::NonNull};

use objc2_core_foundation::{
    CFData, CFNumber, CFRetained, CFString, CFStringBuiltInEncodings, CFType, CFURL,
};
use objc2_core_graphics::CGFont;
use objc2_core_text::{CTFont, CTFontDescriptor, kCTFontURLAttribute};
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
    glyph_count: usize,
}
impl Metadata {
    pub(super) fn name_bytes(&self) -> Result<&[u8]> {
        name_bytes(&self.name_table)
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
