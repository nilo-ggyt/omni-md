//! 在独立真实主线程执行完整 TextKit 合同，或固定的属性窗口快照场景。

/// 测试进程独占合成临时文件与唯一应用，避免 Rust 工作线程测试框架改变 AppKit 前提。
fn main() {
    // 在任何原生构造前确定唯一固定场景；未知参数或额外参数均为调用错误，不能静默跑旧套件。
    let mut arguments = std::env::args_os().skip(1);
    let semantic_snapshot = match arguments.next() {
        None => false, // 无参保留原完整编辑与九个资源场景行为。
        Some(argument) if argument == "--semantic-snapshot" => true, // 唯一显式参数只选择长夹具快照回归。
        Some(_) => panic!("仅支持无参数或唯一 --semantic-snapshot 参数"),
    };
    assert!(
        arguments.next().is_none(),
        "仅支持无参数或唯一 --semantic-snapshot 参数"
    );

    // 只有本独立进程的真实主线程可以构造私有 NSApplication 子类。
    let mtm = objc2_foundation::MainThreadMarker::new().expect("验收必须在真实 AppKit 主线程执行");
    let directory = tempfile::tempdir().expect("创建测试自有合成夹具目录");
    let document = directory.path().join("probe.md");
    let fixture = if semantic_snapshot {
        omni_md_plugin::native_markdown_semantic_snapshot_probe_fixture() // 长文只承担属性窗口隔离合同。
    } else {
        omni_md_plugin::native_markdown_runtime_probe_fixture() // 既有完整合同继续使用原始短夹具。
    };
    std::fs::write(&document, fixture).expect("写入测试自有 Unicode 与表格夹具");
    let result = if semantic_snapshot {
        // SAFETY: 此前没有创建或访问 NSApplication；本进程位于真实主线程，仅拥有合成临时文件，且无嵌套应用循环。
        unsafe {
            omni_md_plugin::run_native_markdown_semantic_snapshot_probe_application(&document, mtm)
        }
    } else {
        // SAFETY: 与独立快照入口相同的唯一应用和真实主线程前提成立；两个分支在本进程内只执行一个。
        unsafe { omni_md_plugin::run_native_markdown_runtime_probe_application(&document, mtm) }
    };
    result.expect("原生 Markdown 事件与严格资源释放合同");
}
