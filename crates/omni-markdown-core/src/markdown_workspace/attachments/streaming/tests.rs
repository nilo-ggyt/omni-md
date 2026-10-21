//! The contiguous grammar is only a finite synthetic oracle. Production rewrite
//! output must come from an anonymous stream over the same frozen real source.
use super::*;

/// Compare exact rewritten bytes/revision and prove the source inode stays unchanged.
fn compare(text: &str, source: &str, destination: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.md");
    fs::write(&path, text).unwrap();
    let source_prefix = markdown_attachment_relative_prefix(source).unwrap();
    let destination_prefix = markdown_attachment_relative_prefix(destination).unwrap();
    let expected = if source_prefix == destination_prefix {
        // An unchanged depth requires no rewritten output even when links are present.
        None
    } else {
        // The finite fixture may use the independently maintained contiguous oracle.
        rewrite_inline_attachment_destinations(text, &source_prefix, &destination_prefix)
    };
    let inspected = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |value| {
        rewrite_moved_note_attachment_links_streamed(value, source, destination)
    })
    .unwrap();
    assert_eq!(inspected.revision, revision_for_bytes(text.as_bytes()));
    match (inspected.value, expected) {
        // Both implementations found no actual replacement and produced no spool.
        (None, None) => {}
        (Some(mut actual), Some(expected)) => {
            // A real anonymous file must contain the complete exact output and
            // revision, with no retained named filesystem entry.
            assert_eq!(actual.byte_len, expected.len() as u64);
            assert_eq!(actual.file.metadata().unwrap().len(), actual.byte_len);
            assert_eq!(actual.file.metadata().unwrap().nlink(), 0);
            assert_eq!(actual.revision, revision_for_bytes(expected.as_bytes()));
            let mut bytes = String::new();
            // Materialize only this finite test oracle, never a production source API.
            actual.file.read_to_string(&mut bytes).unwrap();
            assert_eq!(bytes, expected);
            actual.file.seek(SeekFrom::Start(0)).unwrap();
            let mut second = Vec::new();
            actual.file.read_to_end(&mut second).unwrap();
            assert_eq!(second, expected.as_bytes());
        }
        // A disagreement about whether replacement occurred is itself a regression.
        _ => panic!("streamed与原grammar的实际替换判定不同"),
    }
    // Computing derived output cannot rewrite the original named source.
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
}

#[test]
fn original_fences_inline_ticks_escapes_and_encoded_name_rules_are_byte_exact() {
    // Preserve the established grammar for escaped markers, code spans/fences,
    // incomplete links, CR/LF, Unicode labels, and generated encoded-name syntax.
    let cases = [
        "",
        "没有附件🙂",
        "[a](attachments/a.png)",
        "![a](attachments/%E4%B8%AD.png)",
        "[a](attachments/) [a](attachments/a/b) [a](attachments/a b) [a](attachments/a#b)",
        "[a](attachments/a%ZZ) [a](attachments/a%2) [a](attachments/%)",
        "[a](https://x/attachments/a.png) [a](../attachments/a.png)",
        "\\[a](attachments/a.png) \\](attachments/b.png) \\\\](attachments/c.png)",
        "`[a](attachments/a)` [b](attachments/b) ``[c](attachments/c)``",
        "``[a](attachments/a)`\n[b](attachments/b)`` [c](attachments/c)",
        "```lang\n[a](attachments/a)\n```` \t\r\n[b](attachments/b)",
        "   ~~~lang\n[a](attachments/a)\n  ~~~tail\n[b](attachments/b)\n~~~\n[c](attachments/c)",
        "    ```\n[a](attachments/a)\n    ```\n[b](attachments/b)",
        "```\n~~~\n[a](attachments/a)\n```x\n[b](attachments/b)\n```\n[c](attachments/c)",
        "[a](attachments/a)\r\n[b](attachments/b)\r[c](attachments/c)\n尾🙂",
        "\\\n[a](attachments/a)\n`\n~~~\n[a](attachments/b)\n`\n[c](attachments/c)",
        "[a](attachments/a)[b](attachments/b)![中🙂](attachments/c_%01.~)末尾",
        "[a](attachments/a\n) [b](attachments/b",
    ];
    for text in cases {
        compare(text, "note.md", "nested/deeper/note.md");
        // Links alone cannot allocate output when a move keeps the same relative depth.
        compare(text, "a/note.md", "b/note.md");
    }
    compare("[a](../../attachments/a.png)", "a/b/note.md", "note.md");
}

#[test]
fn grammar_tokens_across_every_four_kib_boundary_and_long_names_have_no_fixed_cap() {
    // Move syntax across each position near the lease edge while the encoded
    // filename spans further leases, proving no fixed token-length truncation.
    for prefix in 4080..4110 {
        let text = format!(
            "{}[中🙂](attachments/{}).\n`opaque [x](attachments/x)`\n[a](attachments/a)",
            "x".repeat(prefix),
            "a%20_~.-9".repeat(1500)
        );
        compare(&text, "note.md", "one/two/note.md");
    }
    for count in [4095, 4096, 4097, 12_289] {
        // Overlong code delimiters retain only their counts across windows and
        // must still hide links until the matching closing run.
        let text = format!(
            "{}\n[a](attachments/a)\n{}\n[b](attachments/b)\n",
            "`".repeat(count),
            "`".repeat(count)
        );
        compare(&text, "note.md", "one/note.md");
    }
}

#[test]
fn deterministic_mixed_syntax_sequences_match_the_unchanged_parser() {
    // Deterministic combinations exercise interactions among the existing grammar
    // states without treating an individual hand-picked token as independent syntax.
    let tokens = [
        "[a](attachments/a)",
        "\\",
        "`",
        "``",
        "```",
        "~~~",
        "   ",
        "    ",
        "\n",
        "\r",
        "x",
        "🙂",
        "](attachments/%a)",
        "](attachments/a/b)",
    ];
    let mut state = 0x519bcda11234u64;
    for _ in 0..48 {
        let mut text = String::new();
        for _ in 0..120 {
            // Keep fixtures reproducible while varying state transitions and lease layout.
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push_str(tokens[(state >> 32) as usize % tokens.len()]);
        }
        compare(&text, "note.md", "a/b/note.md");
    }
}

#[test]
fn partial_range_writer_preserves_original_bytes() {
    // Exercise an exact byte-range export through the real synthetic file and
    // its production frozen backing, without a substitute failing writer.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.md");
    let text = "中文🙂a\u{301}".repeat(1000);
    fs::write(&path, &text).unwrap();
    let inspected = inspect_bounded_markdown(&path, MAX_DOCUMENT_BYTES, |source| {
        // Export exact source bytes even when the requested byte interval splits
        // scalars; range copying changes no UTF-8 representation or source ownership.
        let mut bytes = Vec::new();
        source.copy_range_to(4091..8199, &mut bytes)?;
        assert_eq!(bytes, text.as_bytes()[4091..8199]);
        Ok(())
    });
    assert!(inspected.is_ok());
}
