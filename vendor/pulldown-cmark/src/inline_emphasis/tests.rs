// Copyright 2015 Google Inc. All rights reserved.
// 原语法的独立golden范围回归；MIT许可见LICENSE。

use crate::{Event, Options, Parser, Tag};

#[test]
/// 提取内核不能让两个消费者共享同一个错误；这些既定原Parser范围独立于新文件栈计算。
fn original_parser_emphasis_keeps_known_source_ranges() {
    for (source, expected) in [
        ("a_b_c", vec![]),
        ("a*b*c", vec![("em", 1..4)]),
        ("***foo***", vec![("em", 0..9), ("strong", 1..8)]),
        ("foo***bar***baz", vec![("em", 3..12), ("strong", 4..11)]),
        ("*foo _bar* baz_", vec![("em", 0..10)]),
        ("~~foo~~", vec![("strike", 0..7)]),
        ("H~2~O", vec![("sub", 1..4)]),
        ("^foo^", vec![("sup", 0..5)]),
    ] {
        let actual = Parser::new_ext(
            source,
            Options::ENABLE_STRIKETHROUGH | Options::ENABLE_SUBSCRIPT | Options::ENABLE_SUPERSCRIPT,
        )
        .into_offset_iter()
        .filter_map(|(event, range)| {
            let kind = match event {
                Event::Start(Tag::Emphasis) => "em",
                Event::Start(Tag::Strong) => "strong",
                Event::Start(Tag::Strikethrough) => "strike",
                Event::Start(Tag::Subscript) => "sub",
                Event::Start(Tag::Superscript) => "sup",
                _ => return None,
            };
            Some((kind, range))
        })
        .collect::<Vec<_>>();
        assert_eq!(actual, expected, "source={source:?}");
    }
}
