//! 正文与会话字段共用字面文本策略；只配置系统改写和检测，不接管输入或历史。

use objc2_app_kit::NSTextView;

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
