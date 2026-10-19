//! 显式复制的独立Native输出回归；只写自有命名板，不接触用户通用剪贴板或笔记。

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use omni_md_plugin::native_surface::{
        read_native_markdown_clipboard_control, run_native_markdown_clipboard_control,
    };

    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("剪贴板回归必须位于真实主线程")?;
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => run_native_markdown_clipboard_control(mtm)?, // writer创建真实Doc并启动本二进制的reader。
        Some("--reader") => {
            let name = arguments.next().ok_or("独立读取缺少私有剪贴板名称")?;
            let case = arguments.next().ok_or("独立读取缺少原场景身份")?;
            if arguments.next().is_some() {
                return Err("独立读取不接受额外参数".into()); // 不把harness选项静默变成不同测试载荷。
            }
            read_native_markdown_clipboard_control(&name, &case, mtm)?;
        }
        Some(_) => return Err("用法：native_textkit_clipboard [--reader 私有板 场景]".into()),
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("真实系统剪贴板输出回归仅支持macOS");
}
