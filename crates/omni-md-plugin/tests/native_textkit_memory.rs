//! 独立真实AppKit内存工作负载；无参生成流式合成库，显式seed同样只复制至临时库。

use std::io::{self, Read};
use std::path::PathBuf;

#[path = "native_textkit_memory/fixture.rs"]
mod fixture;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = None;
    let mut giant_paragraph = false;
    let mut stop_after = omni_md_plugin::NativeMarkdownMemoryStopAfter::default();
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--fixture" {
            if input.is_some() {
                return Err("--fixture不能重复".into());
            }
            if giant_paragraph {
                // 已声明内建形状时拒绝第二种来源，不能按参数顺序覆盖原夹具。
                return Err("--fixture与--fixture-shape互斥".into());
            }
            input = Some(PathBuf::from(arguments.next().ok_or("--fixture缺少路径")?));
        } else if argument == "--fixture-shape" {
            if giant_paragraph {
                // 同名声明也拒绝重复，避免把输入错误解释为幂等默认选择。
                return Err("--fixture-shape不能重复".into());
            }
            if input.is_some() {
                // 外部seed与内建形状只允许一个源；此前seed路径仍未被读取或修改。
                return Err("--fixture-shape与--fixture互斥".into());
            }
            let shape = arguments.next().ok_or("--fixture-shape缺少明确形状")?;
            if shape != "giant-paragraph" {
                // 未知或非UTF-8值都不进入生成阶段，禁止回退后错误验收另一种文档。
                return Err(
                    "--fixture-shape仅支持giant-paragraph，未知形状不能回退默认夹具".into(),
                );
            }
            giant_paragraph = true; // 仅显式形状选择反例；无参仍沿原默认长文生成器。
        } else if argument == "--stop-after" {
            let value = arguments.next().ok_or("--stop-after缺少严格阶段枚举")?;
            stop_after = value.to_str().ok_or("阶段枚举必须为UTF-8")?.parse()?;
        } else {
            // harness=false没有libtest flag协议；未知参数必须明确失败，不静默改变工作负载。
            return Err(
                "用法：native_textkit_memory [--fixture 合成seed路径 | --fixture-shape giant-paragraph] [--stop-after 声明阶段]"
                    .into(),
            );
        }
    }
    let directory = tempfile::tempdir()?;
    let vault = directory.path().join("vault");
    std::fs::create_dir(&vault)?;
    let note = vault.join("工作负载.md");
    if let Some(input) = input {
        // 以固定I/O缓冲流式复制调用者合成seed，不修改外部文件，不在测量阶段留整篇String。
        let source = std::fs::File::open(input)?;
        let expected = source.metadata()?.len();
        if expected == 0 || expected > 2 * 1_024 * 1_024 {
            return Err("合成seed必须在1B至2MiB范围内".into());
        }
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&note)?;
        let copied = io::copy(&mut source.take(2 * 1_024 * 1_024 + 1), &mut destination)?;
        if copied != expected {
            return Err("合成seed复制期间长度变化".into());
        }
        destination.sync_all()?;
        println!("native_memory_seed supplied=true bytes={copied}");
    } else if giant_paragraph {
        // 与外部seed相同，反例只存在于本次独占临时库；仍走原严格关闭和原始字节校验。
        omni_md_plugin::write_native_markdown_giant_paragraph_fixture(&note)?;
        println!(
            "native_memory_seed supplied=false shape=giant-paragraph bytes={}",
            std::fs::metadata(&note)?.len()
        );
    } else {
        // 正常cargo test --tests无参也有真实有限工作负载；生成器仅写小块，不构造全文镜像。
        fixture::write_fixture(&note)?;
        println!(
            "native_memory_seed supplied=false bytes={}",
            std::fs::metadata(&note)?.len()
        );
    }
    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("工作负载必须在AppKit真实主线程")?;
    // SAFETY: 此专用harness=false进程此前未访问NSApplication，临时库只有合成内容且覆盖整个run。
    unsafe {
        omni_md_plugin::run_native_markdown_memory_workload_application(
            directory.path(),
            stop_after,
            mtm,
        )
    }?;
    Ok(()) // TempDir在严格关闭与有限测量idle完成之后清理，不在仓库保留生成的笔记或日志。
}
