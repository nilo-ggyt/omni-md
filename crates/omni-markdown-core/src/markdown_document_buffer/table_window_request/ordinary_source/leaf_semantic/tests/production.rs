//! 从公开 Leaf 工厂进入原来源 owner，验证发布、续批和失效；不直接构造语义 job。

use super::*;

pub(super) fn finish_sources(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        // 真实最小量子逐次交还给调用方；没有函数内推进到 EOF 或 Ready 的生产捷径。
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(document, authority(document), 4096)?;
        assert_step(work);
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("公开 Leaf 来源请求未在有限累计预算内结束");
}

pub(super) fn leaf_sources(
    document: &MarkdownDocumentBuffer,
    requested: Range<usize>,
) -> (
    MarkdownViewportSourceRequest,
    VerifiedMarkdownViewportSources,
) {
    let mut request = document
        .viewport_leaf_source_request(requested, 57, document.selection())
        .unwrap();
    finish_sources(&mut request, document).unwrap();
    let sources = request
        .take_sources_for(document, authority(document))
        .unwrap()
        .unwrap();
    (request, sources)
}

#[test]
fn production_far_heading_survives_more_than_256_paragraph_and_blank_line_windows() {
    use crate::markdown_document_buffer::MarkdownHeadingAnchorStep as HeadingStep;
    use std::io::{Read, Write};

    const OPENING: &str = "original opening paragraph.\n\n";
    const PARAGRAPH: &str =
        "original bounded source paragraph keeps unchanged bytes for heading navigation.\n\n";
    const HEADING: &str = "# Deep title\n\n";
    const PARAGRAPHS: usize = 512;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("远端标题原来源续批.md");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(OPENING.as_bytes()).unwrap();
    for _ in 0..PARAGRAPHS {
        file.write_all(PARAGRAPH.as_bytes()).unwrap();
    }
    let expected_heading = OPENING.len() + PARAGRAPH.len() * PARAGRAPHS;
    file.write_all(HEADING.as_bytes()).unwrap();
    for _ in 0..PARAGRAPHS {
        file.write_all(PARAGRAPH.as_bytes()).unwrap();
    }
    file.sync_all().unwrap();
    drop(file); // 合成源逐片写入；生产打开前结束写句柄，不构造整篇期望 String。

    let mut doc = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
    let backing = doc.original_backing_observer().unwrap();
    let mut heading = doc.heading_anchor_request("deep-title").unwrap();
    let target = (0..1_000_000)
        .find_map(|_| {
            let step = heading.advance(&doc, 4096).unwrap();
            let work = match &step {
                HeadingStep::Progress { work }
                | HeadingStep::Found { work, .. }
                | HeadingStep::NotFound { work } => work,
            };
            // 查询的真实源片、复制和文件工作仍受原量子限制；找到标题不等于正文排版已完成。
            assert!(work.source_bytes <= 4096 && work.copied_bytes <= 4096);
            assert!(work.storage_bytes <= 4096 && work.parser_input_bytes <= 16 * 1024);
            match step {
                HeadingStep::Found { target, .. } => Some(target),
                HeadingStep::Progress { .. } => None,
                HeadingStep::NotFound { .. } => panic!("真实远端 ATX 标题必须存在"),
            }
        })
        .expect("有限合成源上的真实标题查询必须结束");
    assert_eq!(target.position_for(&doc).unwrap(), expected_heading);
    let selection = MarkdownTextSelection::caret(expected_heading);
    doc.set_selection(selection).unwrap();
    // 保留第一份公开结果跨越整个续批：淘汰后继不需要的 seed，不能破坏旧只读句柄。
    let (mut request, first) = leaf_sources(&doc, 0..1);
    let mut previous = None;
    for paragraph in 0..PARAGRAPHS {
        let start = OPENING.len() + paragraph * PARAGRAPH.len();
        let blank = start + PARAGRAPH.len() - 1;
        for (range, content) in [(start..start + 1, true), (blank..blank + 1, false)] {
            // 先查询段首，再经过真实空白控制。原 Global 可前探下一行，但不得丢掉其来源，
            // 也不能把512个已经过段落的 seed 留成下一批的历史目录。
            request
                .continue_forward(
                    &doc,
                    previous.as_ref().unwrap_or(&first),
                    range.clone(),
                    authority(&doc),
                )
                .unwrap();
            drop(previous.take()); // 只保第一份独立旧结果和当前结果，不人为积存全部发布。
            finish_sources(&mut request, &doc).unwrap();
            let current = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            if content {
                let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
                    .ordinary_semantics_for(&doc, authority(&doc))
                    .unwrap()
                else {
                    panic!("第{paragraph}段的真实来源与语义不能因历史库存耗尽而缺失");
                };
                assert_eq!(
                    leaf.query_for(&doc, authority(&doc), start)
                        .unwrap()
                        .unwrap()
                        .styles,
                    MarkdownPresentationStyles::default()
                );
            } // 空白不伪造正文样式；下一次段首查询必须重新取得该原行真正的语义结果。
            previous = Some(current);
        }
    }

    let title = expected_heading + 2;
    request
        .continue_forward(
            &doc,
            previous.as_ref().unwrap(),
            expected_heading..title + 1,
            authority(&doc),
        )
        .unwrap();
    drop(previous.take());
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("标题查询成功后，原正文语义仍须完成远端标题交集");
    };
    assert!(
        leaf.query_for(&doc, authority(&doc), title)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::HEADING_1)
    );
    let work = request.total_work();
    assert_eq!(work.copied_source_bytes, 0);
    assert_eq!(work.allocated_source_capacity, 0);
    // 取消唯一在途 owner 后，既有只读结果仍按原 Source/选择可读；取消不能复活请求。
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    let MarkdownOrdinarySemanticState::RootLeafReady(first_leaf) =
        first.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("原第一份不可变发布仍须有完整来源");
    };
    assert_eq!(
        first_leaf
            .query_for(&doc, authority(&doc), 0)
            .unwrap()
            .unwrap()
            .styles,
        MarkdownPresentationStyles::default()
    );
    assert_eq!(doc.selection(), selection);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.is_dirty() && !doc.can_undo() && !doc.can_redo());
    assert!(doc.original_file_identity_is_unchanged());
    // 磁盘期望也逐片独立核验；完整正文从未为了比较而进入一份 String 或 Vec。
    let mut file = std::fs::File::open(&path).unwrap();
    let mut bytes = [0; 128];
    for segment in std::iter::once(OPENING)
        .chain(std::iter::repeat_n(PARAGRAPH, PARAGRAPHS))
        .chain(std::iter::once(HEADING))
        .chain(std::iter::repeat_n(PARAGRAPH, PARAGRAPHS))
    {
        file.read_exact(&mut bytes[..segment.len()]).unwrap();
        assert_eq!(&bytes[..segment.len()], segment.as_bytes());
    }
    assert_eq!(file.read(&mut bytes[..1]).unwrap(), 0);
    drop((file, heading, target, request, first, current, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
    assert_eq!(backing.first_errno(), None);
}

#[test]
fn selection_reprojection_reuses_true_prefix_and_stops_at_later_inline() {
    // 原单段落先真实准备83K远端窗口，再改变完整有向选择返回首窗。未来的 ** 仍在同一
    // 闭合 seed 中，但初次 Plain proof 尚未读到它；重投影不能把 proven_end 扩成全文证明。
    let mut source = "中文abc😀".repeat(12_000);
    let suffix_start = source.encode_utf16().count();
    source.push_str(" **真实粗体**");
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let original_selection = MarkdownTextSelection {
        anchor_utf16: 83_055,
        head_utf16: 83_050,
    };
    doc.set_selection(original_selection).unwrap();
    let (mut request, old) = leaf_sources(&doc, 83_048..83_055);
    let MarkdownOrdinarySemanticState::RootLeafReady(old_leaf) =
        old.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("真实旧窗口必须已完成语义");
    };
    let new_selection = MarkdownTextSelection {
        anchor_utf16: 83_057,
        head_utf16: 83_069,
    };
    doc.set_selection(new_selection).unwrap();
    #[cfg(feature = "paged-line-compose")]
    assert!(
        request
            .selection_style_invariant(&doc, &old, authority(&doc))
            .unwrap()
            .is_some(),
        "原83K有限末端之前的Root Plain事实应能交给同源Composer核验"
    );
    assert!(
        request
            .reproject_selection(&doc, &old, 0..7, authority(&doc))
            .unwrap(),
        "真正完成来源坐标和发布的单物理行必须能沿原结构重投影"
    );
    // 保留旧公开句柄后选回同值，旧样式仍必须拒绝。这不是只靠当前数值不同的暂时失配，
    // 也不能让旧 readonly 句柄为新请求续权；canonical/history 全程保持原始文稿。
    doc.set_selection(original_selection).unwrap();
    assert!(matches!(
        old.ordinary_semantics_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    assert_eq!(
        old_leaf.query_for(&doc, authority(&doc), 83_050),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    doc.set_selection(new_selection).unwrap();
    drop(old);
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    // 每个后继仍使用4096原量子和本请求累计上限；实际完成全部83K小窗才证明 prefix
    // 能供后续用户样式消费，而不是只检验一个保留对象或一次true返回值。
    for start in (0..83_076).step_by(7) {
        let range = start..start + 7;
        if start != 0 {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            drop(previous);
            finish_sources(&mut request, &doc).unwrap();
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("新选区真实普通前缀必须完成，range={range:?}");
        };
        for index in range {
            assert_eq!(
                leaf.query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
        }
    }
    let suffix = suffix_start..doc.utf16_len();
    request
        .continue_forward(&doc, &previous, suffix.clone(), authority(&doc))
        .unwrap();
    drop(previous);
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("原证明末端之后的真实粗体仍须独立完成");
    };
    #[cfg(feature = "paged-line-compose")]
    assert!(
        request
            .selection_style_invariant(&doc, &current, authority(&doc))
            .unwrap()
            .is_none(),
        "未知后缀遇真实特殊语法后不能沿已释放的旧Plain证明扩大能力"
    );
    assert_oracle(leaf, &doc, &source, &suffix);
    assert!(
        leaf.query_for(&doc, authority(&doc), suffix_start + 3)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.selection(), new_selection);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((current, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn selection_reprojection_recomputes_real_markers_links_and_underlines() {
    let source = "中文 **粗体** <u>下划线😀</u> [链接](https://example.com/a?x=1&amp;y=2)\n\n尾\n";
    let (directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let requested = units(source, 0..source.find('\n').unwrap() + 1);
    let outside = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(outside).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, outside)
        .unwrap();
    super::inline::finish_inline(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let bold = units(
        source,
        source.find("粗体").unwrap()..source.find("粗体").unwrap(),
    )
    .start;
    let underline = units(
        source,
        source.find("下划线").unwrap()..source.find("下划线").unwrap(),
    )
    .start;
    let marker = units(
        source,
        source.find("**").unwrap()..source.find("**").unwrap(),
    )
    .start;
    let link = units(
        source,
        source.find("链接").unwrap()..source.find("链接").unwrap(),
    )
    .start;
    for selection in [
        MarkdownTextSelection {
            anchor_utf16: bold + 2,
            head_utf16: bold,
        },
        MarkdownTextSelection::caret(underline),
        outside,
    ] {
        doc.set_selection(selection).unwrap();
        assert!(
            request
                .reproject_selection(&doc, &previous, requested.clone(), authority(&doc))
                .unwrap()
        );
        drop(previous);
        super::inline::finish_inline(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("新选择必须通过原Parser重新决定实际marker显隐");
        };
        assert_oracle(leaf, &doc, source, &requested);
        assert_eq!(
            leaf.query_for(&doc, authority(&doc), marker)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            selection.head_utf16 != bold,
        );
        assert_eq!(
            leaf.link_at_for(&doc, authority(&doc), link)
                .unwrap()
                .unwrap()
                .destination
                .clone()
                .into_string(),
            "https://example.com/a?x=1&y=2"
        );
    }
    assert_eq!(doc.selection(), outside);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn root_code_selection_invariant_reprojects_real_leaf_and_retires_previous_publication() {
    // 四份原文分别保显式关闭/EOF、CRLF/缩进代码。正文中的 '</u>' 与 '**' 是
    // literal，不能改变外部 underline owner 或生成 inline marker；期望仍来自完整 Parser。
    for source in [
        "<u>外部\r\n\r\n  ```text\r\nliteral alpha 中文😀 **标记** </u><u> omega\r\n  ```\r\n\r\n外部关闭</u>\r\n",
        "前方 **真实粗体**\n\n```text\nliteral alpha 中文😀 **标记** omega",
        "前方 [链接](../target)\n\n    literal alpha 中文😀 **标记** omega\n\n后继段落\n",
        "前方\n\n    literal alpha 中文😀 **标记** omega",
    ] {
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let second = units(
            source,
            source.find("omega").unwrap()..source.find("omega").unwrap(),
        )
        .start;
        let requested = first..first + "literal".len();
        let original = MarkdownTextSelection::caret(first + 1);
        let changed = MarkdownTextSelection {
            anchor_utf16: second + 3,
            head_utf16: second,
        };
        doc.set_selection(original).unwrap();
        let (mut request, previous) = leaf_sources(&doc, requested.clone());

        // 独立解析旧/新选择的整份短夹具，证明其它 Root 块也不受这次正文内选择影响；
        // 生产能力只借同一个真实 Leaf，不能靠 requested 的7字符范围签整个 Store。
        let mut before = MarkdownViewportPresentation::default();
        before.set_active_selection(ordered_selection(original));
        before.rebuild(0, 0..doc.utf16_len(), source).unwrap();
        doc.set_selection(changed).unwrap();
        let mut after = MarkdownViewportPresentation::default();
        after.set_active_selection(ordered_selection(changed));
        after.rebuild(0, 0..doc.utf16_len(), source).unwrap();
        for index in 0..doc.utf16_len() {
            assert_eq!(
                before.query(index).unwrap().styles,
                after.query(index).unwrap().styles
            );
        }
        // 撤销、外来 Doc 和错误上下文沿原强类型错误返回，不能误降为“能力不存在”。
        let current = authority(&doc);
        let mut cancelled = current;
        cancelled.cancelled = true;
        assert!(matches!(
            request.selection_style_invariant(&doc, &previous, cancelled),
            Err(MarkdownTableRequestError::Cancelled)
        ));
        let mut wrong = current;
        wrong.context_revision += 1;
        assert!(matches!(
            request.selection_style_invariant(&doc, &previous, wrong),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        ));
        let (_foreign_directory, foreign) = document(source);
        assert!(matches!(
            request.selection_style_invariant(&foreign, &previous, authority(&foreign)),
            Err(MarkdownTableRequestError::SourceSuperseded)
        ));
        assert!(
            request
                .selection_style_invariant(&doc, &previous, current)
                .unwrap()
                .is_some()
        );
        assert!(
            request
                .reproject_selection(&doc, &previous, requested.clone(), current)
                .unwrap()
        );

        // 新选择接管后旧发布永久失权。恢复旧选择数值只能验证 ABA 拒绝，不能给旧 Rc 续权。
        doc.set_selection(original).unwrap();
        assert!(matches!(
            previous.ordinary_semantics_for(&doc, authority(&doc)),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        ));
        doc.set_selection(changed).unwrap();
        drop(previous);
        finish_sources(&mut request, &doc).unwrap();
        let current = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一个真实Code叶必须沿RestartLeaf重新签出有限语义");
        };
        assert_oracle(leaf, &doc, source, &requested);
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), changed);
        assert!(!doc.is_dirty() && !doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((current, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn root_code_selection_invariant_declines_real_marker_owner_edges_and_container_prefixes() {
    // 关闭行的两个空格不是 marker 字节，却属于该物理行的显隐 owner；不能仅排除反引号。
    // 缩进代码尾端可能就是后继 Root 段落起点，EOF 末端也保守排除，不以裸 close_reason 放行。
    for source in [
        "前方\r\n\r\n  ```text\r\nliteral alpha omega\r\n  ```\r\n\r\n后继\r\n",
        "前方\n\n```text\nliteral alpha omega",
        "前方\n\n    literal alpha omega\n\n后继\n",
        "前方\n\n    literal alpha omega",
    ] {
        let (_directory, mut doc) = document(source);
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let original = MarkdownTextSelection::caret(first + 1);
        doc.set_selection(original).unwrap();
        let (request, previous) = leaf_sources(&doc, first..first + 7);
        let last_body_byte = if let Some(closing) = source.rfind("  ```") {
            closing
        } else if let Some(next) = source.find("\n\n后继") {
            next + 1
        } else {
            source.len()
        };
        let body_end = units(source, last_body_byte..last_body_byte).start;
        for changed in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(body_end),
            MarkdownTextSelection {
                anchor_utf16: first + 1,
                head_utf16: 0,
            },
        ] {
            doc.set_selection(changed).unwrap();
            assert!(
                request
                    .selection_style_invariant(&doc, &previous, authority(&doc))
                    .unwrap()
                    .is_none()
            );
            // 只读拒绝没有退休原 publication；恢复真实旧选择后原有限样式仍可正常读取。
            doc.set_selection(original).unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("原Code窗口应保持Ready");
            };
            assert_oracle(leaf, &doc, source, &(first..first + 7));
        }
    }
    for source in [
        "> ```\n> literal alpha\n> omega\n> ```\n",
        "- ```\n  literal alpha\n  omega\n  ```\n",
    ] {
        let (_directory, mut doc) = document(source);
        let first = units(
            source,
            source.find("literal").unwrap()..source.find("literal").unwrap(),
        )
        .start;
        let second = units(
            source,
            source.find("omega").unwrap()..source.find("omega").unwrap(),
        )
        .start;
        doc.set_selection(MarkdownTextSelection::caret(first))
            .unwrap();
        let (request, previous) = leaf_sources(&doc, first..first + 7);
        doc.set_selection(MarkdownTextSelection::caret(second))
            .unwrap();
        // 容器的逐行 prefix 会随端点移动显隐；同形 Code、同 Leaf 与同 Root 参数均不能替代真scope。
        assert!(
            request
                .selection_style_invariant(&doc, &previous, authority(&doc))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn selection_reprojection_declines_missing_records_and_keeps_real_failure_authority() {
    let source = "首段 **粗体**\n\n后段 中文😀 abc 尾\n";
    let (_directory, mut doc) = document(source);
    let original = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(original).unwrap();
    let start = units(
        source,
        source.find("后段").unwrap()..source.find("后段").unwrap(),
    )
    .start;
    let (mut request, previous) = leaf_sources(&doc, start..start + 2);
    doc.set_selection(MarkdownTextSelection::caret(start))
        .unwrap();
    assert!(
        !request
            .reproject_selection(&doc, &previous, 0..1, authority(&doc))
            .unwrap()
    );
    // 资格缺失没有退休旧票或消费旧前沿：真实选择恢复后，原发布与原forward都继续有效。
    doc.set_selection(original).unwrap();
    assert!(
        previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .is_ok()
    );
    request
        .continue_forward(&doc, &previous, start + 2..start + 4, authority(&doc))
        .unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(
        current
            .ordinary_semantics_for(&doc, authority(&doc))
            .is_ok()
    );
    drop((current, request));

    // 取消、换代和异Doc都是真实失权，不能因前面的目标不在记录中而误返回false；
    // 首因固定后，恢复数值参数也不得恢复已经结束的请求。
    for cause in [
        MarkdownTableRequestError::Cancelled,
        MarkdownTableRequestError::SourceSuperseded,
    ] {
        let (_directory, mut current_doc) = document(source);
        let backing = current_doc.original_backing_observer().unwrap();
        current_doc.set_selection(original).unwrap();
        let (mut request, previous) = leaf_sources(&current_doc, start..start + 2);
        let mut current_authority = authority(&current_doc);
        if cause == MarkdownTableRequestError::Cancelled {
            current_authority.cancelled = true;
        } else {
            current_doc
                .replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            current_authority = authority(&current_doc);
        }
        assert_eq!(
            request.reproject_selection(&current_doc, &previous, 0..1, current_authority),
            Err(cause)
        );
        assert_eq!(
            request.reproject_selection(&current_doc, &previous, 0..1, authority(&current_doc)),
            Err(cause)
        );
        drop((previous, request, current_doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
    let (mut request, previous) = leaf_sources(&doc, start..start + 2);
    let (_foreign_directory, foreign) = document(source);
    assert_eq!(
        request.reproject_selection(&foreign, &previous, 0..1, authority(&foreign)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn plain_selection_prefix_declines_markup_and_preserves_original_publication() {
    // 每个负例仍经过真实 Global/Leaf/Parser 完成并移交输出；不能靠构造一个空目录
    // 测出 None。能力缺失后恢复原选择，原公开语义与正常重投影继续工作，证明没有偷偷退休。
    for source in [
        "普通 **粗体** 尾\n",
        "<u>下划线😀</u> 尾\n",
        "<div>真实HTML</div>\n",
        "## 标题😀\n",
        "> 引用普通正文\n",
        "| 列 |\n| --- |\n| 值 |\n",
    ] {
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let original = MarkdownTextSelection::caret(doc.utf16_len());
        doc.set_selection(original).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, original)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let changed = MarkdownTextSelection {
            anchor_utf16: 2,
            head_utf16: 0,
        };
        doc.set_selection(changed).unwrap();
        assert!(
            request
                .selection_style_invariant(&doc, &previous, authority(&doc))
                .unwrap()
                .is_none(),
            "有真实marker/HTML/容器/table的语义不能保普通字形，source={source:?}"
        );
        doc.set_selection(original).unwrap();
        let semantic = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap();
        if let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = semantic {
            for covered in leaf.covered_ranges_for(&doc, authority(&doc)).unwrap() {
                assert_oracle(leaf, &doc, source, covered);
            }
        }
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((previous, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn plain_selection_prefix_keeps_real_source_cancellation_and_context_errors() {
    let source = "中文 abc😀 真实普通前缀 尾";
    let (_directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let original = MarkdownTextSelection::caret(doc.utf16_len());
    doc.set_selection(original).unwrap();
    let (mut request, previous) = leaf_sources(&doc, 0..2);
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: 4,
        head_utf16: 0,
    })
    .unwrap();
    let current = authority(&doc);
    let (_foreign_directory, foreign) = document(source);
    let mut cancelled = current;
    cancelled.cancelled = true;
    let mut wrong_context = current;
    wrong_context.context_revision += 1;
    for (document, authority, expected) in [
        (&doc, cancelled, MarkdownTableRequestError::Cancelled),
        (
            &doc,
            wrong_context,
            MarkdownTableRequestError::PublicationSuperseded,
        ),
        (
            &foreign,
            authority(&foreign),
            MarkdownTableRequestError::SourceSuperseded,
        ),
    ] {
        assert!(matches!(
            request.selection_style_invariant(document, &previous, authority),
            Err(cause) if cause == expected
        ));
    }
    // 只读能力检查不修改原请求；错误参数移除后仍能按真正新选择重投影同一已发布窗口。
    assert!(
        request
            .selection_style_invariant(&doc, &previous, current)
            .unwrap()
            .is_some()
    );
    assert!(
        request
            .reproject_selection(&doc, &previous, 0..2, current)
            .unwrap()
    );
    doc.set_selection(original).unwrap();
    assert!(matches!(
        previous.ordinary_semantics_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    doc.set_selection(current.selection).unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = current
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("原请求在拒绝外来参数之后仍须真实完成新选择发布");
    };
    assert_oracle(leaf, &doc, source, &(0..2));
    doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(matches!(
        request.selection_style_invariant(&doc, &current, authority(&doc)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    drop((current, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_small_leaf_windows_keep_far_directed_selection_under_original_work_limit() {
    // 固定普通单段落混合中文、ASCII和非BMP；每组13B/7UTF16，完全没有Markdown marker。
    // 远端有向选择是真实用户状态，后继只滚动小窗口，不能每批重读其两个已认证端点。
    let source = "中文abc😀".repeat(16_384);
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 83_055,
        head_utf16: 83_050,
    }; // 两端由固定7-unit组声明：a的起点到完整emoji末端，不落在代理对内部。
    doc.set_selection(selection).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(0..7, 57, selection)
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    // 连续真实发布直到远端选区后的完整组；不能只到13K/18K就外推产品83K定位。
    // 每个窗口仍只消费原4096量子和同一请求累计fuel，没有扩大语义视口或提前解析全文。
    // 不读取私有定位器、不按调用数判断优化，能否完成全部真实样式就是本回归的结果。
    for block in 0..=selection.anchor_utf16 / 7 {
        let range = block * 7..block * 7 + 7;
        if block != 0 {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            drop(previous); // 原Ready联合准入后立即归还，不人为保留整串publication。
            finish_sources(&mut request, &doc).unwrap_or_else(|cause| {
                panic!(
                    "真实小窗未完成：range={range:?} cause={cause:?} work={:?}",
                    request.total_work(),
                );
            });
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一真实普通段落的小窗必须完成样式，range={range:?}");
        };
        // 同一发布的结构层只证明无表：连续小窗必须各自裁剪真实 Gap，不能把整个段落
        // 或上一窗的覆盖当成本窗结果；真正的默认样式仍由下方原 Leaf 查询独立证明。
        let table = previous.table_sources_for(&doc, authority(&doc)).unwrap();
        assert!(table.windows_for(&doc).unwrap().next().is_none());
        let [
            crate::markdown_document_buffer::MarkdownTableViewportCoverage::Gap {
                utf16_range, ..
            },
        ] = table.coverage_for(&doc).unwrap()
        else {
            panic!("普通段落小窗必须保留自己的真实无表分区");
        };
        assert_eq!(utf16_range, &range);
        for local in [0, 1, 2, 3, 4, 5] {
            let at = range.start + local;
            let query = leaf.query_for(&doc, authority(&doc), at).unwrap().unwrap();
            assert!(query.utf16_range.contains(&at));
            assert_eq!(query.styles, MarkdownPresentationStyles::default());
        } // 每个固定scalar必须真正有覆盖；未知页不能借空样式或裸viewport范围通过。
    }
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.selection(), selection);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_structure_and_gap_pruning_preserve_real_cell_semantics_after_reposition() {
    // 原 Global 先交普通 Gap，再交真正结构表；每次只请求行首一个单元，不把物理行截短。
    // 最后从尾 Gap 返回粗体格，必须沿公开 reposition 恢复原来源与内联语义。
    let source =
        "前段甲😀乙\n\n| H |\n| --- |\n| **粗体** |\n| *斜体* |\n| `代码` |\n\n尾段甲😀乙xyz\n";
    let (directory, mut doc) = document(source);
    let backing = doc.original_backing_observer().unwrap();
    let (mut request, mut previous) = leaf_sources(&doc, 0..1);
    let mut previous_start = 0;
    let header_byte = source.find("| H |").unwrap();
    let tail = source.find("尾段").unwrap();
    // None是实际普通尾段；其余固定原行既核公开物理身份，也核实际每格的强/弱/代码样式。
    // 重复的粗体行有明确用户行为意义：它位于尾段之前，不能沿前向游标假签旧行。
    for cell in [
        Some((
            "| **粗体** |\n",
            "粗体",
            1,
            MarkdownPresentationStyles::STRONG,
        )),
        Some((
            "| *斜体* |\n",
            "斜体",
            2,
            MarkdownPresentationStyles::EMPHASIS,
        )),
        Some((
            "| `代码` |\n",
            "代码",
            3,
            MarkdownPresentationStyles::INLINE_CODE,
        )),
        None,
        Some((
            "| **粗体** |\n",
            "粗体",
            1,
            MarkdownPresentationStyles::STRONG,
        )),
    ] {
        let byte_start = cell.map_or(tail, |(line, _, _, _)| source.find(line).unwrap());
        let start = units(source, byte_start..byte_start).start;
        let range = start..start + 1;
        if start < previous_start {
            request
                .reposition(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
        } else {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
        }
        drop(previous); // 已准入的旧发布归还后，仍只推进原唯一请求及累计额度。
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(
            previous.requested_for(&doc, authority(&doc)).unwrap(),
            &range
        );
        let Some((line, word, ordinal, expected)) = cell else {
            // 表已离开当前窗口时，结构表目录不能被空阶段选择遗漏，仍须真正裁掉。
            assert!(
                previous
                    .table_row_at_for(&doc, authority(&doc), start)
                    .unwrap()
                    .is_none()
            );
            let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("表后的真实普通段落必须有当前覆盖");
            };
            assert_eq!(
                leaf.query_for(&doc, authority(&doc), start)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
            previous_start = start;
            continue;
        };
        let row = previous
            .table_row_at_for(&doc, authority(&doc), start)
            .unwrap()
            .unwrap();
        assert_eq!(
            row.coordinate_for(&doc, authority(&doc)).unwrap(),
            (header_byte, ordinal, 1)
        );
        assert_eq!(
            row.physical_utf16_range_for(&doc, authority(&doc)).unwrap(),
            units(source, byte_start..byte_start + line.len())
        );
        request
            .prepare_cell_inline(&doc, row, authority(&doc), 0)
            .unwrap();
        // 单格真正借原 scope/raw source/Parser 完成；结构行 Ready 不能代替实际字符样式。
        let mut complete = false;
        for _ in 0..source.len() * 100 {
            let step = request
                .advance_cell_inline(&doc, authority(&doc), 4096)
                .unwrap();
            let (MarkdownTableCellInlineStep::Progress { work }
            | MarkdownTableCellInlineStep::Ready { work }) = step;
            assert!(work.source_bytes <= 4096 && work.structural_transitions <= 4096);
            if matches!(step, MarkdownTableCellInlineStep::Ready { .. }) {
                complete = true;
                break;
            }
        }
        assert!(complete, "有限真实格必须完成全部内联语义");
        let inline = request
            .take_cell_inline_ready_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let word_byte = byte_start + line.find(word).unwrap();
        let at = units(source, word_byte..word_byte).start;
        let query = inline
            .query_for(&doc, authority(&doc), at)
            .unwrap()
            .unwrap();
        assert!(query.utf16_range.contains(&at) && query.styles.contains(expected));
        previous_start = start;
    }
    // 在恢复的非空结构页上取消后继，旧真实发布仍可读，但任务不能再产生新来源。
    request
        .continue_forward(
            &doc,
            &previous,
            units(source, tail..tail + "尾".len()),
            authority(&doc),
        )
        .unwrap();
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(
        previous
            .table_row_at_for(&doc, authority(&doc), previous_start)
            .unwrap()
            .is_some()
    );
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    // 真实编辑再Undo虽恢复字面值，仍是新代次；已取消的请求和旧结构发布都不能补签新来源。
    doc.replace_utf16_range(0..1, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(doc.generation() > 0 && doc.can_redo() && !doc.is_dirty());
    assert_eq!(
        previous
            .table_row_at_for(&doc, authority(&doc), previous_start)
            .err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    // 所有请求、原 publication 与文档都归还后，再核真实映射；不观察私有 phase 或句柄计数。
    drop((previous, request, doc));
    assert!(backing.unmap_succeeded() && !backing.failed());
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn forward_leaf_windows_keep_scalar_floor_and_ceil_across_pieces_and_reposition() {
    // 每组固定10B/4UTF16：甲、完整emoji、乙。窗口故意停在代理对中间，
    // 公开语义必须仍剪回原请求，不能把前一ceil已经读过的后半代理项当成新floor。
    let source = "甲😀乙".repeat(900);
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let selection = MarkdownTextSelection {
        anchor_utf16: 3_599,
        head_utf16: 1,
    };
    doc.set_selection(selection).unwrap();
    let mut request = doc
        .viewport_leaf_source_request(0..2, 57, selection)
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let mut previous_start = 0;
    // 409/410和819/820组跨过原4KiB读片边界；端点由固定字面长度决定，
    // 不读取定位器或候选Source的私有片目录来反造预期。
    for block in [0, 409, 410, 819, 820, 899] {
        let start = block * 4;
        for range in [
            start..start + 2,
            start + 1..start + 2,
            start + 1..start + 3,
            start + 2..start + 4,
        ] {
            request
                .continue_forward(&doc, &previous, range.clone(), authority(&doc))
                .unwrap();
            finish_sources(&mut request, &doc).unwrap_or_else(|cause| {
                panic!(
                    "真实UTF16窗口未完成：range={range:?} cause={cause:?} work={:?}",
                    request.total_work()
                );
            });
            let current = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = current
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("固定普通正文必须有真实语义，range={range:?}");
            };
            for index in range.clone() {
                let query = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap();
                assert!(
                    range.start <= query.utf16_range.start && query.utf16_range.end <= range.end
                );
                assert!(query.utf16_range.contains(&index));
                assert_eq!(query.styles, MarkdownPresentationStyles::default());
            }
            // 新窗口实际发布后，仍持有的旧Ready必须继续按自己的原覆盖读取；
            // 复用owner不能改写旧结果，也不能把旧publication强留成链。
            let MarkdownOrdinarySemanticState::RootLeafReady(old) = previous
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("上一普通窗口必须仍可读");
            };
            assert_eq!(
                old.query_for(&doc, authority(&doc), previous_start)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::default()
            );
            previous = current;
            previous_start = range.start;
        }
    }
    // 真正逆向必须走原重定位，而非沿原前沿倒退；其结果仍保留同一完整有向选择。
    request
        .reposition(&doc, &previous, 1..3, authority(&doc))
        .unwrap();
    drop(previous);
    finish_sources(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = previous
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("逆向恢复必须重新取得真实emoji覆盖");
    };
    for index in 1..3 {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .utf16_range,
            1..3
        );
    }
    request
        .continue_forward(&doc, &previous, 1_636..1_640, authority(&doc))
        .unwrap();
    request.cancel();
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(ready.query_for(&doc, authority(&doc), 1).unwrap().is_some());
    assert_eq!(doc.selection(), selection);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((previous, request, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn production_literal_html_and_metadata_keep_original_values_and_exact_underline_allowlist() {
    for source in [
        "<div>\n**字面粗体** `字面代码` [字面链接](https://example.com) <a href=\"https://example.com\">网页字面</a>\n<u>实际下划线😀</u> <U>大写字面</U> <u class=x>属性字面</u>\n</div>\n",
        "  <div>\n**字面粗体** `字面代码` [字面链接](https://example.com)\n<u>实际下划线😀</u>\n</div>\n",
        "<!-- **字面粗体** `字面代码` [字面链接](https://example.com) <u>实际下划线😀</u> -->\n",
        "<script>\n**字面粗体** `字面代码` [字面链接](https://example.com) <u>实际下划线😀</u>\n</script>\n",
        "> <div>\r\n> **字面粗体** `字面代码` [字面链接](https://example.com)\r\n> <u>实际下划线😀</u>\r\n> </div>\r\n",
        "- <div>\n  **字面粗体** `字面代码` [字面链接](https://example.com)\n  <u>实际下划线😀</u>\n  </div>\n",
        "> - <div>\n>   **字面粗体** `字面代码` [字面链接](https://example.com)\n>   <u>实际下划线😀</u>\n>   </div>\n",
        "---\ntitle: '**字面粗体** `字面代码` [字面链接](https://example.com)'\nvalue: '<u>实际下划线😀</u>'\n---\n",
        "---\r\ntitle: '**字面粗体** `字面代码` [字面链接](https://example.com)'\r\nvalue: '<u>实际下划线😀</u>'\r\n...  \r\n",
    ] {
        // 独立完整 Parser 决定真实叶种类和末端；metadata closing EOL 不属于该叶，
        // 不能为了让整窗 Ready 把 consumed_end 或后继行前缀伪装成已证明字面。
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(event, Event::Start(Tag::HtmlBlock | Tag::MetadataBlock(_))).then_some(range)
        })
        .expect("夹具必须由原 Parser 真正识别为 HTML 或 metadata 叶");
        let requested = units(source, 0..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实字面叶须完成原字面与 allowlist 语义，source={source:?}");
            };
            assert_oracle(ready, &doc, source, &requested);

            // 源码值也由同一个公开文档流式写出；样式 Ready 不能意味着删掉 HTML、
            // YAML marker、Markdown 拼写或容器前缀，更不能产生一个伪链接目标。
            let mut actual = Vec::new();
            doc.write_utf16_range_to(requested.clone(), &mut actual)
                .unwrap();
            assert_eq!(actual, source.as_bytes()[..parsed.end]);
            for literal in ["字面粗体", "字面代码", "字面链接"] {
                let at = source.find(literal).unwrap();
                for index in units(source, at..at + literal.len()) {
                    let styles = ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles;
                    for unexpected in [
                        MarkdownPresentationStyles::STRONG,
                        MarkdownPresentationStyles::INLINE_CODE,
                        MarkdownPresentationStyles::LINK,
                    ] {
                        assert!(!styles.contains(unexpected));
                    }
                    assert!(
                        ready
                            .link_at_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            let underline = source.find("实际下划线😀").unwrap();
            for index in units(source, underline..underline + "实际下划线😀".len()) {
                assert!(
                    ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles
                        .contains(MarkdownPresentationStyles::UNDERLINE)
                );
            }
            // 非精确标签绝不进入 allowlist；这些值仍与原 Parser 一样保持可见字面。
            for literal in ["网页字面", "大写字面", "属性字面"] {
                if let Some(at) = source.find(literal) {
                    let index = units(source, at..at).start;
                    let styles = ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles;
                    assert!(!styles.contains(MarkdownPresentationStyles::UNDERLINE));
                    assert!(
                        ready
                            .link_at_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_giant_literal_html_keeps_real_underline_and_only_publishes_viewport() {
    // 完整合成串仅归测试 oracle；生产只沿原映射和同一 Global 的 4KiB 量子，
    // 不能因 HTML 正文没有 Markdown inline 就复制超过 2MiB 的整段来查找 </u>。
    let opening = "<div><u>\n";
    let source = format!("{opening}{}\n</u></div>\n", "中😀".repeat(310_000));
    let (directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let byte_start = opening.len() + 170_000 * "中😀".len();
    let byte_end = byte_start + 19 * "中😀".len();
    let requested = units(&source, byte_start..byte_end);
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("巨型 HTML 中间窗口须等待真实 allowlist 配对后完成，不得直接签默认样式");
    };
    assert_oracle(ready, &doc, &source, &requested);
    for index in requested.clone() {
        assert!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::UNDERLINE)
        );
    }
    let mut actual = Vec::new();
    doc.write_utf16_range_to(requested, &mut actual).unwrap();
    assert_eq!(actual, source.as_bytes()[byte_start..byte_end]);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_scoped_leaf_prefixes_headings_and_code_match_original_parser() {
    for source in [
        "> ## 引用标题😀\n",
        "- ## 列表标题😀\n",
        "> - ## 引用列表标题😀\n",
        "- > ## 列表引用标题😀\n",
        "1. > ## 有序列表引用标题😀\n",
        "> -\t## Tab列表标题😀\n",
        "> ```text\r\n> 中文😀\r\n> 第二行\r\n> ```\r\n",
        "- ```text\n  中文😀\n  \t保留Tab\n  ```\n",
        ">     中文😀\n>     第二行\n",
        "> ````\n> 正文\n> ```",
        "> ```\n> 正文\n> ~~~",
        "> ```\n> 正文\n> ```不是closer",
        "> ```\n> 真实容器退出\n退出正文\n",
    ] {
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(event, Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_))).then_some(range)
        })
        .unwrap();
        // 首行实际 quote/item prefix 也必须得到真容器语义；不能只核 marker 后的正文。
        let requested = units(source, 0..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实容器 Leaf 必须完成，source={source:?}");
            };
            assert_oracle(ready, &doc, source, &requested);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_footnote_leaf_origins_keep_labels_literal_and_real_child_scope() {
    for source in [
        "[^123]: ## 中文标题😀\n",
        "> [^123]: ## 引用脚注标题😀\n",
        "[^外层]: > [^456]: ## 嵌套脚注标题😀\n",
        "[^123]: ```text\n    中文😀<u>代码字面</u>\n    ```\n",
        "> [^123]: ```text\r\n>     中文😀<u>代码字面</u>\r\n>     ```\r\n",
        "[^123]:\n        缩进代码😀\n",
        "[^123]: <div>\n    **字面粗体** <u>实际下划线😀</u>\n    </div>\n",
    ] {
        let parsed = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            matches!(
                event,
                Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::HtmlBlock)
            )
            .then_some(range)
        })
        .expect("独立原 Parser 必须确认真实脚注子叶");
        // 空 opening 行不是下一代码叶的内容；首行 label 只有与实际 child 同一物理行才随前缀发布。
        let physical = source[..parsed.start].rfind('\n').map_or(0, |at| at + 1);
        let requested = units(source, physical..parsed.end);
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实脚注 child 必须保原 opening 和 Origin，source={source:?}")
            };
            assert_oracle(ready, &doc, source, &requested);
            assert_eq!(
                std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
                source.as_bytes()
            );
            drop(sources);
            drop(request);
            drop(doc);
            assert_eq!(observer.mapped_bytes(), 0);
            assert_eq!(observer.managed_resident_bytes(), 0);
        }
    }
}

#[test]
fn production_giant_footnote_code_keeps_mapped_body_and_only_visible_semantics() {
    // 标签 opening、四列续行和真围栏都由原 producer/Origin 证明；>2MiB 正文只有测试 oracle 持有。
    // 当前窗口不含首行 label，也不能因为它离屏就换一份 Root 源、配额或整段缓冲。
    let source = format!("[^123]: ```text\n    {}\n    ```\n", "中😀".repeat(310_000));
    let (directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let requested = 500_001..500_065;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("真实脚注代码必须沿原映射发布可见交集，不能复制整块或跳过其祖先");
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_root_eof_false_closers_remain_literal_without_losing_code_coverage() {
    for source in [
        "````\n正文\n```",
        "```\n正文\n~~~",
        "```\n正文\n```不是closer",
    ] {
        let (directory, doc) = document(source);
        let observer = doc.original_backing_observer().unwrap();
        let requested = units(source, first_leaf_range(source));
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("EOF 假围栏属于真实代码正文，不能因旧末行猜测拒绝整块");
        };
        assert_oracle(ready, &doc, source, &requested);
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop(sources);
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
fn production_scoped_giant_code_publishes_only_viewport_without_copying_canonical_body() {
    // 只有独立 oracle 测试持完整合成串；生产请求只借原映射小帧，单段代码超过 2MiB。
    let source = format!("> ```text\n> {}\n> ```\n", "中😀".repeat(310_000));
    let (directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(0)).unwrap();
    let requested = 500_001..500_065;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("巨大容器代码仍须发布可见交集，不能整段复制或把合法块留作 Pending");
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_leaf_factory_uses_original_publication_and_matches_complete_projection() {
    for source in [
        "前言\n\n```rust\n中文😀<u>代码</u>\n```\n\n结尾\n",
        "前言\r\n\r\n  ~~~~text\r\n中文😀\r\n  ~~~~~ \r\n\r\n结尾\r\n",
        "前言\n\n    中文😀\n    第二行\n\n结尾\n",
        "前言\n\n  ***  \n\n结尾\n",
        "前言\n\n```text\n真实EOF中文😀",
        "<u>远处开标记\n\n```text\n中文😀\n```\n\n远处闭标记</u>\n",
    ] {
        let requested = units(source, first_leaf_range(source));
        for selection in [
            MarkdownTextSelection::caret(requested.start),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (_directory, mut doc) = document(source);
            doc.set_selection(selection).unwrap();
            let (request, mut sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("公开工厂的真实 Root 代码块或分隔线必须产生有限语义");
            };
            assert_oracle(ready, &doc, source, &requested);
            assert!(
                sources
                    .records
                    .iter()
                    .any(|record| Rc::ptr_eq(record, &ready.coverage.get(0).unwrap().record))
            );
            assert!(request.semantic.is_none());
            assert!(request.records.is_empty());
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);

            // 取出实际 Ready 后结束原 SourcePublication，孤立的语义不能重新借相同 g 取得权限。
            let state = std::mem::replace(
                &mut sources.semantic,
                MarkdownOrdinarySemanticState::SourcesOnly,
            );
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = state else {
                unreachable!();
            };
            drop(sources);
            assert_eq!(
                ready.query_for(&doc, authority(&doc), requested.start),
                Err(MarkdownTableRequestError::PublicationSuperseded)
            );
        }
    }
}

#[test]
/// closing-EOL 的真实控制票不扩张原代码语义；只请求 EOL 时也沿原公开工厂的同一 Root 来源。
fn production_closing_fence_separator_keeps_its_own_publication_and_original_plain_style() {
    for ending in ["\n", "\r\n"] {
        let source =
            format!("前言😀{ending}{ending}```text{ending}代码😀{ending}```  {ending}后文{ending}");
        let end = source.rfind("```  ").unwrap() + 5;
        let separator = units(&source, end..end + ending.len());
        for requested in [0..source.encode_utf16().count(), separator.clone()] {
            let (_directory, mut doc) = document(&source);
            let (request, sources) = leaf_sources(&doc, requested);
            assert_eq!(
                sources
                    .root_fence_separator_for(&doc, authority(&doc), &separator)
                    .unwrap(),
                Some(separator.clone())
            );
            let mut original = MarkdownViewportPresentation::default();
            original
                .rebuild(doc.generation(), 0..doc.utf16_len(), &source)
                .unwrap();
            for index in separator.clone() {
                // 旧完整 Parser 在 closing scanner 外确实只发布普通样式，不能借最后代码/隐藏 run 的字体。
                assert_eq!(
                    original.query(index).unwrap().styles,
                    MarkdownPresentationStyles::default()
                );
                if let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &sources.semantic {
                    assert!(
                        leaf.query_for(&doc, authority(&doc), index)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            for other in [
                separator.start - 1..separator.end,
                separator.end..separator.end + 1,
                separator.end..separator.end,
            ] {
                assert!(
                    sources
                        .root_fence_separator_for(&doc, authority(&doc), &other)
                        .unwrap()
                        .is_none()
                );
            }
            if ending.len() == 2 {
                for half in [
                    separator.start..separator.start + 1,
                    separator.start + 1..separator.end,
                ] {
                    assert!(
                        sources
                            .root_fence_separator_for(&doc, authority(&doc), &half)
                            .unwrap()
                            .is_none()
                    );
                } // 同一 CRLF 不拆成两份控制权限，也不在 getter 读取任何正文。
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
            doc.insert_utf16(0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            assert_eq!(
                sources.root_fence_separator_for(&doc, authority(&doc), &separator),
                Err(MarkdownTableRequestError::SourceSuperseded)
            ); // 源换代后即使仍是相同数字范围，也不能复活旧 closing-EOL 控制票。
        }
    }

    let source = "> ```text\n> 代码\n> ```\n\n尾\n";
    let (_directory, doc) = document(source);
    let (_, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let end = source.rfind("```").unwrap() + 3;
    assert!(
        sources
            .root_fence_separator_for(&doc, authority(&doc), &units(source, end..end + 1))
            .unwrap()
            .is_none()
    ); // 容器自己的前缀与换行归原容器；不能借同形 closing marker 冒充 Root 控制。
}

#[test]
/// 原 Native 字形、EOL、空行的真实续批顺序不能消费后丢失下一份 fence/heading Fact。
fn production_native_control_windows_keep_the_original_next_leaf_across_blank_lines() {
    let source = "# Title\n\n```text\nlet value = 1;\n```\n\n## Next\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..7);
    let global = request.batch.source_probe_state().0;
    let mut ahead = None;
    for requested in [
        7..8,
        8..9,
        9..16,
        16..17,
        17..31,
        31..32,
        32..35,
        35..36,
        36..37,
        37..44,
        44..45,
    ] {
        request
            .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
            .unwrap();
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(request.batch.source_probe_state().0, global);
        if requested == (8..9) || requested == (36..37) {
            assert_eq!(previous.records.len(), 1);
            ahead = Some(Rc::downgrade(&previous.records[0]));
            assert!(
                previous
                    .ordinary_leaves_for(&doc, authority(&doc))
                    .unwrap()
                    .next()
                    .is_none()
            ); // 原下一叶的 metadata 只为续体保留；本空白窗口没有该叶的 getter 或样式权限。
            continue;
        }
        if requested == (9..16) || requested == (37..44) {
            let actual = previous.records.first().unwrap();
            assert!(ahead.take().unwrap().ptr_eq(&Rc::downgrade(actual)));
        } // 后继借回的必须是原 Fact 的同一个 Rc，不重判、复制或重新准入另一份 opaque 叶。
        if requested == (35..36) {
            assert_eq!(
                previous
                    .root_fence_separator_for(&doc, authority(&doc), &requested)
                    .unwrap(),
                Some(requested)
            );
            continue;
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            panic!("合法原 Native 范围 {requested:?} 未沿同一来源完成 RootLeaf 语义");
        };
        assert_oracle(leaf, &doc, source, &requested);
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// 窗后普通行也须保同一语义 seed；只保行坐标会让下一窗口再次丢失真实 inline 来源。
fn production_blank_control_preserves_the_next_ordinary_line_and_its_original_seed() {
    let source = "# Title\n\nplain words\n\n## Next\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..8);
    request
        .continue_forward(&doc, &previous, 8..9, authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_eq!(previous.records.len(), 1);
    let SourceRecord::Line(line) = previous.records[0].as_ref() else {
        panic!("该固定空行之后必须保原普通行，不是另一种叶块");
    };
    let seed = Rc::downgrade(line.semantic_block.as_ref().unwrap());
    assert!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_none()
    );
    request
        .continue_forward(&doc, &previous, 9..20, authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let line = current
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .next()
        .unwrap();
    assert!(seed.ptr_eq(&Rc::downgrade(line.semantic_block.as_ref().unwrap())));
    let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &current.semantic else {
        panic!("原普通行不能因上次窗口只有空白而失去语义 seed");
    };
    assert_oracle(leaf, &doc, source, &(9..20));
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
}

#[test]
/// 正文/EOL/Setext标记/EOL由Native分开取样时，同一 terminal 来源仍须授权原完整 heading 尾部。
fn production_native_setext_control_windows_keep_the_original_terminal_owner_through_underline() {
    let source = "outside\n\nHeading alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\n---\n\nend\n";
    let (_directory, doc) = document(source);
    let (mut request, mut previous) = leaf_sources(&doc, 0..7);
    let global = request.batch.source_probe_state().0;
    let mut heading = None;
    for requested in [
        7..8,
        8..9,
        9..114,
        114..115,
        115..118,
        118..119,
        119..120,
        120..123,
        123..124,
    ] {
        request
            .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
            .unwrap();
        finish_sources(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert_eq!(request.batch.source_probe_state().0, global);
        if requested == (8..9) {
            heading = Some(Rc::downgrade(&previous.records[0]));
            continue;
        }
        if requested == (119..120) {
            continue; // 真实空行没有 heading 样式，不借后继普通行补签已证交集。
        }
        if requested.start >= 9 && requested.end <= 119 {
            assert!(
                heading
                    .as_ref()
                    .unwrap()
                    .ptr_eq(&Rc::downgrade(&previous.records[0]))
            );
        }
        if requested.start >= 115 && requested.end <= 119 {
            assert!(
                previous
                    .ordinary_lines_for(&doc, authority(&doc))
                    .unwrap()
                    .next()
                    .is_none()
            ); // underline 由原 Setext seed 证明；不能捏造一条正文 BlockOriginLine。
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            panic!("原 Setext 分段范围 {requested:?} 丢失自己的 terminal owner");
        };
        assert_oracle(leaf, &doc, source, &requested);
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_leaf_giant_line_has_finite_runs_without_copy_or_parser_input() {
    let source = format!("前言\n\n```text\n{}\n```\n\n尾\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let requested = 50_000..50_017;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("巨型单行不能因为整个块超过窗口大小而失去有限代码语义");
    };
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    assert_eq!(ready.run_count(), 1);
    for index in requested {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            MarkdownPresentationStyles::FENCED_CODE
        );
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// 原首行物理空白与正文属于同一Root代码owner；Composer不能把可见缩进替换成语义空洞。
fn production_indented_code_keeps_the_original_physical_prefix_in_semantic_coverage() {
    let source = "outside\n\n    <u>中文😀</u>\n\n";
    let (_directory, doc) = document(source);
    let (_request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let leaf = sources
        .records
        .iter()
        .find_map(|record| match record.as_ref() {
            SourceRecord::Leaf(leaf) if leaf.kind() == LeafBlockKind::IndentedCode => Some(leaf),
            _ => None,
        })
        .expect("真实Global必须识别缩进代码块");
    assert_eq!(leaf.physical_start(), 9);
    assert_eq!(leaf.source_range().start, 13);
    assert_eq!(leaf.block.scope_depth(), 0);
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("普通Root和缩进代码应完成真实有限语义");
    };
    for position in 9..13 {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), position)
                .unwrap()
                .expect("Composer实际请求的原四格缩进不能成为未授权空洞")
                .styles,
            MarkdownPresentationStyles::default()
        );
    }
}

#[test]
/// 请求只碰首行前缀时仍需原Root代码票；范围剪裁和Tab不能丢字符、借邻块或越过默认样式。
fn production_root_code_prefix_clipping_matches_the_original_parser_without_copying() {
    for (source, prefix) in [
        ("outside\n\n    <u>中文😀</u>\n\n", 9..13),
        ("outside\n\n  ```text\n<u>中文😀</u>\n  ```\n\n", 9..11),
        ("outside\r\n\r\n\t<u>中文😀</u>\r\n\r\n", 11..12),
    ] {
        let (_directory, doc) = document(source);
        let prefix = units(source, prefix);
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        for requested in [
            0..doc.utf16_len(),
            prefix.clone(),
            prefix.end - 1..prefix.end,
        ] {
            let (request, sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
                panic!("首行物理前缀必须沿原Root代码来源完成");
            };
            for position in prefix.start.max(requested.start)..prefix.end.min(requested.end) {
                let query = ready
                    .query_for(&doc, authority(&doc), position)
                    .unwrap()
                    .expect("视口内真实缩进字符没有语义覆盖");
                assert_eq!(query.styles, MarkdownPresentationStyles::default());
                assert_eq!(query.styles, baseline.query(position).unwrap().styles);
            }
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
        }
    }
}

#[test]
fn production_underline_probe_restores_future_code_facts_for_the_same_forward_owner() {
    let source = "<u>外部开标签\r\n\r\n标题一😀\r\n===\r\n\r\n```text\r\n</u><u>代码😀\r\n```\r\n\r\n标题二\r\n---\r\n\r\n    </u><u>缩进代码\r\n\r\n标题三\r\n===\r\n\r\n外部真关闭</u>\r\n";
    let (_directory, doc) = document(source);
    let options =
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
    // 独立完整Parser只用于这个有限夹具；后继四个范围必须由原生产Global重新交付，不能缓存前探Fact。
    let ranges = Parser::new_ext(source, options)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::Heading { .. } | Tag::CodeBlock(_)))
                .then(|| units(source, range))
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges.len(), 5);
    let (mut request, mut previous) = leaf_sources(&doc, ranges[0].clone());
    let global = request.batch.source_probe_state().0;
    for (index, requested) in ranges.iter().enumerate() {
        if index != 0 {
            // 主来源仍从原恢复点继续；前探中已读过的代码块不能变成缺失来源或被替换成普通段落。
            request
                .continue_forward(&doc, &previous, requested.clone(), authority(&doc))
                .unwrap();
            finish_sources(&mut request, &doc).unwrap();
            previous = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
        }
        let ready = match &previous.semantic {
            MarkdownOrdinarySemanticState::RootLeafReady(ready) => ready,
            MarkdownOrdinarySemanticState::LeafPending(reason) => {
                panic!("视口{index}原Root来源仍未完成：{reason:?}")
            }
            _ => panic!("视口{index}没有原Root语义结果"),
        };
        assert_oracle(ready, &doc, source, requested);
        assert_eq!(request.batch.source_probe_state().0, global);
        assert!(
            !request
                .batch
                .source_global_mut()
                .unwrap()
                .semantic_boundary_retained()
        );
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
    }
}

#[test]
fn production_underline_probe_source_reclaim_failure_releases_the_saved_global_and_scratch() {
    let source = format!(
        "<u>原开标签\n\n标题😀\n===\n\n{}\n\n原真关闭</u>\n",
        "中".repeat(100_000)
    );
    let heading = source.find("标题").unwrap();
    let requested = units(&source, heading..heading + "标题😀\n===\n".len());
    let (_directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    while !request
        .batch
        .source_global_mut()
        .unwrap()
        .semantic_boundary_retained()
    {
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("远端close尚未证明，不能先发布语义");
        };
        assert_step(work);
    }
    // 已真实创建主恢复点，再让后继换页归还失败；不是直接伪造业务错误或把取消当作I/O。
    let attempts = observer.reclaim_attempts();
    observer.fail_next_reclaim_for_test();
    let error = loop {
        match request.advance(&doc, authority(&doc), 4096) {
            Ok(MarkdownViewportSourceStep::Progress { work }) => assert_step(work),
            Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => {
                panic!("未完成真实回收不能取得Ready")
            }
            Err(error) => break error,
        }
    };
    assert_eq!(
        error,
        MarkdownTableRequestError::Source(
            crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        )
    );
    assert!(observer.reclaim_attempts() > attempts);
    assert_eq!(observer.first_errno(), Some(libc::EIO));
    assert!(request.leaf_semantic.is_none() && request.semantic.is_none());
    assert!(request.batch.source_global_mut().is_err());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(error)
    );
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_cancel_during_underline_probe_releases_the_main_boundary_without_poisoning_source() {
    let source = format!(
        "<u>开标签\n\n标题\n===\n\n{}\n\n真关闭</u>\n",
        "中".repeat(100_000)
    );
    let heading = source.find("标题").unwrap();
    let requested = units(&source, heading..heading + "标题\n===\n".len());
    let (_directory, doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    while !request
        .batch
        .source_global_mut()
        .unwrap()
        .semantic_boundary_retained()
    {
        request.advance(&doc, authority(&doc), 4096).unwrap();
    }
    // 取消属于整个来源owner，必须同时结束前探与主恢复义务；原健康文档仍可供后继请求读取。
    request.cancel();
    assert!(request.batch.source_global_mut().is_err());
    assert!(request.leaf_semantic.is_none() && request.semantic.is_none());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(observer.first_errno(), None);
    assert!(doc.original_backing_health().is_ok());
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_leaf_forward_charges_old_pages_and_runs_without_retaining_old_publication() {
    let (source, expected) = code_page_fixture(40);
    let (_directory, doc) = document(&source);
    let first_range = 0..expected[29].end;
    let (mut request, previous) = leaf_sources(&doc, first_range.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须有真实页和 run");
    };
    assert!(old.run_count() > PAGE_RUNS);
    let actual_metadata = previous.records.len() * record_metadata()
        + publication_metadata()
        + old.retained_metadata()
        + previous.blocks.len() * block_metadata()
        + previous.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
    assert_eq!(previous.retained_metadata, actual_metadata);
    let old_runs = old.run_count();
    let old_publication = Rc::downgrade(&previous.publication);
    let global = request.batch.source_probe_state().0;
    assert_ne!(
        global, 0,
        "必须实际保留原结构前沿，两个空槽不能证明同一 Global"
    );
    let next = expected[10].start..expected[39].end;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    assert_eq!(quota.remaining_runs(), 8192 - old_runs);
    assert!(
        quota.metadata_used()
            >= previous.retained_metadata
                + source_workspace_metadata()
                + publication_reservation_metadata()
    );
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    let next_sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &next_sources.semantic else {
        panic!("原 Global 续批必须完成同源代码交集");
    };
    super::partial::assert_partial_oracle(&source, &doc, ready, next.clone(), &expected[10..]);
    super::partial::assert_partial_oracle(&source, &doc, old, first_range, &expected[..30]);
    // 后继只共享原 immutable Leaf 记录；不保父 SourcePublication 链或旧 Ready 的页。
    drop(previous);
    assert!(old_publication.upgrade().is_none());
    assert!(
        ready
            .query_for(&doc, authority(&doc), next.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_forward_quota_failure_preserves_old_ready_and_releases_new_owner() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须先取得真正 Ready");
    };
    request
        .continue_forward(
            &doc,
            &previous,
            requested.start + 1..requested.end,
            authority(&doc),
        )
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    request
        .batch
        .admit_source_runs(quota.remaining_runs())
        .unwrap();
    let expected = MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit);
    assert_eq!(finish_sources(&mut request, &doc), Err(expected));
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert!(request.ready.is_none());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(expected)
    );
    assert!(
        old.query_for(&doc, authority(&doc), requested.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_edit_undo_and_cancel_cannot_reauthorize_same_root_or_keep_document_alive() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, mut doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("原发布必须有真正 Ready");
    };
    let root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &std::sync::Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert_eq!(
        ready.query_for(&doc, authority(&doc), requested.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.leaf_semantic.is_none());
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "请求和外带 Ready 都不能强持 piece root/mmap"
    );

    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    request.advance(&doc, authority(&doc), 4096).unwrap();
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request.advance(&doc, cancelled, 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
}

#[test]
fn production_leaf_proves_direct_links_and_keeps_unsupported_scope_pending() {
    // 保留原来判Pending的同一巨段落；现在必须由原完整Parser逐位置验证新增真实能力，
    // 包括外部caret、label内部caret与反向选择，不能换样本躲开marker显隐。
    let complex = format!(
        "只有**{}**，[未知链接](https://example.com)，没有已证代码或分隔线\n",
        "复杂行内正文".repeat(140)
    );
    let label = complex.find("未知链接").unwrap();
    let label = units(&complex, label..label + "未知链接".len());
    for selection in [
        MarkdownTextSelection::caret(complex.encode_utf16().count()),
        MarkdownTextSelection::caret(label.start + 1),
        MarkdownTextSelection {
            anchor_utf16: label.end,
            head_utf16: label.start,
        },
    ] {
        let (directory, mut doc) = document(&complex);
        let backing = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原长direct-link样本必须拥有真实语义，不能维持过期Pending");
        };
        assert_oracle(ready, &doc, &complex, &requested);
        assert_short_link_payload_work(&request, &doc, &complex, &requested, ready);
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            complex.as_bytes()
        );
        drop((request, sources, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }

    // 原非Root thematic break仍没有取得该scope的语义消费权，不能因接Link而签默认值。
    let source = "> ***\n\n尾\n";
    let (_directory, doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (request, sources) = leaf_sources(&doc, requested);
    assert!(matches!(
        sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap(),
        MarkdownOrdinarySemanticState::LeafPending(MarkdownRootLeafPendingReason::NonRootScope)
    ));
    assert!(request.leaf_semantic.is_none());
    assert_eq!(sources.semantic.run_count(), 0);
    assert_eq!(sources.semantic.retained_metadata(), 0);

    // 原 SourcesOnly 工厂仍只提供来源事实，不因加入 Leaf 模式而偷偷升级任何语义。
    let source = "```\n正文\n```\n";
    let (_directory, doc) = document(source);
    let (_, sources) = source_request(&doc, 0..doc.utf16_len());
    assert!(matches!(
        sources.semantic,
        MarkdownOrdinarySemanticState::SourcesOnly
    ));
}

#[test]
fn production_joint_plain_blocks_keep_exact_ranges_and_original_leaf_identity() {
    let first = "中文普通段落😀。\r\n第二行仍是普通文本。\r\n";
    let setext = "第二个普通闭合块😀\n=====\n";
    let last = "末段中文与 emoji 😀。\n";
    let complex = format!(
        "**复杂正文仍未知{}** [未知链接](https://example.com)",
        "中".repeat(700)
    );
    let source =
        format!("# 标题\n\n{first}\r\n{setext}\n{complex}\n\n```text\n代码\n```\n\n{last}");
    let (directory, doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let requested = 0..doc.utf16_len();
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("同一真实来源必须发布普通块与 Leaf 的已证交集");
    };
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(doc.selection()));
    baseline
        .rebuild(doc.generation(), requested.clone(), &source)
        .unwrap();
    for text in [first, setext, complex.as_str(), last] {
        let start = source.find(text).unwrap();
        // 完整Parser给出闭合块真实末端（含其原换行/Setext marker），不拿text搜索
        // 的子串末端冒充段落coverage；尤其新接入的complex原字面自身不含末换行。
        let range = Parser::new_ext(
            &source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .find_map(|(event, range)| {
            (matches!(event, Event::Start(Tag::Paragraph | Tag::Heading { .. }))
                && range.start == start)
                .then(|| units(&source, range))
        })
        .unwrap();
        assert!(
            ready
                .covered_ranges_for(&doc, authority(&doc))
                .unwrap()
                .any(|actual| actual == &range)
        );
        for index in range {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles
            );
            assert!(
                ready
                    .block_kind_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .is_none(),
                "真正普通块不能冒充 LeafBlockKind"
            );
        }
    }
    assert_short_link_payload_work(&request, &doc, &source, &requested, ready);
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop((request, sources, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn production_joint_giant_plain_forward_uses_same_global_and_never_copies_paragraph() {
    let source = format!("{}\n", "中文😀".repeat(30_000));
    let (_directory, doc) = document(&source);
    let first = 50_000..50_019;
    let (mut request, previous) = leaf_sources(&doc, first.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("巨型单段落必须提供有限普通语义");
    };
    assert_oracle(old, &doc, &source, &first);
    let original_global = request.batch.source_probe_state().0;
    assert_ne!(original_global, 0);
    let next = 50_010..50_031;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, original_global);
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &current.semantic else {
        panic!("原 seed/Global 续批不能丢失巨型段落的有限交集");
    };
    assert_oracle(ready, &doc, &source, &next);
    assert_oracle(old, &doc, &source, &first);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_joint_active_plain_cancel_and_run_limit_release_real_scratch() {
    for cancel in [true, false] {
        let (_directory, doc) = document("中文闭合普通段落😀。\n");
        let mut request = doc
            .viewport_leaf_source_request(0..doc.utf16_len(), 57, doc.selection())
            .unwrap();
        // 等到真实 Plain scratch 接管原 underline 后再注入业务取消或联合库存耗尽。
        for _ in 0..10_000 {
            if request.semantic.is_some() {
                break;
            }
            request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(
                request.ready.is_none(),
                "普通块未证明时不能提前发布默认样式"
            );
        }
        assert!(request.semantic.is_some());
        let expected = if cancel {
            request.cancel();
            MarkdownTableRequestError::Cancelled
        } else {
            let quota = request.batch.source_semantic_quota().unwrap();
            quota.admit_retained(quota.remaining_runs(), 0).unwrap();
            MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
        };
        assert_eq!(finish_sources(&mut request, &doc), Err(expected));
        assert!(request.leaf_semantic.is_none());
        assert!(request.records.is_empty());
        assert!(request.blocks.is_empty());
        assert!(request.ready.is_none());
        assert_eq!(request.advance(&doc, authority(&doc), 4096), Err(expected));
    }
}

#[test]
/// 一次冷准备可在原量子装下短端点；surrogate内的视口末端仍沿真实scalar向上取整。
fn joint_bounds_coalesce_short_endpoints_inside_original_quantum() {
    let source = "中文😀普通段落。\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 9,
    })
    .unwrap();
    let mut request = doc
        .viewport_leaf_source_request(2..3, 57, doc.selection())
        .unwrap();
    let MarkdownViewportSourceStep::Progress { work } =
        request.advance(&doc, authority(&doc), 4096).unwrap()
    else {
        panic!("坐标齐备不等于语义或来源已经发布");
    };
    assert_step(work);
    assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
    let job = request.leaf_semantic.as_ref().unwrap();
    assert!(!job.preparing());
    assert_eq!(job.position_index, 4);
    assert_eq!(job.byte_coverage, Some(6..10));
    assert_eq!(job.selection_bytes, Some(22..25));
    assert!(request.ready.is_none());
    assert_eq!(
        request.batch.source_probe_state().1,
        0,
        "原量子只完成四端点，不顺带推进Global"
    );
}

#[test]
/// 远端片内前缀仍可跨量子，真实byte进度/Source错误保留，不为冷显示扩大读窗或预算。
fn joint_bounds_long_prefix_remains_pending_and_reclaims_source_errors() {
    for fault in [false, true] {
        let source = format!("{}\n", "a".repeat(4000));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 3000,
            head_utf16: 3001,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(0..2, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("未完成的远端前缀不能发布来源");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // 原Bounds先真实读入同页并保留未完成端点；归还pin不会驱逐缓存页，不能把注入标志当成I/O失败。
            let observer = doc.original_backing_observer().unwrap();
            let attempts = observer.reclaim_attempts();
            let successes = observer.reclaim_successes();
            doc.fail_next_original_reclaim_for_test();
            assert_eq!(
                doc.reclaim_original_read_windows_for_test(),
                Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
            );
            assert!(observer.reclaim_attempts() > attempts);
            assert!(observer.reclaim_successes() > successes);
            assert_eq!(observer.first_errno(), Some(libc::EIO));
            // 量子之间的真实回收失败先撤销原Source健康权限；下次入口必须固定SourceSuperseded并释放未发布资源。
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert_eq!(
                request.failed,
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            continue; // 真正读窗归还失败固定首因，不能用同坐标重试或发布半份来源。
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds不取得后继结构或语义的发布权限");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
            assert!(work.structural_transitions > 0);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(0..2));
        assert_eq!(job.selection_bytes, Some(3000..3001));
        assert_eq!(request.batch.source_probe_state().1, 0);
    }
}

#[test]
/// 真实长源的窗口端点与远选区各在自己的小片内续用；源工作少于四次独立前缀解码，不读取两端之间正文。
fn joint_bounds_reuse_actual_piece_prefixes_with_far_selection_and_preserve_failure() {
    let source = "a".repeat(100_000);
    for fault in [false, true] {
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 31215,
            head_utf16: 31226,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(161..501, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("有限端点不能提前发布结构和语义");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // 两端点仍归同一可取消请求；取消后不能用已认证小片标量恢复任何读取或发布。
            request.cancel();
            assert_eq!(request.failed, Some(MarkdownTableRequestError::Cancelled));
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096),
                Err(MarkdownTableRequestError::Cancelled)
            );
            continue;
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds只交付端点，不前移Global或发布Ready");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(161..501));
        assert_eq!(job.selection_bytes, Some(31215..31226));
        assert_eq!(request.batch.source_probe_state().1, 0);
        let mut independent_bytes = 0;
        for (target, ceil) in [(161, false), (501, true), (31215, false), (31226, true)] {
            let mut locator = PositionLocator::new(target, ceil);
            for _ in 0..100 {
                if locator.result().is_some() {
                    break;
                }
                independent_bytes += locator.advance(&doc, 4096).unwrap().source_bytes;
            }
            assert_eq!(locator.result(), Some(target));
        }
        assert!(request.total_work().source_bytes < independent_bytes);
        assert_eq!(request.total_work().source_bytes, 501 + (31226 - 7 * 4096));
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
    }
}
