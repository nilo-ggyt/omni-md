//! 正文与会话字段共用字面文本策略；只配置系统改写和检测，不接管输入或历史。

use super::{autoreleasepool, native_markdown_table_intent_step};
use objc2::runtime::AnyObject;
use objc2_app_kit::NSTextView;
use objc2_foundation::{NSAttributedString, NSString};

/// 对尚未交给客户端的主线程文本视图应用同一策略；不更改Undo、组合输入或选区。
pub(super) fn configure_literal_text_input(view: &NSTextView) {
    // 字面输入必须保持用户提交的空格、引号、连字符与替换文本，不由系统另行改写。
    view.setSmartInsertDeleteEnabled(false);
    view.setAutomaticQuoteSubstitutionEnabled(false);
    view.setAutomaticDashSubstitutionEnabled(false);
    view.setAutomaticTextReplacementEnabled(false);

    // Markdown、查询和文件名自行解释原字符，不启动拼写纠正、链接/数据识别或自动补全。
    view.setAutomaticSpellingCorrectionEnabled(false);
    view.setAutomaticLinkDetectionEnabled(false);
    view.setAutomaticDataDetectionEnabled(false);
    view.setAutomaticTextCompletionEnabled(false);

    // 单项开关不会清除默认检查位；同时禁用连续检查、语法检查及未使用的检测类型。
    view.setContinuousSpellCheckingEnabled(false);
    view.setGrammarCheckingEnabled(false);
    view.setEnabledTextCheckingTypes(0);
}

// 只限制本次新增输入，不读取整个文稿；正文与会话字段共用同一 Native 转换合同。
pub(in crate::native_surface) const NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES: usize =
    32 * 1024 * 1024;

/// 在原输入权限下复制一次新增候选，绝不以属性对象建立第二份正文。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) fn copy_native_markdown_input_text(
    string: &AnyObject,
    current: &impl Fn() -> bool,
) -> Result<String, String> {
    copy_native_markdown_input_text_with_limit(string, current, &|bytes| {
        bytes <= NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
    })
}

/// 延后输入只借原转换实现；先在同票下预留本次字节额度，才复制新增候选。
/// 预留回调只在已确定的字符串分支调用一次，失败由调用方归还本次预留；
/// 后置current可重入，故额度不能等本函数返回后才登记。
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
                // SAFETY: 借用在当前池内结束，只复制有上限的本次新增输入。
                let plain = unsafe { value.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            if !current() {
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
                // SAFETY: Native claim 与 UTF8 借用均在本池内结束，外层还会检查原权限。
                let plain = unsafe { plain.to_str(pool) };
                if !current() || !reserve_bytes(plain.len()) {
                    return Err("输入文本已失效或超过当前会话上限".to_owned());
                }
                return Ok(plain.to_owned());
            }
            Err("输入对象不是受支持的文本类型".to_owned())
        })
    })
    .map_err(|_| "读取候选时原输入票已失效")?
}
