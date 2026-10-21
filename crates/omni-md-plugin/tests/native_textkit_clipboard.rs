//! Verify explicit Native copy in an independent reader process. Only the
//! regression's named board and synthetic files are owned here; General and
//! the user's existing notes are outside this executable's authority.

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use omni_md_plugin::native_surface::{
        read_native_markdown_clipboard_control, run_native_markdown_clipboard_control,
    };

    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("剪贴板回归必须位于真实主线程")?;
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        // The writer uses actual documents and starts this same executable as
        // the independent reader; it does not substitute a second publisher.
        None => run_native_markdown_clipboard_control(mtm)?,
        Some("--reader") => {
            let name = arguments.next().ok_or("独立读取缺少私有剪贴板名称")?;
            let case = arguments.next().ok_or("独立读取缺少原场景身份")?;
            if arguments.next().is_some() {
                return Err("独立读取不接受额外参数".into()); // Unknown harness arguments cannot silently select another payload.
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
