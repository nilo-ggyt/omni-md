//! 同步显式查找的匹配适配；Foundation 枚举与 canonical 可变事务严格分开。

use std::ops::Range;
use std::time::{Duration, Instant};

use objc2::runtime::Bool;
use objc2_foundation::{
    NSMatchingFlags, NSMatchingOptions, NSRegularExpression, NSRegularExpressionOptions,
    NSTextCheckingResult,
};
#[cfg(test)]
use omni_markdown_core::MarkdownReplacementPart;
use omni_markdown_core::markdown_find::{
    MarkdownFindMatch, MarkdownLiteralFindOptions, visit_markdown_literal_matches_cancellable,
    whole_word_boundary,
};

use super::*;

mod spool;
mod template;
use spool::*;
use template::*;

#[path = "backend.rs"]
pub(super) mod backend;

/// 独立 TextKit 夹具显式提供自己的实际图；正式 Find 直接使用分页后端。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn execute_find_action(
    bar: &Rc<NativeMarkdownFindBar>,
    view: &PieceBackedMarkdownTextView,
    controller: &NativeMarkdownController,
    intent: u64,
    action: MarkdownFindAction,
) -> Result<(), String> {
    execute_find_backend(
        bar,
        NativeMarkdownFindBackend::TextKit {
            view,
            controller,
            intent,
        },
        action,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}
/// 查询和值只活在一次同步动作内，退出动作即释放所有容量。
pub(super) struct Query {
    pub(super) search: String,
    replacement: String,
    pub(super) case_sensitive: bool,
    pub(super) regex: bool,
    pub(super) word: bool,
    compiled: std::cell::OnceCell<Result<Retained<NSRegularExpression>, String>>,
}

/// 导航只取主范围；只有替换模板需要 capture 坐标，避免每个导航命中都分配 Vec。
#[derive(Clone, Copy, Eq, PartialEq)]
enum CaptureRanges {
    Omit,
    Include,
}

/// 一个 match 只携带标量坐标；不跨 Foundation callback 保留 NSTextCheckingResult。
#[derive(Clone, Debug)]
struct Found {
    range: Range<usize>,
    precise: bool,
    captures: Vec<Option<Range<usize>>>,
}

/// 反转义只处理 baseline 声明的四种字符，未知 escape 保留给 regex 或字面搜索。
pub(super) fn unquote(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        // 只向前消费有界输入；已识别的二字符 escape 一次消费，未知序列保留原文。
        if character == '\\' {
            // 只有明确的查询语言 escape 才改变值，末尾单独反斜线按字面保留。
            let replacement = match chars.peek() {
                Some('n') => Some('\n'),  // 显式换行。
                Some('r') => Some('\r'),  // 显式 carriage return。
                Some('t') => Some('\t'),  // 显式 tab。
                Some('\\') => Some('\\'), // 显式反斜线。
                _ => None, // 非集合 escape 留给下面的专门转换或原生校验。                // 未知 escape 不被悄悄删除。
            };
            if let Some(replacement) = replacement {
                // 已确认替代字符，跳过 escape 的第二个 scalar，避免下轮重复输出。
                chars.next();
                result.push(replacement);
                continue;
            }
        }
        result.push(character);
    }
    result
}

