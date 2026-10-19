//! 独立ACTIVE小表回归；只写本测试的临时copy，不接收或修改外部笔记路径。

use omni_md_plugin::NativeMarkdownTableAppendCase;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 无参保持正常cargo test --tests的正例；显式枚举每次只驱动一个独立进程负例。
    let mut args = std::env::args_os().skip(1);
    let case = match args.next() {
        None => NativeMarkdownTableAppendCase::Positive,
        Some(flag) if flag == "--toolbar-format-selection" => {
            NativeMarkdownTableAppendCase::ToolbarFormatSelection
        }
        Some(flag) if flag == "--toolbar-field-history" => {
            NativeMarkdownTableAppendCase::ToolbarFieldHistory
        }
        Some(flag) if flag == "--pointer-selection" => {
            NativeMarkdownTableAppendCase::PointerSelection
        }
        Some(flag) if flag == "--find-replacement-composition" => {
            NativeMarkdownTableAppendCase::FindReplacementComposition
        }
        Some(flag) if flag == "--case" => {
            match args.next().as_deref().and_then(|value| value.to_str()) {
                Some("positive") => NativeMarkdownTableAppendCase::Positive,
                Some("budget") => NativeMarkdownTableAppendCase::Budget,
                Some("cancel") => NativeMarkdownTableAppendCase::Cancel,
                Some("format-unchanged") => NativeMarkdownTableAppendCase::FormatUnchanged,
                Some("commit-resigned") => NativeMarkdownTableAppendCase::CommitResigned,
                Some("format-selection") => NativeMarkdownTableAppendCase::FormatSelection,
                Some("toolbar-format-selection") => {
                    NativeMarkdownTableAppendCase::ToolbarFormatSelection
                }
                Some("toolbar-field-history") => NativeMarkdownTableAppendCase::ToolbarFieldHistory,
                Some("pointer-selection") => NativeMarkdownTableAppendCase::PointerSelection,
                Some("find-replacement-composition") => {
                    NativeMarkdownTableAppendCase::FindReplacementComposition
                }
                _ => {
                    return Err(
                        "用法：native_textkit_table_append [--toolbar-format-selection | --toolbar-field-history | --pointer-selection | --find-replacement-composition | --case positive|budget|cancel|format-unchanged|commit-resigned|format-selection|toolbar-format-selection|toolbar-field-history|pointer-selection|find-replacement-composition]".into(),
                    );
                }
            }
        }
        Some(_) => {
            return Err("用法：native_textkit_table_append [--toolbar-format-selection | --toolbar-field-history | --pointer-selection | --find-replacement-composition | --case positive|budget|cancel|format-unchanged|commit-resigned|format-selection|toolbar-format-selection|toolbar-field-history|pointer-selection|find-replacement-composition]".into());
        }
    };
    if args.next().is_some() {
        return Err("追加回归不接受额外参数".into()); // 不把未知harness参数静默解释为其他场景。
    }
    let directory = tempfile::tempdir()?;
    let vault = directory.path().join("vault");
    std::fs::create_dir(&vault)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(vault.join("工作负载.md"))?;
    // 夹具只有一个两列表格，末行末格坐标固定；不建立长文String或原文件镜像。
    let source = if case == NativeMarkdownTableAppendCase::PointerSelection {
        "| 编号 | 内容 |\n| :--- | :--- |\n| 项目 | **Alpha 中😀后** `code` \\| ~~A אבג 😀 Z~~ |\n"
    } else if case == NativeMarkdownTableAppendCase::FormatUnchanged {
        "| 编号 | 值 |\n| --- | --- |\n| 末项 | **甲** |\n" // 真实格式结果可恰好恢复本来已存在的 canonical 标记。
    } else if matches!(
        case,
        NativeMarkdownTableAppendCase::FormatSelection
            | NativeMarkdownTableAppendCase::ToolbarFormatSelection
            | NativeMarkdownTableAppendCase::ToolbarFieldHistory
    ) {
        "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前中😀后 |\n" // 中部选区包含非BMP字符，旁边汉字和邻格必须原样保留。
    } else {
        "| 编号 | 值 |\n| --- | --- |\n| 末项 | 原值 |\n" // 原追加三个场景保持相同夹具。
    };
    file.write_all(source.as_bytes())?;
    file.sync_all()?;
    drop(file); // 测量/Native阶段不保持生成器FD。
    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("追加回归必须位于真实主线程")?;
    // SAFETY: 独立harness=false进程首次访问NSApplication，TempDir涵盖完整真实关闭周期。
    unsafe {
        omni_md_plugin::run_native_markdown_table_append_workload_application(
            directory.path(),
            case,
            mtm,
        )
    }?;
    Ok(())
}
