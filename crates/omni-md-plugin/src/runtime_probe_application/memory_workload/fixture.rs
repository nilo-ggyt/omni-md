//! 按有限块生成独立内存工作负载，夹具初始化不建立全文字符串或整表副本。

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const EXPECTED_BYTES: usize = 345_112;
    const EXPECTED_FNV1A64: u64 = 0xc505_5100_3c3c_1e93;

    /// 摘要只锁定合成输出，不能替代工作负载保存与关闭边界的正式SHA校验。
    fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
        for &byte in bytes {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    struct StreamFacts {
        bytes: usize,
        utf16: usize,
        largest_write: usize,
        newlines: usize,
        table_markers: usize,
        code_markers: usize,
        hash: u64,
        tail: [u8; 64],
        tail_len: usize,
    }

    impl Default for StreamFacts {
        fn default() -> Self {
            Self {
                bytes: 0,
                utf16: 0,
                largest_write: 0,
                newlines: 0,
                table_markers: 0,
                code_markers: 0,
                hash: 0xcbf2_9ce4_8422_2325,
                tail: [0; 64],
                tail_len: 0,
            }
        }
    }

    impl Write for StreamFacts {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            // 每次原生成写入都必须是完整UTF-8小块；统计只留标量与固定64B尾部。
            let text = std::str::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.bytes += bytes.len();
            self.utf16 += text.encode_utf16().count();
            self.largest_write = self.largest_write.max(bytes.len());
            self.hash = hash_bytes(self.hash, bytes);
            for &byte in bytes {
                if byte == b'\n' || byte == b'\r' {
                    self.newlines += 1;
                }
                if self.tail_len == self.tail.len() {
                    self.tail.copy_within(1.., 0);
                    self.tail[self.tail_len - 1] = byte;
                } else {
                    // 尾部未满时保留全部已见字节，跨write边界仍按真实输出计数标记。
                    self.tail[self.tail_len] = byte;
                    self.tail_len += 1;
                }
                let tail = &self.tail[..self.tail_len];
                self.table_markers += usize::from(tail.ends_with("第115行".as_bytes()));
                self.code_markers +=
                    usize::from(tail.ends_with("巨段落可见区域绘制检查".as_bytes()));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(()) // 此测试sink没有缓冲或文件owner。
        }
    }

    #[test]
    fn giant_paragraph_stream_is_bounded_and_preserves_exact_counterexample() {
        let mut facts = StreamFacts::default();
        write_giant_paragraph(&mut facts).expect("固定反例应完整流式生成");
        assert_eq!(facts.bytes, EXPECTED_BYTES);
        assert_eq!(facts.utf16, 165_042);
        assert!(facts.largest_write <= 2 * 1_024);
        assert_eq!(facts.newlines, 0);
        assert_eq!(facts.table_markers, 1);
        assert_eq!(facts.code_markers, 3);
        assert_eq!(facts.hash, EXPECTED_FNV1A64);
    }

    #[test]
    fn giant_paragraph_file_is_complete_and_create_new_preserves_it() {
        let directory = tempfile::tempdir().expect("独占测试目录");
        let path = directory.path().join("giant-paragraph.md");
        write_native_markdown_giant_paragraph_fixture(&path).expect("生成完整反例");
        let error =
            write_native_markdown_giant_paragraph_fixture(&path).expect_err("重复路径必须拒绝覆盖");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        // 固定2KiB读缓冲验证落盘摘要，不把测试反例读成全文String或建立Core文档。
        let mut file = std::fs::File::open(&path).expect("读取本测试独占文件");
        let mut scratch = [0_u8; 2 * 1_024];
        let mut bytes = 0;
        let mut hash = 0xcbf2_9ce4_8422_2325;
        loop {
            let read = file.read(&mut scratch).expect("有限块读取");
            if read == 0 {
                break; // 只有真实EOF才能结算完整文件，不按预期长度提前停止。
            }
            bytes += read;
            hash = hash_bytes(hash, &scratch[..read]);
        }
        assert_eq!(bytes, EXPECTED_BYTES);
        assert_eq!(hash, EXPECTED_FNV1A64);
        drop(file); // 先关闭本测试文件，再由唯一TempDir的正常Drop归还全部测试scratch。
    }
}
