//! 两种固定单段落输入；字节、UTF16和编辑后期望由样本定义，不由待测排版结果推导。

use std::io::{self, Write};

use super::SOURCE_BYTES;

const CJK_SOURCE_BYTES: usize = 4_194_306;
const CJK_SOURCE_UTF16: usize = 1_398_102;
const CJK_BLOCK_BYTES: usize = 4095;

#[derive(Clone, Copy)]
pub(super) enum SourceFixture {
    Ascii,
    Cjk,
}

impl SourceFixture {
    pub(super) fn from_arguments() -> Self {
        let mut arguments = std::env::args().skip(1);
        let selected = match arguments.next().as_deref() {
            None => Self::Ascii, // 原无参数入口仍只运行原ASCII样本，便于与旧artifact直接对照。
            Some("cjk") => Self::Cjk,
            Some(other) => panic!("不支持的Source验收样本：{other}"),
        };
        assert!(arguments.next().is_none(), "Source验收只接受一个可选样本名");
        selected
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::Cjk => "cjk",
        }
    }

    pub(super) fn source_bytes(self) -> usize {
        match self {
            Self::Ascii => SOURCE_BYTES,
            Self::Cjk => CJK_SOURCE_BYTES,
        }
    }

    pub(super) fn source_utf16(self) -> usize {
        match self {
            Self::Ascii => SOURCE_BYTES,
            Self::Cjk => CJK_SOURCE_UTF16,
        }
    }

    pub(super) fn maximum_write_bytes(self) -> usize {
        match self {
            Self::Ascii => 4096,
            Self::Cjk => CJK_BLOCK_BYTES,
        }
    }

    pub(super) fn write_source(self, file: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Ascii => {
                // 原ASCII生成路径及4MiB负载保持不变；不把整篇先收集为String或Vec。
                let block = [b'a'; 4096];
                for _ in 0..SOURCE_BYTES / block.len() {
                    file.write_all(&block)?;
                }
            }
            Self::Cjk => {
                // U+4E2D固定UTF8为E4 B8 AD、UTF16为4E2D。唯一栈块只占4095byte，
                // 每次写入保持完整scalar，所有块都无换行，不能借分段规避超长单段落。
                let mut block = [0_u8; CJK_BLOCK_BYTES];
                for scalar in block.as_chunks_mut::<3>().0 {
                    scalar.copy_from_slice(&[0xE4, 0xB8, 0xAD]);
                }
                for _ in 0..1024 {
                    file.write_all(&block)?;
                }
                file.write_all(&block[..1026])?;
                // 独立常量同时约束固定写入配方；后续再用真实Doc读回的长度验证打开结果。
                assert_eq!(CJK_BLOCK_BYTES * 1024 + 1026, CJK_SOURCE_BYTES);
                assert_eq!(CJK_SOURCE_UTF16 * 3, CJK_SOURCE_BYTES);
            }
        }
        Ok(())
    }

    pub(super) fn edited_prefix(self) -> [u16; 4] {
        match self {
            Self::Ascii => [b'X' as u16, b'a' as u16, b'a' as u16, b'a' as u16],
            Self::Cjk => [0x0058, 0x4E2D, 0x4E2D, 0x4E2D],
        }
    }

    pub(super) fn initial_prefix(self) -> [u16; 4] {
        match self {
            Self::Ascii => [b'a' as u16; 4],
            Self::Cjk => [0x4E2D; 4],
        }
    }

    pub(super) fn edited_source_bytes(self) -> usize {
        // 插入一个ASCII X，再经真实GCB命令删除一个原scalar。ASCII字节数不变；
        // CJK删的是三byte中，故UTF16回到初值、原4,194,306byte精确少两而仍有4MiB。
        self.source_bytes()
            .checked_sub(match self {
                Self::Ascii => 0,
                Self::Cjk => 2,
            })
            .unwrap()
    }

    pub(super) fn edited_byte_end(self, utf16_end: usize) -> usize {
        assert!(utf16_end <= self.source_utf16());
        match self {
            Self::Ascii => utf16_end,
            Self::Cjk if utf16_end == 0 => 0,
            Self::Cjk => {
                // 编辑后是一个单byte X和后续三byte中。匹配79001 UTF16的独立字节
                // 边界因此是237001；原首scalar懒准备门应与此字节边界同量纲比较。
                utf16_end.checked_mul(3).unwrap().checked_sub(2).unwrap()
            }
        }
    }
}
