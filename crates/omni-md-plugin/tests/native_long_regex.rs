//! 独立真实主线程的Foundation正则负控；夹具只通过固定4KiB写缓冲生成。

#[cfg(target_os = "macos")]
fn main() {
    use std::io::Write;
    let mut args = std::env::args_os().skip(1);
    let mib = match args.next().as_deref() {
        Some(value) if value == "16" => 16,
        Some(value) if value == "64" => 64,
        _ => panic!("仅支持16或64，分别选择16MiB或64MiB合成单段"),
    };
    assert!(args.next().is_none(), "不得静默忽略额外正则负控参数");
    let bytes = mib * 1024 * 1024;
    let temporary_root = std::env::var_os("OMNI_NATIVE_REGEX_TEMP_ROOT")
        .expect("必须指定ROOT下的正则负控临时目录，不能写到未登记位置");
    let directory = tempfile::Builder::new()
        .prefix("native-long-regex-")
        .tempdir_in(temporary_root)
        .expect("在ROOT中创建正则负控自有临时目录");
    let path = directory.path().join("长单段正则负控.md");
    {
        let mut file = std::fs::File::create(&path).expect("创建合成ASCII夹具");
        let block = [b'a'; 4096];
        let mut remaining = bytes - 1;
        while remaining != 0 {
            let count = remaining.min(block.len());
            file.write_all(&block[..count]).expect("流式写入合成单段");
            remaining -= count;
        }
        file.write_all(b"Z").expect("追加唯一真实尾部匹配");
        file.sync_all().expect("完成夹具IO再开始原生扫描");
    } // 文件实际关闭后Core才捕获自己的不可变原始inode，不共享writer或wholeString。
    let mtm = objc2_foundation::MainThreadMarker::new().expect("正则负控必须在真实主线程");
    omni_md_plugin::native_surface::run_native_markdown_long_regex_control(&path, bytes, mtm)
        .expect("负控功能/canonical/实际资源收据必须通过，整段读取如实留在诊断输出");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("Foundation正则负控仅能在macOS实际执行，不能用非原生替身签通过");
}
