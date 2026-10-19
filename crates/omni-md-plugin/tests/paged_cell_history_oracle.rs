//! 真实旧field历史分组的独立有限进程；外层watchdog不参与AppKit输入或关闭授权。

#[cfg(target_os = "macos")]
fn child() {
    let directory = tempfile::tempdir().expect("建立历史oracle自有临时目录");
    let path = directory.path().join("cell-history.md");
    // 固定短合成表，不读用户文件、不复制大草稿；首格保持明确的原始值base。
    std::fs::write(&path, "# Probe\n\n| H | C |\n| --- | --- |\n| base | 1 |\n").unwrap();
    let mtm = objc2_foundation::MainThreadMarker::new().expect("独立进程必须在真实主线程");
    // SAFETY: 此前没有NSApplication访问；专用子进程只拥有本测试短表，无嵌套run loop。
    unsafe { omni_md_plugin::native_surface::run_native_markdown_cell_history_oracle(&path, mtm) }
        .expect("原生产field历史与严格资源关闭oracle");
}

#[cfg(target_os = "macos")]
fn main() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--child" {
        child();
        return;
    }
    assert!(arguments.is_empty(), "仅支持无参或内部唯一--child入口");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("--child");
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        // 实际被测进程沿默认旧field生产策略；不继承其他诊断或动态库注入实验。
        if name.starts_with("OMNI_") || name.starts_with("DYLD_") {
            command.env_remove(key);
        }
    }
    let mut process = command.spawn().expect("启动独立真实AppKit历史oracle");
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        match process.try_wait().expect("查询自有子进程状态") {
            Some(status) => {
                assert!(status.success(), "历史oracle子进程失败：{status}");
                break;
            }
            None if Instant::now() >= deadline => {
                // 超时只是失败并回收自有进程；绝不据等待时间接受Native资源释放。
                process.kill().expect("终止超时自有oracle子进程");
                process.wait().expect("回收已终止自有oracle子进程");
                panic!("历史oracle未在45秒外进程watchdog内收到真实终点");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("真实NSTextField历史oracle仅支持macOS");
}