/// 仅转换已核对的 ECMAScript 字符类/转义；不是私有 parser，也不声称 ICU 支持全部 JS 语法。
fn native_pattern(pattern: &str, case_sensitive: bool) -> String {
    let word = if case_sensitive {
        // 区分大小写时 JS word 仅含 ASCII 字母、数字与下划线。
        "A-Za-z0-9_"
    } else {
        // Unicode ignore-case 额外允许与 ASCII word simple-fold 等价的两个字符。
        "A-Za-z0-9_ſK"
    };
    let space = r"\u0009-\u000d\u0020\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\ufeff";
    let word_atom = format!("(?-i:[{word}])");
    let mut output = String::new();
    let mut characters = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(character) = characters.next() {
        // 转换单个 token，不递归展开用户 pattern；最终语法仍由 ICU 公共 compiler 校验。
        if character == '\\' {
            let Some(escape) = characters.next() else {
                // 不完整 escape 原样交给 compiler 报错，不补造合法查询。
                output.push('\\');
                break;
            };
            let class = match escape {
                'w' => Some((false, word)), // JS 的 word 类不等于 ICU 的 Unicode word 类。
                'W' => Some((true, word)),  // word 的补集仍按 JS 字符集合定义。
                'd' => Some((false, "0-9")), // JS \d 只包含 ASCII digit。
                'D' => Some((true, "0-9")), // 非 ASCII digit 的字符不能被 ICU Unicode digit 吞掉。
                's' => Some((false, space)), // 包含 BOM，不把 ICU 的 NEL 当 JS whitespace。
                'S' => Some((true, space)), // 与同一 JS whitespace 集合取补集。
                _ => None,                  // 非集合 escape 留给下面的专门转换或原生校验。
            };
            if let Some((negative, class)) = class {
                // ICU 的公开 UnicodeSet 支持嵌套补集；正向集合在现有括号内直接展开。
                if in_class && !negative {
                    // 复用外层字符类，避免把其中成员错误解释为额外的字面括号。
                    output.push_str(class);
                } else {
                    // 独立 atom 或补集需要自己的括号，维持集合而不是文本拼接语义。
                    output.push('[');
                    if negative {
                        // 仅在此内层集合反转成员，不反转整个用户查询。
                        output.push('^');
                    }
                    output.push_str(class);
                    output.push(']');
                }
            } else if !in_class && matches!(escape, 'b' | 'B') {
                if escape == 'b' {
                    // 相邻两侧 word 类别不同；使用零宽断言，不消费正文。
                    output.push_str(&format!(
                        "(?:(?<!{word_atom})(?={word_atom})|(?<={word_atom})(?!{word_atom}))"
                    ));
                } else {
                    // 相同类别是边界的补集，正文边缘也按非 word 处理。
                    output.push_str(&format!(
                        "(?:(?<={word_atom})(?={word_atom})|(?<!{word_atom})(?!{word_atom}))"
                    ));
                }
            } else if escape == 'u' && characters.peek() == Some(&'{') {
                // JS Unicode scalar escape 与 ICU 的公开 \x{...} 使用不同前缀。
                output.push_str("\\x");
            } else {
                output.push('\\');
                output.push(escape); // 其余语法由 ICU 正常验证并返回明确错误。
            }
        } else if character == '^' && !in_class {
            // ECMAScript multiline 只承认四种行终止符；ICU 原生 m 还包含 VT/FF/NEL。
            output.push_str(r"(?:\A|(?<=[\n\r\u2028\u2029]))");
        } else if character == '$' && !in_class {
            // 使用绝对正文末尾，不把 ICU 的额外换行语义带进 baseline 查询。
            output.push_str(r"(?:\z|(?=[\n\r\u2028\u2029]))");
        } else if character == '.' && !in_class {
            output.push_str(r"[^\n\r\u2028\u2029]"); // JS dot 不排除 NEL，保留其实际匹配行为。
        } else {
            if character == '[' {
                // 字符类内部的点和锚点是字面成员，不能套用外部 token 转换。
                in_class = true;
            }
            if character == ']' {
                // 结束当前类；无效或不配对的括号仍交给 ICU 拒绝。
                in_class = false;
            }
            output.push(character);
        }
    }
    output
}

/// 原生字符串先检查有限长度再复制；不会为搜寻而截断或取消正在进行的 composition。
fn read_field(field: &NSTextField) -> Result<String, String> {
    if let Some(editor) = field.currentEditor() {
        let editor = editor
            .downcast_ref::<NSTextView>()
            .ok_or("查找字段的原输入上下文不可用")?;
        // SAFETY: 实际字段编辑器已经核为NSTextView，公开NSTextInputClient只读该原生组合状态。
        if unsafe { msg_send![editor, hasMarkedText] } {
            return Err("请先完成当前输入法组合，再执行查找或替换".into());
        }
    } // 按钮/菜单与Return共用同一组合边界；不提交、取消或读取未完成的查询候选。
    let value = field.stringValue();
    if value.length() > 16_384 {
        // 先拒绝超限 native 值，避免复制不可控输入或静默截断用户模板。
        return Err("查找与替换输入最多为 16,384 个 UTF-16 单元".into());
    }
    Ok(value.to_string())
}

