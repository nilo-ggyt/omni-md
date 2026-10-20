//! 真实长段落 Link 只通过原 SourceRequest 验收；完整 Parser 仅在测试中作独立差分。

use super::*;

#[test]
fn long_root_links_keep_complete_labels_large_targets_and_original_selection_styles() {
    // 段落、label 与合法目标都越过原2048小块门；目的中的括号/实体与title中的星号
    // 必须由同一原语法处理，不可截断 label、直接保 raw URL 或把目标里的 delimiter 送进强调栈。
    let label = format!(
        " label **粗体😀** `a]b` {} 尾 ",
        "中文😀 \\[字面\\] ".repeat(400)
    );
    let raw_target = format!("https://example.com/{}", "p\\(a\\)&amp;😀".repeat(400));
    let source = format!(
        "{}*外层 [{label}]({raw_target} \"标题 *不是强调*\") 后文* tail",
        "普通 abc😀 ".repeat(400),
    );
    let open = source.find('[').unwrap();
    let label_start = open + 1;
    let label_end = label_start + label.len();
    let expected_label = units(&source, label_start..label_end);
    let (expected_owner, expected_destination, expected_title) = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Start(Tag::Link {
            dest_url, title, ..
        }) => Some((range, dest_url.into_string(), title.into_string())),
        _ => None,
    })
    .expect("完整原Parser必须确认同一合法长Link");
    assert!(expected_destination.len() > 2048);
    assert_eq!(expected_title, "标题 *不是强调*");
    for selection in [
        MarkdownTextSelection::caret(source.encode_utf16().count()),
        MarkdownTextSelection::caret(expected_label.start + 2),
        MarkdownTextSelection {
            anchor_utf16: expected_label.end,
            head_utf16: expected_label.start,
        },
    ] {
        let (directory, mut doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实长label及大于2048的合法目标必须完成，不得仍假作默认或General");
        };
        assert_oracle(leaf, &doc, &source, &requested);
        let link = leaf
            .link_at_for(&doc, authority(&doc), expected_label.start + 2)
            .unwrap()
            .unwrap();
        assert_eq!(link.label_utf16_range, expected_label);
        assert_eq!(link.destination.clone().into_string(), expected_destination);
        // 独立完整Parser的规范化目标是唯一可复制payload，不能把长label/段落复制混入账。
        assert_eq!(
            request.total_work().copied_source_bytes,
            expected_destination.len()
        );
        // 点击范围保完整真实 label；目标/title 的原字节虽然属于 LINK marker，却不变成可点击label。
        let destination_start = units(&source, label_end + 2..label_end + 2).start;
        assert!(
            leaf.link_at_for(&doc, authority(&doc), destination_start)
                .unwrap()
                .is_none()
        );
        let opener = units(&source, expected_owner.start..expected_owner.start).start;
        assert_eq!(
            leaf.query_for(&doc, authority(&doc), opener)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            selection.head_utf16 == doc.utf16_len(),
        );
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_link_label_emphasis_cannot_pair_with_outer_delimiters() {
    // 外层的闭合强调可包住整个Link，label内未配对的*却不能夺走外层opener；反向情形也相同。
    // 两条真实段落都超小块上限，不能让完整小块Parser兜住流式scope接线缺失。
    for text in [
        "*outside [inside*](https://example.com/a) after*",
        "[**inside](https://example.com/b) outside**",
        "**outside [*inside*](https://example.com/c \"*title*\") after**",
    ] {
        let source = format!("{} {text} tail", "普通😀 ".repeat(1000));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let backing = doc.original_backing_observer().unwrap();
        let requested = units(&source, source.find(text).unwrap()..source.len());
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("Root真实link强调scope必须完成");
        };
        assert_oracle(leaf, &doc, &source, &requested);
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_links_resolve_reference_and_image_priority_with_original_parser() {
    // 原引用与图片样本保持不变。现在必须沿同Global/唯一外存栈完成真实优先级，
    // 不能继续把已经接入的语法当General，也不能把图片本身冒成可点击Link。
    for text in [
        "[外 [内][T]](https://example.com/outer)\n\n[T]: https://example.com/inner\n",
        "![图片](https://example.com/image)\n",
    ] {
        let source = format!("{} {text}", "普通😀 ".repeat(1000));
        assert!(
            Parser::new_ext(
                &source,
                crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            )
            .any(|event| matches!(event, Event::Start(Tag::Link { .. } | Tag::Image { .. })))
        );
        let (_directory, doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let start = source.find(text).unwrap();
        let end = source.find("\n\n[T]:").unwrap_or(source.len());
        let requested = units(&source, start..end);
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一Global已经能回答原引用/图片，不能再签General空洞");
        };
        assert_oracle(ready, &doc, &source, &requested);
        if let Some(inner) = source.find("内") {
            let index = units(&source, inner..inner).start;
            assert_eq!(
                ready
                    .link_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .destination
                    .clone()
                    .into_string(),
                "https://example.com/inner",
            );
            let outer = units(&source, start + 1..start + 1).start;
            assert!(
                ready
                    .link_at_for(&doc, authority(&doc), outer)
                    .unwrap()
                    .is_none()
            );
        } else {
            let label = units(&source, start + 2..start + 2).start;
            assert!(
                ready
                    .link_at_for(&doc, authority(&doc), label)
                    .unwrap()
                    .is_none()
            );
        }
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn active_large_link_target_keeps_real_cancellation_generation_and_release() {
    let source = format!(
        "{} [{}](https://example.com/{}) tail",
        "*a ".repeat(100),
        "普通😀 ".repeat(1200),
        "x&amp;".repeat(1600),
    );
    for cancelled in [true, false] {
        let (directory, mut doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested, 57, doc.selection())
            .unwrap();
        // 只观察公开真实work，确认大目标正在归一化且未发布；不突变decoder/FD/私有phase。
        let mut decoding = false;
        for _ in 0..source.len() * 100 + 10000 {
            request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(
                request
                    .sources_for(&doc, authority(&doc))
                    .unwrap()
                    .is_none()
            );
            if request.total_work().normalizer_input_bytes > 2048 {
                decoding = true;
                break;
            }
        }
        assert!(decoding, "真实目标必须已进入流式归一化且未发布");
        let expected = if cancelled {
            request.cancel();
            MarkdownTableRequestError::Cancelled
        } else {
            doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            MarkdownTableRequestError::SourceSuperseded
        };
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(expected)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(expected)
        );
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn streamed_direct_link_fixtures_keep_complete_parser_targets_and_cross_viewport_labels() {
    // 这七个字面值完整保留原vendor用例，长例仍有512份前文、700份标签及5000字符目标。
    // Core公开请求拥有真实mmap帧，不伪造原1/7/1024字节帧；精确小片解码由共享内核自身验收。
    let long = format!(
        "{} [中文{} **粗体** `a]b`](/{}/中文&amp;😀) 后",
        "前文 mixed ".repeat(512),
        "标签".repeat(700),
        "a".repeat(5000),
    );
    for (fixture, source) in [
        "pre [中文 **粗体** `a]b` \\[转义](https://example.invalid/a\\(b\\)?x=&amp; \"题&amp;\") post",
        "outer *[内**粗**与`code`](../x)* trailing",
        "[](a)[x](b)",
        "[a ` b](x)",
        "[<span title='['>中文</span>](<目标>)",
        "[x](<a>\r\n\"多行\r\n标题\")",
        &long,
    ]
    .into_iter()
    .enumerate()
    {
        let expected: Vec<_> = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .filter_map(|(event, owner)| match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                // 本组固定目标/title都不含字面"]("；独立Parser先给真owner，随后这个明确
                // 分隔符只定位夹具label。不从待测proof取端点，也不在测试中重写括号语法。
                let raw = &source[owner.clone()];
                let close = raw.rfind("](").expect("原固定直接链接的label分隔符");
                assert_eq!(raw.as_bytes()[0], b'[');
                let label = units(source, owner.start + 1..owner.start + close);
                Some((units(source, owner), label, dest_url.into_string()))
            }
            _ => None,
        })
        .collect();
        assert!(!expected.is_empty());
        // 原生产open映射本次独占文件，所有后继只借这一个Doc；完整Parser和源字符串
        // 只属于独立预期，不作为被测请求的Storage实现、回调或预先解析结果注入。
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let selection = MarkdownTextSelection::caret(doc.utf16_len());
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let mut sources = request.take_sources_for(&doc, authority(&doc)).unwrap().unwrap();
        {
            // 必须取得真实RootLeafReady；随后全经公开Source/selection/publication门查询，
            // 不把SourcesOnly或某个内部span存在冒充整个请求已经有可见语义。
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) =
                sources.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
            else {
                panic!("原直接链接fixture必须经真实文件端口完成，fixture={fixture}");
            };
            // 全源逐位置差分也验证Code/Emphasis不能越过label的原Begin/End作用域。
            assert_oracle(ready, &doc, source, &requested);
            for (owner, label, destination) in &expected {
                for index in label.clone() {
                    let actual = ready.link_at_for(&doc, authority(&doc), index).unwrap().unwrap();
                    assert_eq!(&actual.label_utf16_range, label);
                    assert_eq!(actual.destination.clone().into_string(), *destination);
                }
                // 空label依旧有原marker样式，但没有可点击字符；目标/title也不取得label权限。
                for index in owner.start..label.start {
                    assert!(ready.link_at_for(&doc, authority(&doc), index).unwrap().is_none());
                }
                for index in label.end..owner.end {
                    assert!(ready.link_at_for(&doc, authority(&doc), index).unwrap().is_none());
                }
            }
        }
        if fixture == 6 {
            // 长例不得借完整小块Parser通过，复制账只准原Parser规范化后的URL；这同时
            // 排除把长label、普通前文或title复制到目标库存后仍宣称正文零复制的实现。
            let expected_bytes: usize = expected.iter().map(|(_, _, target)| target.len()).sum();
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().copied_source_bytes, expected_bytes);
            let (_, label, destination) = &expected[0];
            // 三个真实canonical窗口分别落在完整长label首/中/尾；它们不是伪装的SourceFrame宽度。
            // 同一请求通过公开reposition继续使用原Global，所有新输出仍经正常advance发布。
            let middle = label.start + label.len() / 2;
            for viewport in [label.start..label.start + 2, middle..middle + 2, label.end - 3..label.end] {
                request.reposition(&doc, &sources, viewport.clone(), authority(&doc)).unwrap();
                drop(sources);
                // 旧发布先归还，再按原量子等待真实重定位，不把旧label票直接补签给新窗。
                // 新窗的样式范围仍有限，完整label点击范围则来自同一闭合语法owner。
                super::inline::finish_inline(&mut request, &doc).unwrap();
                sources = request.take_sources_for(&doc, authority(&doc)).unwrap().unwrap();
                let MarkdownOrdinarySemanticState::RootLeafReady(ready) =
                    sources.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
                else {
                    panic!("长label的真实有限窗口必须完成");
                };
                assert_oracle(ready, &doc, source, &viewport);
                let actual = ready.link_at_for(&doc, authority(&doc), viewport.start).unwrap().unwrap();
                assert_eq!(&actual.label_utf16_range, label);
                assert_eq!(actual.destination.clone().into_string(), *destination);
            }
        }
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), selection);
        assert!(!doc.can_undo() && !doc.can_redo() && !doc.is_dirty());
        assert!(doc.original_file_identity_is_unchanged());
        // 这是只读路径，原文件身份、选择、历史和字节都必须保持；不能靠重开同内容文件
        // 冒充资源归还。observer只保实际计量事实，不保活Doc、Source或mmap。
        assert_eq!(std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(), source.as_bytes());
        drop((sources, request, doc));
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn original_unknown_link_fixtures_now_use_real_resolved_missing_and_image_permissions() {
    // 这四个原字面值不再以旧General期待掩盖接线缺口：Missing引用和坏目标仍是原字面，
    // Image不创造点击目标，内层已成功Link必须禁用外层Link，完整Parser给出独立样式。
    for source in [
        "[reference]",
        "![image](x)",
        "[outer [inner](x)](y)",
        "[x](<bad",
    ] {
        let (directory, doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let selection = doc.selection();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原ReferenceMissing/Image/禁用祖先必须得到真实结果，source={source:?}");
        };
        assert_oracle(ready, &doc, source, &requested);
        let inner = source
            .find("inner")
            .map(|start| units(source, start..start + "inner".len()));
        // 完整Parser在嵌套例只接纳内层x，外层括号仍是字面；图片alt有自身样式，
        // 但没有独立点击URI。逐位置核目标，不能只看“至少存在一个链接”。
        for index in requested {
            let actual = ready.link_at_for(&doc, authority(&doc), index).unwrap();
            if let Some(label) = inner.as_ref().filter(|label| label.contains(&index)) {
                let actual = actual.unwrap();
                assert_eq!(&actual.label_utf16_range, label);
                assert_eq!(actual.destination.clone().into_string(), "x");
            } else {
                assert!(actual.is_none());
            }
        }
        // 正常发布后取消真实父请求，后继advance也不能凭健康Source重新取得原动作权限。
        request.cancel();
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::Cancelled)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::Cancelled)
        );
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), selection);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        // 实际取消后丢弃全部原发布和请求；没有测试clear或手写零资源收据。
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_footnote_scope_remains_explicitly_pending_without_plain_or_link_authority() {
    // 引用/图片转正不能顺带授予尚未接入的长Footnote作用域；两种真实脚注语法都越过
    // 原2048有限Parser门，不能靠短例兜底或把有语义的后缀当成默认plain来通过。
    for source in [
        format!("{} [^f] tail\n\n[^f]: 脚注正文\n", "普通😀 ".repeat(1000)),
        format!("[^f]: {} **粗体** [x](y)\n", "普通😀 ".repeat(1000)),
    ] {
        assert!(
            Parser::new_ext(
                &source,
                crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            )
            .any(|event| matches!(
                event,
                Event::FootnoteReference(_) | Event::Start(Tag::FootnoteDefinition(_))
            ))
        );
        let (directory, doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let end = source.find("\n\n").unwrap_or(source.len());
        let requested = units(&source, 0..end);
        // 只声明固定输入中首个真实段落/定义的窗口；未来定义仍可由原Global读取，
        // 但不能因此让尚未支持的长Footnote作用域获得普通Link或默认plain权限。
        let mut request = doc
            .viewport_leaf_source_request(requested, 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert!(matches!(
            sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap(),
            MarkdownOrdinarySemanticState::LeafPending(_)
        ));
        // Pending就是本例的完整业务结果，不继续query一个假默认样式来制造局部通过。
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn original_url_literals_keep_parser_values_and_source_supersession_after_real_undo() {
    // 六个原URL字面与独立expected完整保留；Core真实规范化结果必须一次给出完整合法UTF8。
    // 精确1字节/8字节carry仍由生产UnescapeStream纯逻辑测试负责，此处不制造假Source帧。
    for (source, expected) in [
        (r"[x](/a\(b\))", "/a(b)"),
        ("[x](</a&amp;&#x1f600;>)", "/a&😀"),
        ("[x](</a&unknown;z>)", "/a&unknown;z"),
        (r"[x](</a\q>)", r"/a\q"),
        ("[x](</a&#0;>)", "/a�"),
        ("[x]()", ""),
    ] {
        let original: Vec<_> = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.into_string()),
            _ => None,
        })
        .collect();
        assert_eq!(original, [expected]);
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原合法URL不得因流式化而丢失");
        };
        assert_oracle(ready, &doc, source, &requested);
        let actual = ready
            .link_at_for(&doc, authority(&doc), 1)
            .unwrap()
            .unwrap();
        assert_eq!(actual.label_utf16_range, 1..2);
        assert_eq!(actual.destination.clone().into_string(), expected);
        // 这些原短Root由真实闭合小块Parser接入，唯一正文库存精确复制完整fixture一次；
        // 不能把vendor逐帧decoder的URL输出账冒充本入口的正文成本，空URL也不等于零输入。
        // Parser确实消费完整有限fixture；每步仍由finish_inline守原2048-byte输入上限，
        // 不把内部解析次数固化成快照。完整URL的独立期望仍来自上面的字面和Parser结果。
        assert_eq!(request.total_work().copied_source_bytes, source.len());
        assert!(request.total_work().parser_input_bytes >= source.len());
        // 真编辑先撤原Source，Undo只恢复字节不恢复旧generation或已失败请求；两次拒绝
        // 分别使用当前健康authority，不能靠调用方一直传cancelled掩盖sticky首因。
        doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
            .unwrap();
        assert_eq!(
            ready.link_at_for(&doc, authority(&doc), 1).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        doc.undo().unwrap();
        // Undo只恢复字节，不恢复旧Source代次；用当前健康authority重新问旧请求，
        // 也不能重建decoder接着写旧部分输出，原SourceSuperseded必须仍为首因。
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        let mut restored = Vec::new();
        doc.write_utf16_range_to(0..doc.utf16_len(), &mut restored)
            .unwrap();
        assert_eq!(restored, source.as_bytes());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}
