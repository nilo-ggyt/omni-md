//! 按有限块生成真实Host合成笔记；初始化不建立全文字符串或整表副本，文件由Host独占目录收尾。

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// 只写调用者独占的全新文件；任一写入失败都阻止后续原生测量使用半成品。
pub fn write_native_markdown_memory_fixture(path: &Path) -> io::Result<()> {
    // create_new 保留已有文件；固定 8 KiB 缓冲与整篇长度无关，也避免逐词系统调用。
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = BufWriter::with_capacity(8 * 1_024, file);
    writer.write_all("# 中文长文、分级标题与二百四十行混合表格交互验收\n\n".as_bytes())?;

    // 每节分别写入标题、普通段落和列表，覆盖前部排版而不积累先前节的内容。
    for section in 1..=60 {
        writeln!(writer, "## 第{section}节：中文长文与分级标题\n")?;
        writer.write_all(
            concat!(
                "中文长文需要保持自然换行，滚动和切换展示模式时保留真实源位置。",
                "这一段用于检查标题、正文和列表之间的间距。\n\n",
                "### 子标题与列表\n\n",
                "- 第一项包含中文和 emoji 😀\n",
                "- 第二项保留 **粗体**、*斜体* 与 `行内代码`\n\n",
            )
            .as_bytes(),
        )?;
    }

    // 240 行使用同一真实表头；混合高、短行使 Find 前后都有需要重新准备的几何。
    writer.write_all("| 序号 | 内容 | 状态 | 地址 |\n| --- | --- | --- | --- |\n".as_bytes())?;
    for row in 1..=240 {
        let content = if row % 3 == 0 {
            // 长行只借用固定只读字面值；不能为每一行拼接整张表格。
            concat!(
                "中文自动换行测试。混合高行和短行，滚动时不能重叠、留白或跳位。",
                "中文自动换行测试。混合高行和短行，滚动时不能重叠、留白或跳位。",
                "中文自动换行测试。混合高行和短行，滚动时不能重叠、留白或跳位。",
            )
        } else {
            // 短行与前后高行交错，保留第115行这一独立查询目标。
            "短行"
        };
        writeln!(
            writer,
            "| 第{row}行 | {content} | 完成 👩‍💻 | https://example.com/r/{row} |"
        )?;
    }

    // 单个引用逻辑行包含 3000 个正文字符；分块写入不把它变成多个源段落。
    writer.write_all("\n> ".as_bytes())?;
    for _ in 0..200 {
        writer.write_all("引用内容保持原来的边框和坐标。".as_bytes())?;
    }

    // 单个 fence 内保留 18000 字符长行；每次只提交 12 字符的固定模式。
    writer.write_all("\n\n```text\n".as_bytes())?;
    for _ in 0..1_500 {
        writer.write_all("巨段落可见区域绘制检查 ".as_bytes())?;
    }
    writer.write_all(b"\n```\n")?;

    // 显式完成缓冲与文件同步后才交给 mmap/Core；错误由主入口清理独占临时目录。
    writer.flush()?;
    writer.get_ref().sync_all()
}

/// 显式生成零物理换行的巨型段落反例；不改默认长文，也不建立完整正文或新文档对象。
pub fn write_native_markdown_giant_paragraph_fixture(path: &Path) -> io::Result<()> {
    // create_new 限定本次独占路径；失败保留已有文件，半成品由调用者的临时目录统一回收。
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    write_giant_paragraph(&mut file)?;
    file.sync_all()
}

fn write_giant_paragraph(writer: &mut impl Write) -> io::Result<()> {
    const PATTERN: &[u8] = "中文单段落abc😀 ".as_bytes();
    const MARKERS: &[u8] = concat!(
        "第115行 巨段落可见区域绘制检查 ",
        "巨段落可见区域绘制检查 巨段落可见区域绘制检查 ",
    )
    .as_bytes();
    const BLOCK_BYTES: usize = PATTERN.len() * 50;

    // 唯一生成缓冲固定为2KiB；只填入本轮1150B，不为累计345112B分配另一份副本。
    let mut scratch = [0_u8; 2 * 1_024];
    // BLOCK_BYTES 是固定模式长度的整倍数，数组分块没有尾段，也不需要逐次检查动态块长。
    for pattern in scratch[..BLOCK_BYTES]
        .as_chunks_mut::<{ PATTERN.len() }>()
        .0
    {
        pattern.copy_from_slice(PATTERN);
    }
    for index in 0..300 {
        writer.write_all(&scratch[..BLOCK_BYTES])?;
        if index == 150 {
            // 原失败样本在零基index150之后插入一次标记；空格分隔不产生物理换行。
            writer.write_all(MARKERS)?;
        }
    }
    writer.flush() // 任一块失败立即返回；未完整写入的反例不能进入后续原生验收。
}
