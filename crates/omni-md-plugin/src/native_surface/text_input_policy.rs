//! Share literal-text policy between body and session fields, configuring system rewriting without replacing input or history.

use super::{autoreleasepool, native_markdown_table_intent_step};
use objc2::runtime::AnyObject;
use objc2_app_kit::NSTextView;
use objc2_foundation::{NSAttributedString, NSString};

/// Configure a main-thread view before exposing it to a client, preserving its undo, composition and selection owners.
pub(super) fn configure_literal_text_input(view: &NSTextView) {
    // Preserve the spaces, quotes, dashes and replacement text actually submitted by the user.
    view.setSmartInsertDeleteEnabled(false);
    view.setAutomaticQuoteSubstitutionEnabled(false);
    view.setAutomaticDashSubstitutionEnabled(false);
    view.setAutomaticTextReplacementEnabled(false);

    // Markdown, queries and filenames interpret their own characters; native detectors and
    // spelling/completion substitutions must not silently alter that literal input.
    view.setAutomaticSpellingCorrectionEnabled(false);
    view.setAutomaticLinkDetectionEnabled(false);
    view.setAutomaticDataDetectionEnabled(false);
    view.setAutomaticTextCompletionEnabled(false);

    // Individual toggles do not clear default checking bits. Disable continuous/grammar checking
    // and the unused detector mask together rather than leaving hidden background checks enabled.
    view.setContinuousSpellCheckingEnabled(false);
    view.setGrammarCheckingEnabled(false);
    view.setEnabledTextCheckingTypes(0);
}

// Bound newly supplied input only, without reading existing document text. Body and session
// fields use the same native conversion contract for this one transaction's payload.
pub(in crate::native_surface) const NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES: usize =
    32 * 1024 * 1024;

/// Copy one new input candidate under its original authority, without making an attributed mirror of the document.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) fn copy_native_markdown_input_text(
    string: &AnyObject,
    current: &impl Fn() -> bool,
) -> Result<String, String> {
    copy_native_markdown_input_text_with_limit(string, current, &|bytes| {
        bytes <= NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
    })
}

/// Deferred input reuses this conversion and reserves bytes under the same ticket before copying.
/// The reservation runs once in the admitted string branch; the caller releases it on failure.
/// The outer current check may reenter, so reservation cannot wait until this function returns.
pub(in crate::native_surface) fn copy_native_markdown_input_text_with_limit(
    string: &AnyObject,
    current: &impl Fn() -> bool,
    reserve_bytes: &impl Fn(usize) -> bool,
) -> Result<String, String> {
    native_markdown_table_intent_step(current, || {
        autoreleasepool(|pool| {
            if let Some(value) = string.downcast_ref::<NSString>() {
                if !current() {
                    return Err("输入文本类型查询已撤权".to_owned());
                }
                // SAFETY: The UTF-8 borrow ends in this pool; only this transaction's bounded new input is copied.
                let plain = unsafe { value.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            if !current() {
                // A failed type query may still reenter. Do not inspect another payload branch after revocation.
                return Err("输入文本类型查询已撤权".to_owned());
            }
            if let Some(value) = string.downcast_ref::<NSAttributedString>() {
                if !current() {
                    return Err("输入文本类型查询已撤权".to_owned());
                }
                let plain = value.string();
                if !current() {
                    return Err("读取属性候选字符串时已撤权".to_owned());
                }
                // SAFETY: Both the native string claim and its UTF-8 borrow end in this pool;
                // the outer intent boundary independently revalidates the original authority.
                let plain = unsafe { plain.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            // Neither public text payload type matched; do not stringify an arbitrary native object.
            Err("输入对象不是受支持的文本类型".to_owned())
        })
    })
    .map_err(|_| "读取候选时原输入票已失效")?
}
