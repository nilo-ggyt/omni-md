//! 实际中文提交的有限分类和四 MiB 合成原文的流式核对；两种原生入口共用同一结果标准。

use std::{cell::RefCell, rc::Weak};

use objc2_foundation::{
    NSMatchingOptions, NSRange, NSRegularExpression, NSRegularExpressionOptions, NSString,
    ns_string,
};
use omni_markdown_core::{MarkdownDocumentBuffer, MarkdownTextSelection};

use super::INSERTION;
use super::native_types::{NativeMarkdownPagedBody, NativeMarkdownPagedInputCommit};

pub(super) const ORIGINAL_UTF16: usize = 1_398_102;
pub(super) const ORIGINAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMMIT_UTF16: usize = 64;

/// 仅捕获原远端插入点这次实际提交，最多保留 128 字节；不根据输入法名称猜候选词。
pub(super) fn capture_han(
    document: &Weak<RefCell<MarkdownDocumentBuffer>>,
    body: &Weak<NativeMarkdownPagedBody>,
    commit: &NativeMarkdownPagedInputCommit,
) -> Result<Vec<u16>, &'static str> {
    let length = commit
        .state
        .utf16_len
        .checked_sub(ORIGINAL_UTF16)
        .filter(|length| (1..=MAX_COMMIT_UTF16).contains(length))
        .ok_or("实际汉字提交为空或超出有限观察范围")?;
    if commit.state.selection != MarkdownTextSelection::caret(INSERTION + length) {
        return Err("实际汉字提交没有保留原插入点末端光标");
    }
    let mut units = vec![0; length];
    {
        // Core 借用仅覆盖有限读取；ICU 与原生客户查询可能重入，不能跨越此借用。
        let document = document.upgrade().ok_or("原合成文稿已经释放")?;
        let document = document.borrow();
        if document.generation() != commit.state.generation
            || document.utf16_len() != commit.state.utf16_len
            || document.selection() != commit.state.selection
        {
            return Err("原合成文稿已不是这次实际提交的后态");
        }
        document
            .copy_utf16_code_units(INSERTION, &mut units)
            .map_err(|_| "无法读取实际有限提交")?;
    }
    let text = String::from_utf16(&units).map_err(|_| "实际提交不是合法 UTF-16")?;
    let string = NSString::from_str(&text);
    let expression = NSRegularExpression::regularExpressionWithPattern_options_error(
        ns_string!("\\A\\p{Han}+\\z"),
        NSRegularExpressionOptions::empty(),
    )
    .map_err(|_| "无法建立系统 Unicode 汉字分类")?;
    let range = NSRange::new(0, length);
    if expression.rangeOfFirstMatchInString_options_range(
        &string,
        NSMatchingOptions::empty(),
        range,
    ) != range
    {
        return Err("实际候选提交不是完整汉字");
    }
    // 分类后的同一客户仍须持有原后态；不得给已变化的输入补签一次成功。
    if !body.upgrade().is_some_and(|body| {
        body.view()
            .is_ok_and(|view| view.input_state().ok() == Some(commit.state))
    }) {
        return Err("实际提交分类后原输入后态已撤销");
    }
    Ok(units)
}

/// 每次只用 4 KiB 栈缓冲核完整原文；原文首字为 X，其余为中，插入值来自同次实际提交。
pub(super) fn verify(
    document: &Weak<RefCell<MarkdownDocumentBuffer>>,
    inserted: &[u16],
) -> Result<(), String> {
    let document = document.upgrade().ok_or("流式核对的原文稿已释放")?;
    let document = document.borrow();
    let bytes =
        char::decode_utf16(inserted.iter().copied()).try_fold(0usize, |total, scalar| {
            scalar
                .map(|value| total + value.len_utf8())
                .map_err(|_| "实际汉字提交不是完整字符")
        })?;
    if document.utf16_len() != ORIGINAL_UTF16 + inserted.len()
        || document.byte_len() != ORIGINAL_BYTES + bytes
    {
        return Err("实际提交后的原文长度不符".into());
    }
    let mut units = [0u16; 2048];
    for start in (0..document.utf16_len()).step_by(units.len()) {
        let count = units.len().min(document.utf16_len() - start);
        document
            .copy_utf16_code_units(start, &mut units[..count])
            .map_err(|_| "无法流式核对实际中文输入后的原文")?;
        for (offset, &actual) in units[..count].iter().enumerate() {
            let at = start + offset;
            let expected = if at == 0 {
                b'X' as u16
            } else if (INSERTION..INSERTION + inserted.len()).contains(&at) {
                inserted[at - INSERTION]
            } else {
                0x4e2d
            };
            if actual != expected {
                return Err(format!("实际中文输入修改了非目标位置 {at}"));
            }
        }
    }
    Ok(())
}
