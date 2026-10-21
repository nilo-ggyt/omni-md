//! Export the current synthetic window's bounded, owned NSView rendering.
//! The caller retains original Source/font/Input authority and a complete CPU
//! endpoint; this module owns only opt-in bitmap encoding and PNG file I/O.

use super::*;

/// Require an explicit existing output directory. Ordinary acceptance allocates
/// no bitmap, creates no directory, and performs no Native work through this hook.
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

/// Render only the original content root's current finite bounds. A static label
/// and actual window/paint/pixel facts identify each file; create_new preserves
/// previous output instead of silently overwriting it on a repeated observation.
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
        drop(window); // The temporary Native claim does not cross encoding or subsequent observations.
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
        // Actual view backing determines pixel dimensions, without scaling away
        // layout defects or extending capture to the logical document height.
        // Cache the installed content rather than painting an alternative Scene
        // that could disagree with the user's current view.
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
        // Standard PNG encoding preserves the original bitmap's actual colors;
        // no recoloring, resizing, or screen pixels fill missing content.
        // SAFETY: Empty properties contain no mismatched values, and the original
        // bitmap remains retained throughout this synchronous main-thread pool.
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
        // SAFETY: This encoding produced immutable NSData with no mutable alias.
        // Its bytes remain borrowed only during synchronous file output, without
        // Native calls, run-loop reentry, or another whole-window PNG copy. The
        // file, bitmap, and NSData owners all end within this invocation.
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
