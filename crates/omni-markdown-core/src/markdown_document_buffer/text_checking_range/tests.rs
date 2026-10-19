//! 真实冻结mmap的范围结果与piece访问计量；完整UTF-16 oracle只存在于合成测试。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tempfile::{TempDir, tempdir};

use super::*;

fn fixture(source: &str) -> (TempDir, MarkdownDocumentBuffer) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("checking-range.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

/// 包装真实FrozenText而非替身正文；每次piece_bytes实际借用都会计数，无法以空统计冒充零读取。
struct CountingSource<'a> {
    source: &'a FrozenText,
    calls: Cell<usize>,
    bytes: Cell<usize>,
    maximum: Cell<usize>,
    starts: RefCell<Vec<usize>>,
}

impl<'a> CountingSource<'a> {
    fn new(snapshot: &'a MarkdownTextSnapshot) -> Self {
        Self {
            source: snapshot.text.as_ref(),
            calls: Cell::new(0),
            bytes: Cell::new(0),
            maximum: Cell::new(0),
            starts: RefCell::new(Vec::new()),
        }
    }

    fn query(&self, request: Range<usize>) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        // 每次独立请求先清观测量；生产与计量调用同一generic查询，不另写只供测试的算法。
        self.calls.set(0);
        self.bytes.set(0);
        self.maximum.set(0);
        self.starts.borrow_mut().clear();
        paragraph_range(self, self.source.root.as_deref(), request)
    }
}

impl PieceByteSource for CountingSource<'_> {
    fn source_health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.source.source_health()
    }
    fn piece_bytes(
        &self,
        piece: &MarkdownPiece,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        let bytes = self.source.piece_bytes(piece)?;
        self.calls.set(self.calls.get() + 1);
        self.bytes.set(self.bytes.get() + bytes.len());
        self.maximum.set(self.maximum.get().max(bytes.len()));
        self.starts.borrow_mut().push(piece.byte_start);
        Ok(bytes)
    }

    // 按实际返回的局部lease记录长度和起点，沿用原测试的真实字节观测口径。
    #[cfg(feature = "paged-shaping-frontier")]
    fn piece_range_bytes(
        &self,
        piece: &MarkdownPiece,
        local: Range<usize>,
    ) -> Result<PieceBytes<'_>, MarkdownDocumentBufferError> {
        let start = piece.byte_start + local.start;
        let bytes = self.source.piece_range_bytes(piece, local)?;
        self.calls.set(self.calls.get() + 1);
        self.bytes.set(self.bytes.get() + bytes.len());
        self.maximum.set(self.maximum.get().max(bytes.len()));
        self.starts.borrow_mut().push(start);
        Ok(bytes)
    }
}

fn oracle(units: &[u16], request: Range<usize>) -> Range<usize> {
    // 独立平面oracle用切片搜索，既不使用树聚合，也不调用生产递归查找。
    let terminator = |unit: &u16| matches!(*unit, 0x000d | 0x000a | 0x2029);
    let start = units[..request.start]
        .iter()
        .rposition(terminator)
        .map_or(0, |index| index + 1);
    let anchor = request.end - usize::from(!request.is_empty());
    let end = units[anchor..]
        .iter()
        .position(terminator)
        .map_or(units.len(), |relative| {
            let position = anchor + relative;
            // 尾端CR的后继LF仍属于该段；起点查找故意没有这个反向归并。
            position
                + if units[position] == 0x000d && units.get(position + 1) == Some(&0x000a) {
                    2
                } else {
                    1
                }
        });
    start..end
}

