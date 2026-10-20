//! 只导出当前合成窗口的有限自有NSView渲染；不领取正文、Scene或WindowServer截图权限。
//! 调用者保原Source/Font/完整Input与CPU终点；本模块唯一拥有目录选择及bitmap/PNG文件I/O。

use super::*;

/// 只有显式目录才启用输出；普通验收不查询Native、不分配bitmap，也不创建任何默认目录。
pub(super) fn directory() -> Result<Option<std::path::PathBuf>, String> {
    let Some(path) = std::env::var_os("OMNI_NATIVE_UI_CAPTURE_DIRECTORY") else {
        return Ok(None);
    };
    let directory = std::path::PathBuf::from(path);
    if !directory.is_dir() {
        return Err("UI内部渲染图输出目录不存在".into());
    }
    Ok(Some(directory))
}

/// 真实内容根必须由调用者原窗口取得；只写同View当前有限bounds，不遍历逻辑全文或其它窗口。
/// 静态label与实际窗口号/paint/像素尺寸组成文件名；重复输出真实失败，绝不覆盖之前证据。
pub(super) fn write(
    directory: &Path,
    view: &NSView,
    label: &'static str,
    paints: usize,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    autoreleasepool(|_| {
        let window = step(current, || view.window())?.ok_or("UI图内容根已脱离原窗口")?;
        let number = step(current, || window.windowNumber())?;
        drop(window); // 原Native claim只活本次getter，不进入PNG或跨下一步骤。
        let bounds = step(current, || view.bounds())?;
        if ![
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || bounds.size.width <= 0.0
            || bounds.size.height <= 0.0
        {
            return Err("UI内部渲染内容根没有有限非空bounds".into());
        }
        // 原View的真实backing决定像素数，不缩放图片来掩盖布局，也不扩大到滚动文档高度。
        // cacheDisplay读取原已安装内容；调用者不能以新Scene.paint绘制一张替代通过图。
        let backing = step(current, || view.convertRectToBacking(bounds))?;
        let bitmap = step(current, || {
            view.bitmapImageRepForCachingDisplayInRect(bounds)
        })?
        .ok_or("AppKit无法缓存原内容根内部渲染图")?;
        step(current, || {
            view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap)
        })?;
        let (width, height) = step(current, || (bitmap.pixelsWide(), bitmap.pixelsHigh()))?;
        if width <= 0
            || height <= 0
            || width as f64 != backing.size.width
            || height as f64 != backing.size.height
        {
            return Err("UI图实际bitmap没有覆盖同窗口backing尺寸".into());
        }
        // 空属性只选择标准PNG编码；保原bitmap的实际颜色信息，不重染、拉伸或回填屏幕像素。
        // SAFETY: properties为空，无不匹配的属性值；原bitmap在本同步主线程池内保活。
        let data = step(current, || unsafe {
            bitmap.representationUsingType_properties(
                objc2_app_kit::NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        })?
        .ok_or("原窗口内部渲染PNG编码失败")?;
        let path = directory.join(format!(
            "{label}-window{number}-paint{paints}-{width}x{height}.png"
        ));
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|cause| cause.to_string())?;
        // SAFETY: data是本次独占编码得到的不可变NSData，未暴露可变别名；借用期间仅同步写盘，
        // 不调用Native、不进runloop，也不复制第二份整窗PNG。FD/bitmap/NSData均在本调用归还。
        let bytes = unsafe { data.as_bytes_unchecked() };
        output.write_all(bytes).map_err(|cause| cause.to_string())?;
        if !current() {
            return Err("UI内部渲染写入后原窗口权限已撤销".into());
        }
        println!(
            "native_ui_internal_render label={label} window={number} width={width} height={height} cpu_paints={paints} own_content_view=true presented_screen=false physical_ime=false"
        );
        Ok(())
    })
}
