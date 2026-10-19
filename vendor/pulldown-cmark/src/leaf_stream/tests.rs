// Copyright 2015 Google Inc. All rights reserved.
// 独立oracle保留固定0.13.4原决策；MIT条款见LICENSE。
use super::*;
use crate::scanners::*;

fn original(bytes: &[u8]) -> LeafStart {
    if bytes.first() == Some(&b'<') {
        if let Some(end) = get_html_end_tag(&bytes[1..]) {
            return LeafStart::HtmlTerminated(end);
        }
        if starts_html_block_type_6(&bytes[1..]) || scan_html_type_7(bytes).is_some() {
            return LeafStart::HtmlBlankTerminated;
        }
    }
    if let Ok(count) = scan_hrule(bytes) {
        return LeafStart::Rule(count);
    }
    if let Some(level) = scan_atx_heading(bytes) {
        return LeafStart::Heading(level);
    }
    if let Some((count, marker)) = scan_code_fence(bytes) {
        return LeafStart::Fence { marker, count };
    }
    LeafStart::DefinitionOrParagraph
}

fn classify(source: &[u8], width: usize, budget: usize) -> (String, usize) {
    let mut probe = LeafProbe::new(0);
    let mut total = 0;
    loop {
        let start = probe.next_byte();
        let end = start.saturating_add(width).min(source.len());
        let (step, read, work) = probe.advance(&source[start..end], end == source.len(), budget);
        assert!(read <= width && work <= budget);
        total += work;
        assert!(total <= source.len() * 10 + 64, "不可在frame尾空转");
        if let LeafProbeStep::Complete(result) = step {
            return (format!("{result:?}"), total);
        }
        assert!(
            read != 0 || work != 0,
            "NeedInput必须由调用者借出新的非空frame或真实EOF"
        );
    }
}

#[test]
fn every_frame_and_single_transition_match_original_leaf_opening() {
    let mut inputs = vec![
        "",
        "<",
        "<pre",
        "<prex>",
        "<pre >x",
        "<SCRIPT\rtext",
        "<!--x",
        "<![CDATA[",
        "<div>",
        "<divx>",
        "</table>",
        "<foo a='中文😀'>\r\nrest",
        "<foo a=x\ty>",
        "<foo a=x\u{b}y>",
        "<foo a b = 'x'>  \n",
        "<foo a b>tail",
        "</foo a>",
        "<foo a='x\ny'>",
        "<foo a=>",
        "<foo/ >",
        "<foo />\r",
        "<foo a\n>",
        "---",
        "- - -\r\n",
        "___\t_\rZ",
        "* *",
        "***x",
        "###### x",
        "####### ",
        "###\u{b}",
        "###中",
        "~~~info",
        "```info\r```",
        "```info\r\n```",
        "``",
        "paragraph",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    // 固定种子的短字节组合覆盖attribute回退、无值属性和闭合标签，不引入随机不稳定性。
    let alphabet = b"<>/ =:'\"-_a0\t\r\n";
    let mut seed = 0x917d_b38fu64;
    for _ in 0..4096 {
        let mut source = String::from("<a");
        for _ in 0..24 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            source.push(alphabet[seed as usize % alphabet.len()] as char);
        }
        inputs.push(source);
    }
    for source in inputs {
        let expected = format!("{:?}", original(source.as_bytes()));
        for width in [1, 2, 3, 7, 16, 29] {
            assert_eq!(
                classify(source.as_bytes(), width, 1).0,
                expected,
                "{source:?} width={width}"
            );
        }
    }
}

#[test]
fn giant_info_and_attribute_keep_fixed_storage_and_bounded_work() {
    for source in [
        format!("```{}\nbody", "x".repeat(1024 * 1024)),
        format!("```{}`\nbody", "x".repeat(1024 * 1024)),
        format!("<custom data='{}'>\nbody", "中😀".repeat(200_000)),
        format!("<custom data='{}\nbody", "x".repeat(1024 * 1024)),
    ] {
        let expected = format!("{:?}", original(source.as_bytes()));
        assert_eq!(classify(source.as_bytes(), 8191, 4096).0, expected);
    }
    assert!(std::mem::size_of::<LeafProbe>() <= 160);
}