impl Query {
    /// 自动反馈只读原查询和三个选项，不读取替换候选；每个Native getter分别服从同一原输入票。
    pub(super) fn read_viewport(
        bar: &NativeMarkdownFindBar,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        Ok(Self {
            search: native_markdown_table_intent_step(current, || read_field(&bar.query))
                .map_err(|_| "原查询输入已变化")??,
            replacement: String::new(), // 反馈不会解析替换模板或自动提交替换。
            case_sensitive: native_markdown_table_intent_step(current, || bar.case.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            regex: native_markdown_table_intent_step(current, || bar.regex.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            word: native_markdown_table_intent_step(current, || bar.word.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            compiled: std::cell::OnceCell::new(),
        })
    }

    /// 导航只读查询；真正替换才读取模板，替换字段的中文候选不能阻断无关导航。
    fn read(bar: &NativeMarkdownFindBar, action: MarkdownFindAction) -> Result<Self, String> {
        Ok(Self {
            search: read_field(&bar.query)?,
            replacement: match action {
                MarkdownFindAction::ReplaceNext | MarkdownFindAction::ReplaceAll => {
                    unquote(&read_field(&bar.replacement)?) // 保留实际替换的组合输入拒绝、长度及 escape 合同。
                }
                MarkdownFindAction::Next
                | MarkdownFindAction::Previous
                | MarkdownFindAction::SelectAll
                | MarkdownFindAction::Hide => String::new(), // 导航和隐藏不取得替换候选，也不解析用户模板。
            },
            case_sensitive: bar.case.state() != 0,
            regex: bar.regex.state() != 0,
            word: bar.word.state() != 0,
            compiled: std::cell::OnceCell::new(),
        })
    }

    /// 每次动作或有限反馈最多编译一次；模板和枚举复用同一个只读原生对象。
    pub(super) fn compiled_regex(&self) -> Result<&NSRegularExpression, String> {
        self.compiled
            .get_or_init(|| {
                let mut options = NSRegularExpressionOptions::empty();
                if !self.case_sensitive {
                    // 仅此旗标引入 ICU full folding；已审差异不冒充 JS simple folding。
                    options |= NSRegularExpressionOptions::CaseInsensitive;
                }
                let pattern =
                    NSString::from_str(&native_pattern(&self.search, self.case_sensitive));
                NSRegularExpression::regularExpressionWithPattern_options_error(&pattern, options)
                    .map_err(|_| "正则表达式不符合原生 ICU 引擎的语法".to_owned())
            })
            .as_ref()
            .map(|regex| &**regex)
            .map_err(Clone::clone)
    }

    /// 字面模式用流式 NFKD；正则使用公开 ICU API，语法差异在 oracle 中单独记账。
    fn visit(
        &self,
        document: &SharedMarkdownDocument,
        native_string: &NSString,
        overlapping: bool,
        captures: CaptureRanges,
        visit: impl FnMut(Found) -> Result<bool, String>,
    ) -> Result<(), String> {
        self.visit_with_budget(
            document,
            native_string,
            overlapping,
            captures,
            Duration::from_secs(2),
            visit,
        )
    }

    /// 测试可注入更短预算；产品在进度/结果 callback 协作检查两秒，取消为完整错误。
    /// 编译和两个公开 callback 之间没有硬实时中断保证，不能把此预算写成输入到返回的上界。
    fn visit_with_budget(
        &self,
        document: &SharedMarkdownDocument,
        native_string: &NSString,
        overlapping: bool,
        capture_ranges: CaptureRanges,
        budget: Duration,
        mut visit: impl FnMut(Found) -> Result<bool, String>,
    ) -> Result<(), String> {
        let length = document.borrow().utf16_len();
        if self.search.is_empty() {
            return Ok(()); // 空查询没有匹配，不执行无限零长查找。
        }
        if !self.regex {
            // 字面路径在纯 Rust 固定源窗口中 KMP 扫描，不创建完整正文的 normalized 镜像。
            let text = unquote(&self.search);
            let options = MarkdownLiteralFindOptions {
                case_sensitive: self.case_sensitive,
                whole_word: self.word,
                overlapping,
            };
            let callback_error = RefCell::new(None);
            let started = Instant::now();
            let result = visit_markdown_literal_matches_cancellable(
                &document.borrow(),
                &text,
                0..length,
                options,
                || started.elapsed() <= budget,
                |MarkdownFindMatch {
                     range_utf16,
                     precise,
                 }| {
                    // 回调只做标量/标准 I/O，不调用 Native 或可重入的 AppKit getter。
                    if started.elapsed() > budget {
                        // 标准 I/O visitor 也计入扫描预算，取消后调用方不会提交已生成的记录。
                        callback_error.replace(Some("查找超过 2 秒执行时限".to_owned()));
                        return Ok(false);
                    }
                    match visit(Found {
                        captures: Vec::new(), // 字面模板没有捕获引用，不为每个命中分配一个无用 Vec。
                        range: range_utf16,
                        precise,
                    }) {
                        Ok(proceed) => Ok(proceed), // 调用方显式控制早停，不保留后续匹配。
                        Err(error) => {
                            callback_error.replace(Some(error));
                            Ok(false)
                        } // 错误在释放只读借用后传播。
                    }
                },
            );
            result.map_err(|error| error.to_string())?;
            if let Some(error) = callback_error.take() {
                // 释放 canonical 只读借用后保留 visitor 的首个失败，匿名记录随上层动作丢弃。
                return Err(error);
            }
            return Ok(());
        }
        let regex = self.compiled_regex()?;
        let started = Instant::now();
        let error = RefCell::new(None);
        let visitor = RefCell::new(&mut visit);
        let next = Cell::new(0usize);
        let stopped = Cell::new(false);
        let accepted_end = Cell::new(None);
        loop {
            // 每批最多 1,024 个结果后退出并排空本地 pool，不积攒百万个 autoreleased match 对象。
            let count = Cell::new(0usize);
            let batch_full = Cell::new(false);
            autoreleasepool(|_| {
                let block = block2::StackBlock::new(
                    |result: *mut NSTextCheckingResult,
                     flags: NSMatchingFlags,
                     mut stop: NonNull<Bool>| {
                        if flags.contains(NSMatchingFlags::InternalError) {
                            // 原生匹配器内部失败不能伪装成“没有匹配”或发布部分替换。
                            error.replace(Some("原生正则表达式引擎无法完成搜索".to_owned()));
                            // SAFETY: Foundation 在本次同步 callback 内提供有效的 stop 输出。
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        if started.elapsed() > budget {
                            error.replace(Some("正则表达式搜索超过 2 秒执行时限".to_owned()));
                            // SAFETY: Foundation 只在本次同步 callback 期间提供可写 BOOL stop 指针。
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        // SAFETY: Foundation 的 result 要么 nil（进度）要么是在本次同步 callback 内有效的 match 对象。
                        let Some(result) = (unsafe { result.as_ref() }) else {
                            // NSMatchingReportProgress 的 nil result 是进度，不是一个空匹配。
                            return;
                        };
                        let native = result.range();
                        let Some(end) = native
                            .location
                            .checked_add(native.length)
                            .filter(|end| *end <= length)
                        else {
                            error.replace(Some("正则表达式返回了无效的 UTF-16 范围".to_owned()));
                            // SAFETY: 相同的同步 Foundation stop 输出，明确终止整个动作。
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        };
                        let range = native.location..end;
                        let validated = {
                            // Native result 已转成纯标量后才借 canonical；借用在 visitor/Native 返回前结束。
                            let document = document.borrow();
                            document
                                .byte_offset_for_utf16(range.start)
                                .and_then(|_| document.byte_offset_for_utf16(range.end))
                                .and_then(|_| {
                                    if self.word {
                                        whole_word_boundary(&document, range.clone())
                                    } else {
                                        Ok(true)
                                    }
                                })
                        };
                        let admitted = match validated {
                            Ok(admitted) => admitted, // 正文边界验证后才允许业务动作。
                            Err(failure) => {
                                error.replace(Some(failure.to_string()));
                                false
                            } // 非 scalar 坐标不能替换。
                        };
                        let admitted = admitted
                            && (!range.is_empty()
                                || accepted_end.get().is_none_or(|prior| range.start > prior));
                        if admitted {
                            // baseline 略过最后已接受匹配末尾的相邻 zero；保持批量枚举，不逐匹配重开引擎。
                            accepted_end.set(Some(range.end));
                            let range_count = if capture_ranges == CaptureRanges::Include {
                                result.numberOfRanges()
                            } else {
                                0
                            };
                            let mut captures = Vec::with_capacity(range_count);
                            for index in 0..range_count {
                                // 只拷贝范围，不持 native result 或复制捕获正文。
                                let capture = result.rangeAtIndex(index);
                                captures.push(if capture.location == NSNotFound as usize {
                                    None
                                } else {
                                    Some(
                                        capture.location
                                            ..capture.location.saturating_add(capture.length),
                                    )
                                });
                            }
                            match (visitor.borrow_mut())(Found {
                                range: range.clone(),
                                precise: true,
                                captures,
                            }) {
                                Ok(true) => {} // 继续本批同步枚举。
                                Ok(false) => {
                                    stopped.set(true);
                                } // 导航已得到需要的结果。
                                Err(failure) => {
                                    error.replace(Some(failure));
                                } // 不签发部分 ReplaceAll。
                            }
                        }
                        let following = if range.is_empty() && end < length {
                            // 零长匹配至少跨过一个完整 scalar，不在 surrogate 内重开搜索。
                            let document = document.borrow();
                            end + if document
                                .utf16_code_unit_at(end)
                                .is_ok_and(|unit| (0xd800..=0xdbff).contains(&unit))
                            {
                                2
                            } else {
                                1
                            }
                        } else if range.is_empty() {
                            length.saturating_add(1)
                        } else {
                            end
                        };
                        next.set(following);
                        count.set(count.get() + 1);
                        if count.get() >= 1_024 {
                            batch_full.set(true);
                        }
                        if stopped.get() || error.borrow().is_some() || batch_full.get() {
                            // SAFETY: 当前 Foundation callback 的 stop 指针有效；只结束本轮，不改变 source。
                            unsafe { *stop.as_mut() = Bool::YES };
                        }
                    },
                );
                // ReportProgress 允许在没有匹配的灾难性回溯中检查单调 deadline；没有 timer/重试线程。
                regex.enumerateMatchesInString_options_range_usingBlock(
                    native_string,
                    NSMatchingOptions::ReportProgress
                        | NSMatchingOptions::WithTransparentBounds
                        | NSMatchingOptions::WithoutAnchoringBounds,
                    NSRange::new(next.get(), length - next.get()),
                    &block,
                );
            });
            if let Some(failure) = error.take() {
                return Err(failure);
            }
            if stopped.get() || !batch_full.get() || next.get() > length {
                return Ok(()); // 完成、明确早停或最终零长匹配，均不再次投递搜索。
            }
        }
    }
}

/// 导航只保留五个候选；替换排除当前项，普通导航允许唯一非空匹配回绕。
#[derive(Default)]
struct NavigationMatches {
    first: Option<Found>,
    last: Option<Found>,
    previous: Option<Found>,
    next: Option<Found>,
    current: Option<Found>,
}

impl NavigationMatches {
    /// 与旧编辑器从 selection 边界开始再回绕的行为一致；零长当前位置仍必须前进。
    fn navigation_target(self, backwards: bool) -> Option<Found> {
        let target = if backwards {
            // 优先前一处，越过文件起点后选择最后一处非当前匹配。
            self.previous.or(self.last)
        } else {
            // 优先后一处，越过文件终点后选择第一处非当前匹配。
            self.next.or(self.first)
        };
        target.or_else(|| {
            // 唯一非空匹配仍是有效搜索结果；不能把“已选中”误报为“没有匹配”。
            self.current.filter(|found| !found.range.is_empty())
        })
    }

    fn observe(&mut self, found: Found, selection: &Range<usize>) {
        if found.range == *selection {
            // 当前选择只为 ReplaceNext 提供替换候选；零长当前匹配也不能自我回绕。
            self.current = Some(found);
            return;
        }
        if self.first.is_none() {
            // 全文第一个非当前匹配用于正向回绕。
            self.first = Some(found.clone());
        }
        if found.range.end <= selection.start {
            // 升序扫描中的最后一个前置匹配就是 Previous；不必反向解码或再次编译查询。
            self.previous = Some(found.clone());
        }
        if self.next.is_none() && found.range.start >= selection.end {
            // 只保留首次满足正向位置的候选，后续结果不覆盖它。
            self.next = Some(found.clone());
        }
        self.last = Some(found); // 反向回绕只需要最后一个非当前匹配，内存与结果总数无关。
    }
}

/// 导航只保留首/末/当前候选；替换先封存匿名记录，再一次提交原子 canonical 事务。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn execute_textkit_find_action(
    bar: &Rc<NativeMarkdownFindBar>,
    view: &PieceBackedMarkdownTextView,
    controller: &NativeMarkdownController,
    intent: u64,
    action: MarkdownFindAction,
) -> Result<(), String> {
    // 空尾槽先于所有 Native claim 声明；只有实际替换才放入原 guard，最终 string/storage/regex 析构后再清槽。
    let pending_tail: RefCell<Option<NativeMarkdownPendingSelectionGuard>> = RefCell::new(None);
    // 任何显式动作先撤掉前次延后目标；当前controller票据在所有查询Native getter之前固定。
    view.cancel_pending_find_reveal();
    // 在查询Native getter之前固定栏状态；隐藏仅允许原Next/Previous，不自动显示或领取新权限。
    let bar_revision = bar.layout_revision.get();
    let visibility =
        NativeMarkdownFindVisibility::capture(action, bar.visible.get(), bar.closed.get())
            .ok_or("查找请求已失效")?;
    let table_epoch = view.ivars().table_input_intent_epoch.get();
    // 首个栏/正文Native getter之前固定原Core选择票；明确Find新目标不要求旧非空主head已知。
    let selection_capture = view
        .ivars()
        .folds
        .capture_selection()
        .map_err(|error| error.to_string())?;
    let current = || {
        native_markdown_controller_has_foreground_authority(controller)
            && controller.find_input_intent() == intent
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == table_epoch
            && visibility.permits(
                bar.visible.get(),
                bar.closed.get(),
                bar_revision,
                bar.layout_revision.get(),
            )
    };
    if !current() {
        return Err("查找请求已失效".into());
    }
    let read_current = || current() && selection_capture.current();
    let query = native_markdown_table_intent_step(&read_current, || Query::read(bar, action))
        .map_err(|_| "读取查找输入时原权限已失效")??;
    if !read_current() {
        return Err("查找输入已变更".into());
    }
    let storage =
        native_markdown_table_intent_step(&read_current, || view.ivars().text_storage.load())
            .map_err(|_| "读取笔记存储时原查找权限已失效")?
            .ok_or("笔记编辑器已关闭")?;
    let document = storage.document().ok_or("笔记文档已关闭")?;
    if !view.ivars().folds.matches_document(&document) {
        return Err("查找选择与原笔记身份不一致".into());
    }
    let (generation, length, canonical_before) = {
        let document = document.borrow();
        (
            document.generation(),
            document.utf16_len(),
            document.selection(),
        )
    };
    // 完整anchor/head与查找使用的有序范围分别保存；后续Native getter不能把反向历史变成正向。
    let native_string = native_markdown_table_intent_step(&read_current, || storage.string())
        .map_err(|_| "读取查找正文时原权限已失效")?;
    let selected = native_markdown_table_intent_step(&read_current, || {
        view.native_ordered_selection_state_for_capture(&selection_capture)
    })
    .map_err(|_| "读取查找原选区时权限已失效")?
    .map_err(|error| error.to_string())?;
    let (native_before, selection) = match selected {
        NativeMarkdownOrderedSelection::Single(range) => {
            let end = range
                .location
                .checked_add(range.length)
                .filter(|end| *end <= length)
                .ok_or("查找原选区越过正文边界")?;
            (Some(range), range.location..end)
        }
        NativeMarkdownOrderedSelection::Multiple => {
            // 多选没有公开主head；扫描从仍持有的Core范围出发，明确新目标由Find命令授权。
            (
                None,
                canonical_before
                    .anchor_utf16
                    .min(canonical_before.head_utf16)
                    ..canonical_before
                        .anchor_utf16
                        .max(canonical_before.head_utf16),
            )
        }
    };
    let mut navigation = NavigationMatches::default();
    let mut matches = Vec::new();
    // 模板 owner 比 spool/reader/core 借用活得久，退出动作时一次释放所有 arena/token 容量。
    let capture_count = if query.regex {
        query.compiled_regex()?.numberOfCaptureGroups() + 1
    } else {
        0
    };
    let template = ReplacementTemplate::parse(&query.replacement, query.regex, capture_count);
    let mut spool = if action == MarkdownFindAction::ReplaceAll {
        Some(ReplacementSpool::new(&template)?)
    } else {
        None
    };
    let capture_ranges = if !template.captures.is_empty()
        && matches!(
            action,
            MarkdownFindAction::ReplaceNext | MarkdownFindAction::ReplaceAll
        ) {
        CaptureRanges::Include
    } else {
        CaptureRanges::Omit
    };
    let overlapping = matches!(
        action,
        MarkdownFindAction::Next | MarkdownFindAction::Previous | MarkdownFindAction::ReplaceNext
    );
    let consume = |found: Found| {
        if !read_current() {
            return Err("查找期间原选择或输入权限已失效".into());
        }
        if action == MarkdownFindAction::SelectAll {
            // 限额超出时整个动作失败，尚未向 AppKit 发布任何部分选择。
            if matches.len() == 1_000 {
                return Err("全部选中最多支持 1,000 处匹配".into());
            }
            matches.push(found.range.clone());
        }
        if let Some(spool) = &mut spool
            && found.precise
        {
            // 只写坐标；模板 literal 与捕获 token 已在动作开头解析，逐命中不再复制。
            spool.push(&found)?;
        }
        if matches!(
            action,
            MarkdownFindAction::Next
                | MarkdownFindAction::Previous
                | MarkdownFindAction::ReplaceNext
        ) {
            // SelectAll/ReplaceAll 不保留导航候选，也不复制其 capture 描述符。
            navigation.observe(found, &selection);
        }
        Ok(true)
    };
    query.visit(
        &document,
        &native_string,
        overlapping,
        capture_ranges,
        consume,
    )?;
    if document.borrow().generation() != generation || !read_current() {
        return Err("查找期间文档已变更，请重试".into());
    }
    let target = match action {
        MarkdownFindAction::Previous => {
            // 双向导航允许唯一非空匹配回绕，仍只消费本轮扫描的有限候选。
            navigation.navigation_target(true)
        }
        MarkdownFindAction::Next => {
            // 选择本身不是替换，回绕同一非空匹配不会再编辑正文或搜索新插入文字。
            navigation.navigation_target(false)
        }
        MarkdownFindAction::ReplaceNext => {
            let target = navigation.next.or(navigation.first);
            if let Some(found) = navigation.current.filter(|found| found.precise) {
                // 下一候选来自旧 snapshot，先映射再提交；不搜索本次刚插入的 replacement 文本。
                let mut replacement = ReplacementSpool::new(&template)?;
                replacement.push(&found)?;
                let before = canonical_before; // Undo恢复原方向；明确的新Find目标才采用start→end。
                let after = if let Some(target) = &target {
                    // 尚未改变 canonical，candidate 的两个端点仍以旧坐标计算。
                    replacement.map_selection(MarkdownTextSelection {
                        anchor_utf16: target.range.start,
                        head_utf16: target.range.end,
                    })?
                } else {
                    // 最后一个匹配被替换后保留新片段末尾 caret，不把 replacement 再次当成旧匹配。
                    MarkdownTextSelection::caret(found.range.start + replacement.inserted_utf16())
                };
                let (edit, committed, pending) = storage.apply_find_replacements(
                    &{
                        let captured = selection_capture
                            .clone()
                            .finish(canonical_before, false)
                            .map_err(|error| error.to_string())?;
                        match native_before {
                            Some(before) => captured
                                .with_observed_native_before(before)
                                .map_err(|error| error.to_string())?,
                            None => captured, // 实际 Multi 不伪造旧主范围；完整目标仍只来自本次明确 Find 动作。
                        }
                    },
                    replacement.into_replacements()?,
                    generation,
                    before,
                    after,
                )?;
                drop(pending_tail.replace(pending));
                // storage 返回纯发布时构造的同一 Rc；查找定位直接复用，不再另包一层收据身份。
                if !current() || !committed.current() {
                    return Ok(()); // 正文已提交；后继输入撤权时不再移动旧选择或覆盖新状态。
                }
                // 原 pending guard 仍覆盖这里的同步 Native 后果；先用同一 Rc 精确安装 Core after，
                // 再把它交给后续 reveal，不让等待可见区布局期间的旧 Native before 冒充已完成选择。
                if !view.apply_committed_selection_if(&committed, current) {
                    return Ok(()); // 已提交正文保留；原显示票失权时不 fresh capture 或重放替换。
                }
                let start = after.anchor_utf16.min(after.head_utf16);
                let end = after.anchor_utf16.max(after.head_utf16);
                // 事务已提交；新代次目标仍沿原输入票据定位，不能以选区已发布冒充可见。
                let revealed = committed.native_step(|| {
                    view.begin_find_reveal(
                        bar,
                        controller,
                        intent,
                        NativeMarkdownFindRevealTarget {
                            document: Rc::downgrade(&document),
                            // core原子事务每次实际提交只推进一代；Native通知重入的新编辑不可冒领此收据。
                            generation: if edit.is_some() {
                                generation.wrapping_add(1)
                            } else {
                                generation
                            },
                            range: start..end,
                            visibility,
                            bar_revision,
                            table_epoch,
                            selection: NativeMarkdownFindRevealSelection::Committed(Rc::clone(
                                &committed,
                            )),
                            feedback: if target.is_some() {
                                NativeMarkdownFindRevealFeedback::ReplacedNext
                            } else {
                                NativeMarkdownFindRevealFeedback::ReplacedLast
                            },
                        },
                    )
                });
                if !current() || !committed.current() {
                    return Ok(());
                }
                revealed.map_err(|error| error.to_string())??;
                return Ok(());
            }
            target // 没有精确的当前匹配时只前进，NFKD 字符内部命中不会误删未匹配内容。
        }
        MarkdownFindAction::ReplaceAll => {
            let mut spool = spool.expect("ReplaceAll creates one operation-local spool");
            let count = spool.count();
            let before = canonical_before; // map_selection逐端映射，不经过有序NSRange丢失方向。
            let after = spool.map_selection(before)?;
            let (_edit, committed, pending) = storage.apply_find_replacements(
                &{
                    let captured = selection_capture
                        .clone()
                        .finish(canonical_before, false)
                        .map_err(|error| error.to_string())?;
                    match native_before {
                        Some(before) => captured
                            .with_observed_native_before(before)
                            .map_err(|error| error.to_string())?,
                        None => captured,
                    }
                },
                spool.into_replacements()?,
                generation,
                before,
                after,
            )?;
            drop(pending_tail.replace(pending));
            // 同一次 pending guard 保留到选择安装、反馈字符串和最后状态 setter 都结束，不能提前清槽。
            // 原子提交成功才改变 native 选择，失败时字段、正文与选区全部保留。
            if !view.apply_committed_selection_if(&committed, current) {
                return Ok(());
            }
            let Ok(status) =
                committed.native_step(|| NSString::from_str(&format!("已替换 {count} 处匹配")))
            else {
                return Ok(()); // 旧提交反馈失效不再生成错误覆盖后继查询。
            };
            if current() {
                let _ = committed.native_step(|| bar.status.setStringValue(&status));
            }
            drop(status); // Native临时值析构结束后也不再从新epoch领取旧反馈权限。
            return Ok(());
        }
        MarkdownFindAction::SelectAll => {
            if let [target] = matches.as_slice() {
                // 唯一目标由本次明确命令授权，可发布完整方向；不让单caret通知意外撤掉原动作。
                let mut captured = selection_capture
                    .clone()
                    .finish(
                        MarkdownTextSelection {
                            anchor_utf16: target.start,
                            head_utf16: target.end,
                        },
                        true,
                    )
                    .map_err(|error| error.to_string())?;
                if let Some(before) = native_before {
                    captured = captured
                        .with_observed_native_before(before)
                        .map_err(|error| error.to_string())?;
                }
                let captured = captured
                    .with_head_side(Some(false))
                    .with_vertical_goal(None);
                let Some(committed) = storage.update_active_selection(captured) else {
                    return Err("全部选中的原选择权限已失效".into());
                };
                if !view.apply_committed_selection_if(&committed, current) {
                    return Ok(()); // 新输入只撤旧选择后果，不能回滚已发布的明确单目标。
                }
                if current() {
                    let _ = committed
                        .native_step(|| bar.status.setStringValue(ns_string!("已选中 1 处匹配")));
                }
                return Ok(());
            }
            let ranges: Vec<_> = matches
                .iter()
                .map(|range| {
                    native_markdown_table_intent_step(&read_current, || {
                        // SAFETY: 匹配范围已核对Core UTF-16边界，NSValue仅同步复制该有限值。
                        unsafe {
                            objc2_foundation::NSValue::valueWithRange(NSRange::new(
                                range.start,
                                range.len(),
                            ))
                        }
                    })
                    .map_err(|_| "构造全部匹配范围时原权限已失效")
                })
                .collect::<Result<_, _>>()?;
            let array = native_markdown_table_intent_step(&read_current, || {
                NSArray::from_retained_slice(&ranges)
            })
            .map_err(|_| "全部选中期间原选择权限已失效")?;
            if !ranges.is_empty() {
                native_markdown_table_intent_step(&read_current, || view.setSelectedRanges(&array))
                    .map_err(|_| "发布全部匹配范围时原权限已失效")?;
            }
            let status = native_markdown_table_intent_step(&read_current, || {
                NSString::from_str(&format!("已选中 {} 处匹配", ranges.len()))
            })
            .map_err(|_| "全部选中反馈已失效")?;
            native_markdown_table_intent_step(&read_current, || bar.status.setStringValue(&status))
                .map_err(|_| "全部选中反馈已被后继输入撤销")?;
            drop(status);
            drop(array);
            drop(ranges);
            if !matches.is_empty() && read_current() {
                // 多范围保留Native显示，临时claims结束后纯撤完整方向；绝不从数组猜主head。
                view.ivars().folds.reset_selection();
            }
            return Ok(());
        }
        MarkdownFindAction::Hide => return Ok(()), // 上层在读取查询前已经处理隐藏。
    };
    if let Some(target) = target {
        // 一次粗定位后交由原有bounded executor完成真实segment可见性认证。
        view.begin_find_reveal(
            bar,
            controller,
            intent,
            NativeMarkdownFindRevealTarget {
                document: Rc::downgrade(&document),
                generation,
                range: target.range,
                visibility,
                bar_revision,
                table_epoch,
                selection: NativeMarkdownFindRevealSelection::Captured {
                    capture: selection_capture.clone(),
                    native_before,
                },
                feedback: NativeMarkdownFindRevealFeedback::Found,
            },
        )?;
    } else {
        bar.status.setStringValue(ns_string!("没有匹配结果"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
