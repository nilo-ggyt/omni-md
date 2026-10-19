//! 从同一存活worker领取真实有限LineEnd；不拼接全文，也不把item末尾当作软行。

use std::{cell::RefCell, io::Write};

use omni_markdown_core::{
    MarkdownComposedLine, MarkdownComposedLineFacts, MarkdownComposedLines,
    MarkdownLineComposeAnswer, MarkdownLineComposeBudget, MarkdownLineComposeStyle,
    MarkdownLineComposer,
};
use omni_md_plugin::native_surface::paged_line_fonts::NativeMarkdownLineFont;

pub const SOURCE_BYTES: usize = 1024 * 1024;
pub const FIRST_LINES: usize = 12;
pub const SECOND_LINES: usize = 24;
pub const MAX_PREFIX_LINES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Natural,
    Unbroken,
}

pub fn fixture(kind: Kind, source_bytes: usize) -> tempfile::NamedTempFile {
    assert!(source_bytes >= SOURCE_BYTES && source_bytes.is_multiple_of(4096));
    let mut file = tempfile::NamedTempFile::new().unwrap();
    if kind == Kind::Unbroken {
        let block = [b'a'; 4096];
        // 一个无空格、无换行的完整超长词；固定4KiB缓冲循环写入，不创建整个单段String。
        for _ in 0..source_bytes / block.len() {
            file.write_all(&block).unwrap();
        }
        file.flush().unwrap();
        assert_eq!(
            file.as_file().metadata().unwrap().len(),
            source_bytes as u64
        );
        return file;
    }
    let mut bytes = 0;
    // 每次仅格式化一个短序号；自然空格保留原UAX机会，换行数严格为零。
    // 后继单词不同，滚动后不能拿首行相同墨迹替代真实后续正文。
    for sequence in 0..100_000 {
        write!(
            file,
            "section{sequence:06} office view paragraph flowing native text "
        )
        .unwrap();
        bytes += b"section000000 office view paragraph flowing native text ".len();
        if bytes >= source_bytes {
            break;
        } // 只累计已知固定片段长度，末尾再核文件长度，不缓存所有已写片段。
    }
    file.flush().unwrap();
    assert_eq!(file.as_file().metadata().unwrap().len(), bytes as u64);
    assert!((source_bytes..source_bytes + 128).contains(&bytes));
    file
}

pub fn next_lines(
    worker: &mut MarkdownLineComposer,
    font: &NativeMarkdownLineFont,
    count: usize,
) -> MarkdownComposedLineFacts {
    assert!((1..=MAX_PREFIX_LINES).contains(&count));
    let mut remaining = count;
    // 循环只推进原coalesced worker；固定上限防止测试把永久Pending误报成已完成。
    for _ in 0..100_000 {
        match worker
            .step(MarkdownLineComposeBudget {
                bytes: 4096,
                scalars: 256,
                records: 256,
                metadata_units: 256,
            })
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {} // 原有限预算耗尽，保留同cursor续做。
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                font.registered(),
                                MarkdownLineComposeStyle {
                                    points: font.native().point_size() as f32,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap(); // 原请求只接受同Native capture的注册，不按字体名称替换。
            }
            MarkdownLineComposeAnswer::LineReady(facts) => {
                remaining -= 1;
                if remaining == 0 {
                    return facts;
                } // 只在真实LineEnd停下，未封存的半行绝不发布。
            }
            MarkdownLineComposeAnswer::NeedsFrontier(reason) => {
                panic!("当前真实Font计划尚无前沿能力：{reason:?}")
            }
            MarkdownLineComposeAnswer::Complete => panic!("只请求有限视口，却已消费完整超长单段"),
            MarkdownLineComposeAnswer::NeedsResolution(_) => {
                panic!("有限单段前缀出现未完成的真实解析")
            }
            MarkdownLineComposeAnswer::NeedsControl(_) => panic!("无控制符夹具出现控制请求"),
            MarkdownLineComposeAnswer::NeedsTabPlacement(_) => panic!("无制表符夹具出现Tab请求"),
        }
    }
    panic!("原worker在固定执行预算内没有交付请求的有限行数");
}

pub fn handles(output: &RefCell<MarkdownComposedLines>) -> Vec<MarkdownComposedLine> {
    let mut reader = output.borrow_mut();
    let mut current = reader.last_line().unwrap();
    let mut lines = Vec::new();
    // 领取上界固定的真实已封存行；密集大窗增加有限overscan，不扫描剩余单段或建立全文数组。
    while let Some(line) = current {
        assert!(lines.len() < MAX_PREFIX_LINES);
        current = reader.previous_line(&line).unwrap();
        lines.push(line);
    }
    lines.reverse();
    lines
}