#[test]
fn native_golden_ranges_keep_empty_eof_crlf_and_low_surrogate_semantics() {
    let cases = [
        ("", vec![(0..0, 0..0)]),
        (
            "甲😀A\r\n乙B\r\n丙",
            vec![
                (0..0, 0..6),
                (2..2, 0..6),
                (4..4, 0..6),
                (5..5, 5..6),
                (9..9, 9..10),
                (3..10, 0..10),
                (11..11, 10..11),
            ],
        ),
        (
            "A\r\nB\r\n",
            vec![(0..0, 0..3), (2..2, 2..3), (5..5, 5..6), (6..6, 6..6)],
        ),
        (
            "a\u{85}b\u{2028}c",
            vec![(0..0, 0..5), (2..4, 0..5), (5..5, 0..5)],
        ),
        ("a\u{2029}b", vec![(1..1, 0..2), (2..2, 2..3), (0..2, 0..2)]),
    ];
    for (source, ranges) in cases {
        let (_directory, document) = fixture(source);
        let snapshot = document.text_snapshot();
        for (request, expected) in ranges {
            // 这些具体坐标来自实际NSTextView控制；低代理项是合法元数据请求，不是编辑边界。
            assert_eq!(
                snapshot.text_checking_paragraph_range(request),
                Ok(expected)
            );
        }
        let counted = CountingSource::new(&snapshot);
        assert_eq!(
            counted.query(snapshot.utf16_len() + 1..snapshot.utf16_len() + 1),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(counted.calls.get(), 0);
        assert_eq!(
            counted.query(Range { start: 1, end: 0 }),
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
        assert_eq!(counted.calls.get(), 0);
    }
}

#[test]
fn aggregate_counts_raw_crlf_and_ps_without_changing_physical_lines() {
    let whole = MarkdownDocumentMetrics::from_text("甲\r\n\u{2029}\u{85}\u{2028}");
    let joined = MarkdownDocumentMetrics::joined(
        MarkdownDocumentMetrics::from_text("甲\r"),
        MarkdownDocumentMetrics::from_text("\n\u{2029}\u{85}\u{2028}"),
    );
    assert_eq!(whole, joined);
    assert_eq!(whole.text_checking_terminators, 3);
    assert_eq!(whole.first_text_checking_terminator.unwrap().get(), 2);
    assert_eq!(whole.last_text_checking_terminator.unwrap().get(), 4);
    assert_eq!(whole.line_breaks, 1); // PS不改变已有CR/LF物理行规则，CRLF仍只算一个换行。
}

#[test]
fn giant_separator_free_requests_do_not_touch_any_body_piece() {
    let source = "甲😀\u{85}\u{2028}".repeat(200_000);
    let (_directory, document) = fixture(&source);
    let snapshot = document.text_snapshot();
    let counted = CountingSource::new(&snapshot);
    let length = snapshot.utf16_len();
    for request in [
        0..0,
        2..2,
        17..30,
        length / 2..length / 2,
        length..length,
        0..length,
    ] {
        // 即使整段远超物化上限，本API也只返回范围，零聚合直接跳过所有正文页。
        assert_eq!(counted.query(request), Ok(0..length));
        assert_eq!(counted.calls.get(), 0);
        assert_eq!(counted.bytes.get(), 0);
        assert_eq!(counted.maximum.get(), 0);
    }
}

#[test]
fn only_two_endpoint_leaves_and_one_crlf_unit_are_read_across_a_giant_gap() {
    let mut bytes = vec![b'x'; ORIGINAL_PIECE_BYTES * 256];
    let left = ORIGINAL_PIECE_BYTES * 3;
    let right = ORIGINAL_PIECE_BYTES * 250;
    for position in [left + 100, left + 200, left + 300, right + 100, right + 200] {
        bytes[position] = b'\n';
    }
    bytes[right + 300] = b'\r';
    bytes[right + 301] = b'\n';
    let source = String::from_utf8(bytes).unwrap();
    let (_directory, document) = fixture(&source);
    let snapshot = document.text_snapshot();
    let counted = CountingSource::new(&snapshot);
    assert_eq!(
        counted.query(left + 250..right + 251),
        Ok(left + 201..right + 302)
    );
    // 两端都特意夹在同叶多个终止符中，迫使真实叶内解码；大段中间叶不允许被访问。
    assert_eq!(counted.calls.get(), 3);
    assert_eq!(counted.bytes.get(), ORIGINAL_PIECE_BYTES * 3);
    assert_eq!(counted.maximum.get(), ORIGINAL_PIECE_BYTES);
    assert_eq!(counted.starts.borrow().as_slice(), &[left, right, right]);
}

#[test]
fn crlf_and_ps_keep_ranges_at_original_four_kib_piece_edges() {
    let source = format!(
        "{}\r\n{}\u{2029}末😀",
        "x".repeat(ORIGINAL_PIECE_BYTES - 1),
        "y".repeat(ORIGINAL_PIECE_BYTES - 2),
    );
    let (_directory, document) = fixture(&source);
    assert_eq!(document.line_count(), 2);
    let snapshot = document.text_snapshot();
    let units: Vec<_> = source.encode_utf16().collect();
    let counted = CountingSource::new(&snapshot);
    let ps = ORIGINAL_PIECE_BYTES * 2 - 1;
    for position in [
        0,
        ORIGINAL_PIECE_BYTES - 1,
        ORIGINAL_PIECE_BYTES,
        ps,
        ps + 1,
        units.len() - 1,
        units.len(),
    ] {
        assert_eq!(
            counted.query(position..position),
            Ok(oracle(&units, position..position))
        );
        assert!(counted.calls.get() <= 3);
        assert!(counted.maximum.get() <= ORIGINAL_PIECE_BYTES);
    }
    assert_eq!(
        snapshot
            .text
            .root
            .as_ref()
            .unwrap()
            .metrics()
            .text_checking_terminators,
        3
    );
}

#[test]
fn random_utf16_ranges_match_a_flat_oracle_with_constant_piece_reads() {
    let tokens = [
        "a", "中", "😀", "\r", "\n", "\r\n", "\u{2029}", "\u{85}", "\u{2028}", "e\u{301}",
    ];
    let mut seed = 0x1234_5678_u64;
    let mut source = String::new();
    for _ in 0..6_000 {
        // 固定seed让失败可复现；混合标量和分隔符在多个4KiB叶中形成真实随机端点。
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        source.push_str(tokens[(seed >> 32) as usize % tokens.len()]);
    }
    let (_directory, document) = fixture(&source);
    let snapshot = document.text_snapshot();
    let counted = CountingSource::new(&snapshot);
    let units: Vec<_> = source.encode_utf16().collect();
    for _ in 0..1_000 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let a = (seed >> 32) as usize % (units.len() + 1);
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let b = (seed >> 32) as usize % (units.len() + 1);
        let request = a.min(b)..a.max(b);
        assert_eq!(counted.query(request.clone()), Ok(oracle(&units, request)));
        assert!(counted.calls.get() <= 3);
        assert!(counted.maximum.get() <= ORIGINAL_PIECE_BYTES);
    }
}

#[test]
fn edited_crlf_undo_redo_and_dropped_documents_keep_their_frozen_range_values() {
    let (directory, document) = fixture("left\nright😀");
    let document = Rc::new(RefCell::new(document));
    let weak = Rc::downgrade(&document);
    let original = document.borrow().text_snapshot();
    document
        .borrow_mut()
        .insert_utf16(4, "\r", MarkdownTextSelection::caret(5))
        .unwrap();
    let crlf = document.borrow().text_snapshot();
    assert_eq!(crlf.text_checking_paragraph_range(5..5), Ok(5..6));
    document.borrow_mut().undo().unwrap();
    let undone = document.borrow().text_snapshot();
    document.borrow_mut().redo().unwrap();
    let redone = document.borrow().text_snapshot();
    document
        .borrow_mut()
        .insert_utf16(6, "\u{2029}", MarkdownTextSelection::caret(7))
        .unwrap();
    let ps = document.borrow().text_snapshot();
    assert_eq!(document.borrow().line_count(), 2); // 编辑的PS只影响文本检查段落，不增加物理行。
    drop(document);
    drop(directory);
    assert!(weak.upgrade().is_none());
    for (snapshot, expected) in [
        (original, "left\nright😀"),
        (undone, "left\nright😀"),
        (crlf, "left\r\nright😀"),
        (redone, "left\r\nright😀"),
        (ps, "left\r\n\u{2029}right😀"),
    ] {
        let units: Vec<_> = expected.encode_utf16().collect();
        for position in 0..=units.len() {
            // 旧root、匿名mmap、编辑段仍由值持有；查询不能偷用已经释放的活动文档坐标。
            assert_eq!(
                snapshot.text_checking_paragraph_range(position..position),
                Ok(oracle(&units, position..position))
            );
        }
    }
}
