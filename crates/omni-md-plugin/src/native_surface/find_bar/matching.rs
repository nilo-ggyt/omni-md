//! Adapt matching for explicit synchronous Find actions. Keep Foundation
//! enumeration separate from canonical mutable transactions.

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

/// The independent TextKit fixture explicitly supplies its actual graph;
/// production Find uses the paged backend directly.
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
/// Query values live for one synchronous action; all capacity is released when it ends.
pub(super) struct Query {
    pub(super) search: String,
    replacement: String,
    pub(super) case_sensitive: bool,
    pub(super) regex: bool,
    pub(super) word: bool,
    compiled: std::cell::OnceCell<Result<Retained<NSRegularExpression>, String>>,
}

/// Navigation needs only the main range. Capture coordinates are allocated only
/// for replacement templates that consume them, avoiding per-navigation-match vectors.
#[derive(Clone, Copy, Eq, PartialEq)]
enum CaptureRanges {
    Omit,
    Include,
}

/// A match carries scalar coordinates only, never an NSTextCheckingResult retained
/// beyond its Foundation callback.
#[derive(Clone, Debug)]
struct Found {
    range: Range<usize>,
    precise: bool,
    captures: Vec<Option<Range<usize>>>,
}

/// Decode only the baseline query language's four escapes. Preserve unknown
/// escapes for the regex engine or literal search.
pub(super) fn unquote(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        // Consume bounded input forward once. Recognized two-scalar escapes are
        // consumed together; unknown sequences retain their original characters.
        if character == '\\' {
            // Only defined query-language escapes change value; a trailing lone
            // backslash remains literal.
            let replacement = match chars.peek() {
                Some('n') => Some('\n'),  // Explicit line feed.
                Some('r') => Some('\r'),  // Explicit carriage return.
                Some('t') => Some('\t'),  // Explicit tab.
                Some('\\') => Some('\\'), // Explicit backslash.
                _ => None,                // Unknown escapes must not silently lose their backslash.
            };
            if let Some(replacement) = replacement {
                // Consume the recognized escape's second scalar so it is not emitted twice.
                chars.next();
                result.push(replacement);
                continue;
            }
        }
        result.push(character);
    }
    result
}

/// Translate only reviewed ECMAScript character classes/escapes. ICU's public
/// compiler still owns syntax validation; this is no complete JavaScript parser.
fn native_pattern(pattern: &str, case_sensitive: bool) -> String {
    let word = if case_sensitive {
        // Case-sensitive JavaScript word characters are ASCII letters, digits, and underscore.
        "A-Za-z0-9_"
    } else {
        // Unicode ignore-case adds the two scalars whose simple fold equals an ASCII word character.
        "A-Za-z0-9_ſK"
    };
    let space = r"\u0009-\u000d\u0020\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\ufeff";
    let word_atom = format!("(?-i:[{word}])");
    let mut output = String::new();
    let mut characters = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(character) = characters.next() {
        // Translate one token without recursively expanding user syntax; ICU
        // remains responsible for compiling and rejecting the resulting pattern.
        if character == '\\' {
            let Some(escape) = characters.next() else {
                // Preserve an incomplete escape for the compiler to reject; invent no valid query.
                output.push('\\');
                break;
            };
            let class = match escape {
                'w' => Some((false, word)), // JavaScript word differs from ICU Unicode word.
                'W' => Some((true, word)),  // Complement that same JavaScript set.
                'd' => Some((false, "0-9")), // JavaScript digits contain ASCII only.
                'D' => Some((true, "0-9")), // ICU Unicode digits must not broaden the complemented set.
                's' => Some((false, space)), // Include BOM; exclude ICU's NEL whitespace.
                'S' => Some((true, space)), // Complement the same JavaScript whitespace set.
                _ => None, // Other escapes follow the explicit translations or Native validation below.
            };
            if let Some((negative, class)) = class {
                // Public ICU UnicodeSet supports nested complements; positive
                // classes can expand directly inside an existing character class.
                if in_class && !negative {
                    // Reuse the outer character class rather than introducing literal brackets.
                    output.push_str(class);
                } else {
                    // Standalone atoms/complements need their own brackets to preserve set semantics.
                    output.push('[');
                    if negative {
                        // Complement this inner set only, not the user's whole pattern.
                        output.push('^');
                    }
                    output.push_str(class);
                    output.push(']');
                }
            } else if !in_class && matches!(escape, 'b' | 'B') {
                if escape == 'b' {
                    // A word boundary separates different word categories; zero-width assertions consume no text.
                    output.push_str(&format!(
                        "(?:(?<!{word_atom})(?={word_atom})|(?<={word_atom})(?!{word_atom}))"
                    ));
                } else {
                    // Equal categories form the complement; document edges count as nonword.
                    output.push_str(&format!(
                        "(?:(?<={word_atom})(?={word_atom})|(?<!{word_atom})(?!{word_atom}))"
                    ));
                }
            } else if escape == 'u' && characters.peek() == Some(&'{') {
                // JavaScript scalar escapes use a different prefix from ICU's public \x{...} syntax.
                output.push_str("\\x");
            } else {
                output.push('\\');
                output.push(escape); // ICU validates remaining syntax and returns an explicit error.
            }
        } else if character == '^' && !in_class {
            // ECMAScript multiline recognizes four terminators; ICU's native m also includes VT/FF/NEL.
            output.push_str(r"(?:\A|(?<=[\n\r\u2028\u2029]))");
        } else if character == '$' && !in_class {
            // Use the absolute document end, excluding ICU's additional line-ending semantics.
            output.push_str(r"(?:\z|(?=[\n\r\u2028\u2029]))");
        } else if character == '.' && !in_class {
            output.push_str(r"[^\n\r\u2028\u2029]"); // JavaScript dot permits NEL; preserve that behavior.
        } else {
            if character == '[' {
                // Dots/anchors inside a class are literal members, not outside-class tokens.
                in_class = true;
            }
            if character == ']' {
                // Close this class; ICU still rejects invalid/unpaired brackets.
                in_class = false;
            }
            output.push(character);
        }
    }
    output
}

/// Check Native length before bounded copying. Find must neither truncate user
/// input nor commit/cancel an active composition to obtain a value.
fn read_field(field: &NSTextField) -> Result<String, String> {
    if let Some(editor) = field.currentEditor() {
        let editor = editor
            .downcast_ref::<NSTextView>()
            .ok_or("查找字段的原输入上下文不可用")?;
        // SAFETY: The actual field editor was checked as NSTextView; the public
        // NSTextInputClient method only observes its marked-text state.
        if unsafe { msg_send![editor, hasMarkedText] } {
            return Err("请先完成当前输入法组合，再执行查找或替换".into());
        }
    } // Buttons, menus, and Return share this composition boundary and never read unfinished candidates.
    let value = field.stringValue();
    if value.length() > 16_384 {
        // Reject oversized Native values before copying, without silently truncating a user's template.
        return Err("查找与替换输入最多为 16,384 个 UTF-16 单元".into());
    }
    Ok(value.to_string())
}

impl Query {
    /// Automatic feedback reads the original query and three options, never the
    /// replacement candidate. Each Native getter obeys the same original input ticket.
    pub(super) fn read_viewport(
        bar: &NativeMarkdownFindBar,
        current: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        Ok(Self {
            search: native_markdown_table_intent_step(current, || read_field(&bar.query))
                .map_err(|_| "原查询输入已变化")??,
            replacement: String::new(), // Feedback neither parses replacement templates nor performs replacement.
            case_sensitive: native_markdown_table_intent_step(current, || bar.case.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            regex: native_markdown_table_intent_step(current, || bar.regex.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            word: native_markdown_table_intent_step(current, || bar.word.state() != 0)
                .map_err(|_| "原查询选项已变化")?,
            compiled: std::cell::OnceCell::new(),
        })
    }

    /// Read a template only for replacement. Composition in the replacement field
    /// must not block unrelated query navigation.
    fn read(bar: &NativeMarkdownFindBar, action: MarkdownFindAction) -> Result<Self, String> {
        Ok(Self {
            search: read_field(&bar.query)?,
            replacement: match action {
                MarkdownFindAction::ReplaceNext | MarkdownFindAction::ReplaceAll => {
                    unquote(&read_field(&bar.replacement)?) // Preserve replacement composition, length, and escape contracts.
                }
                MarkdownFindAction::Next
                | MarkdownFindAction::Previous
                | MarkdownFindAction::SelectAll
                | MarkdownFindAction::Hide => String::new(), // Navigation/hide read no replacement candidate/template.
            },
            case_sensitive: bar.case.state() != 0,
            regex: bar.regex.state() != 0,
            word: bar.word.state() != 0,
            compiled: std::cell::OnceCell::new(),
        })
    }

    /// Compile once per action or bounded feedback call. Template parsing and
    /// enumeration share this one read-only Native regex object.
    pub(super) fn compiled_regex(&self) -> Result<&NSRegularExpression, String> {
        self.compiled
            .get_or_init(|| {
                let mut options = NSRegularExpressionOptions::empty();
                if !self.case_sensitive {
                    // This flag introduces ICU full folding; reviewed differences
                    // must not be described as JavaScript simple folding equivalence.
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

    /// Literal matching streams NFKD; regex uses public ICU APIs. Independent
    /// oracle data records syntax differences separately.
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

    /// Tests may choose a shorter real elapsed-time budget. Product progress/result
    /// callbacks cooperatively check two seconds and return cancellation as an error.
    /// Compilation and gaps between callbacks have no hard real-time interruption;
    /// the budget is not an input-to-return latency guarantee.
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
            return Ok(()); // Empty queries enumerate no matches, avoiding unbounded zero-length searching.
        }
        if !self.regex {
            // Pure Rust KMP scans fixed source windows without constructing a
            // normalized mirror of the complete document.
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
                    // Visitors use scalars/standard I/O only, with no Native calls
                    // or reentrant AppKit getters under the document borrow.
                    if started.elapsed() > budget {
                        // Visitor I/O counts toward the budget; cancellation keeps
                        // callers from committing records already generated.
                        callback_error.replace(Some("查找超过 2 秒执行时限".to_owned()));
                        return Ok(false);
                    }
                    match visit(Found {
                        captures: Vec::new(), // Literal templates need no per-hit capture allocation.
                        range: range_utf16,
                        precise,
                    }) {
                        Ok(proceed) => Ok(proceed), // Callers own early stopping; retain no later matches.
                        Err(error) => {
                            callback_error.replace(Some(error));
                            Ok(false)
                        } // Propagate the error after ending the read-only document borrow.
                    }
                },
            );
            result.map_err(|error| error.to_string())?;
            if let Some(error) = callback_error.take() {
                // Preserve the visitor's failure after releasing the canonical
                // borrow; the enclosing action drops its anonymous records.
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
            // End each batch after at most 1,024 results and drain its local pool,
            // preventing accumulation of unbounded autoreleased match objects.
            let count = Cell::new(0usize);
            let batch_full = Cell::new(false);
            autoreleasepool(|_| {
                let block = block2::StackBlock::new(
                    |result: *mut NSTextCheckingResult,
                     flags: NSMatchingFlags,
                     mut stop: NonNull<Bool>| {
                        if flags.contains(NSMatchingFlags::InternalError) {
                            // An engine failure is neither zero matches nor permission for partial replacement.
                            error.replace(Some("原生正则表达式引擎无法完成搜索".to_owned()));
                            // SAFETY: Foundation supplies valid writable stop storage for this synchronous callback.
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        if started.elapsed() > budget {
                            error.replace(Some("正则表达式搜索超过 2 秒执行时限".to_owned()));
                            // SAFETY: Foundation's BOOL stop pointer is writable only for this callback's lifetime.
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        }
                        // SAFETY: Foundation supplies nil for progress or a valid
                        // match object for this synchronous callback's lifetime.
                        let Some(result) = (unsafe { result.as_ref() }) else {
                            // A nil ReportProgress result is progress, not an empty match.
                            return;
                        };
                        let native = result.range();
                        let Some(end) = native
                            .location
                            .checked_add(native.length)
                            .filter(|end| *end <= length)
                        else {
                            error.replace(Some("正则表达式返回了无效的 UTF-16 范围".to_owned()));
                            // SAFETY: The same synchronous Foundation stop output explicitly ends this action.
                            unsafe { *stop.as_mut() = Bool::YES };
                            return;
                        };
                        let range = native.location..end;
                        let validated = {
                            // Borrow canonical source only after copying scalar
                            // coordinates; end the borrow before the visitor/Native callback returns.
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
                            Ok(admitted) => admitted, // Source-boundary validation precedes action admission.
                            Err(failure) => {
                                error.replace(Some(failure.to_string()));
                                false
                            } // Coordinates inside a scalar cannot authorize replacement.
                        };
                        let admitted = admitted
                            && (!range.is_empty()
                                || accepted_end.get().is_none_or(|prior| range.start > prior));
                        if admitted {
                            // The baseline skips an adjacent zero-length match at
                            // the last accepted end. Keep batched enumeration
                            // rather than restarting the engine for every match.
                            accepted_end.set(Some(range.end));
                            let range_count = if capture_ranges == CaptureRanges::Include {
                                result.numberOfRanges()
                            } else {
                                0
                            };
                            let mut captures = Vec::with_capacity(range_count);
                            for index in 0..range_count {
                                // Copy coordinates only, retaining no Native result or captured source text.
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
                                Ok(true) => {} // Continue this synchronous batch.
                                Ok(false) => {
                                    stopped.set(true);
                                } // The caller has the result it needs.
                                Err(failure) => {
                                    error.replace(Some(failure));
                                } // Failure cannot authorize a partial Replace All.
                            }
                        }
                        let following = if range.is_empty() && end < length {
                            // Advance an empty match by one complete scalar,
                            // never restarting search inside a surrogate pair.
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
                            // SAFETY: This callback's stop pointer remains valid;
                            // ending the batch does not modify the source.
                            unsafe { *stop.as_mut() = Bool::YES };
                        }
                    },
                );
                // ReportProgress permits cooperative deadline checks during
                // no-match backtracking, without a timer or retry thread.
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
                return Ok(()); // Completion, explicit stop, or final empty match never resubmits search.
            }
        }
    }
}

/// Navigation retains five candidates only. Replacement excludes the current
/// match; ordinary navigation may wrap to a sole nonempty match.
#[derive(Default)]
struct NavigationMatches {
    first: Option<NumberedMatch>,
    last: Option<NumberedMatch>,
    previous: Option<NumberedMatch>,
    next: Option<NumberedMatch>,
    current: Option<NumberedMatch>,
    total: usize,
}

/// Ordinals come only from this actual ascending scan and remain on at most five
/// candidates, never a complete hit list or another document copy.
#[derive(Clone)]
struct NumberedMatch {
    found: Found,
    ordinal: std::num::NonZeroUsize,
}

impl NavigationMatches {
    /// Preserve the original selection-boundary navigation/wrap behavior. An empty
    /// match at the current position must still advance.
    fn navigation_target(self, backwards: bool) -> Option<NumberedMatch> {
        let target = if backwards {
            // Prefer the preceding match; crossing document start wraps to the last noncurrent match.
            self.previous.or(self.last)
        } else {
            // Prefer the following match; crossing document end wraps to the first noncurrent match.
            self.next.or(self.first)
        };
        target.or_else(|| {
            // A sole nonempty selected match remains a result; selected does not mean absent.
            self.current.filter(|found| !found.found.range.is_empty())
        })
    }

    fn observe(&mut self, found: Found, selection: &Range<usize>) -> Result<(), String> {
        // Navigation already scans completely. Count actual hits at their entry;
        // byte positions or visible highlights cannot establish an ordinal.
        self.total = self.total.checked_add(1).ok_or("查找匹配数量超出范围")?;
        let found = NumberedMatch {
            found,
            ordinal: std::num::NonZeroUsize::new(self.total).expect("真实枚举计数在加一后必为正数"),
        };
        if found.found.range == *selection {
            // The current selection supplies ReplaceNext's candidate. An empty
            // current match cannot wrap to itself.
            self.current = Some(found);
            return Ok(());
        }
        if self.first.is_none() {
            // Keep the first noncurrent document match for forward wrap.
            self.first = Some(found.clone());
        }
        if found.found.range.end <= selection.start {
            // The last preceding match in ascending order is Previous; no reverse
            // decoding or second compilation is required.
            self.previous = Some(found.clone());
        }
        if self.next.is_none() && found.found.range.start >= selection.end {
            // Keep the first forward candidate without overwriting it with later results.
            self.next = Some(found.clone());
        }
        self.last = Some(found); // Reverse wrap needs only the final noncurrent match, independent of total hits.
        Ok(())
    }
}

/// Navigation retains bounded candidates; replacement seals anonymous records
/// before committing one atomic canonical transaction.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) fn execute_textkit_find_action(
    bar: &Rc<NativeMarkdownFindBar>,
    view: &PieceBackedMarkdownTextView,
    controller: &NativeMarkdownController,
    intent: u64,
    action: MarkdownFindAction,
) -> Result<(), String> {
    // Declare the empty tail slot before Native claims. Actual replacement alone
    // installs its guard, which outlives string/storage/regex destruction.
    let pending_tail: RefCell<Option<NativeMarkdownPendingSelectionGuard>> = RefCell::new(None);
    // Explicit actions revoke previous deferred targets before any Native query
    // getter; the controller ticket is already fixed at entry.
    view.cancel_pending_find_reveal();
    // Fix bar state before query getters. A hidden bar permits only original
    // Next/Previous navigation without showing controls or acquiring new authority.
    let bar_revision = bar.layout_revision.get();
    let visibility =
        NativeMarkdownFindVisibility::capture(action, bar.visible.get(), bar.closed.get())
            .ok_or("查找请求已失效")?;
    let table_epoch = view.ivars().table_input_intent_epoch.get();
    // Capture original Core selection before any bar/document Native getter. An
    // explicit new Find target does not require a known prior nonempty primary head.
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
    // Preserve directed anchor/head separately from Find's ordered range. Later
    // Native getters must not turn backward selection history into forward history.
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
            // Multiple selections expose no primary head; scan from the retained
            // Core extent, with the explicit Find command authorizing the new target.
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
    // The template owner outlives spool/reader/Core borrows; action exit releases
    // all literal-arena and token capacity together.
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
            // Exceeding the limit fails the whole action before AppKit sees any partial selection.
            if matches.len() == 1_000 {
                return Err("全部选中最多支持 1,000 处匹配".into());
            }
            matches.push(found.range.clone());
        }
        if let Some(spool) = &mut spool
            && found.precise
        {
            // Write coordinates only. Template literals/capture tokens were parsed
            // once at action entry and are not copied for every hit.
            spool.push(&found)?;
        }
        if matches!(
            action,
            MarkdownFindAction::Next
                | MarkdownFindAction::Previous
                | MarkdownFindAction::ReplaceNext
        ) {
            // SelectAll/ReplaceAll retain no navigation candidates or copied capture descriptors.
            navigation.observe(found, &selection)?;
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
    // Only complete enumeration and original-source verification authorize totals
    // for subsequent successful reveal feedback.
    let total = navigation.total;
    let target = match action {
        MarkdownFindAction::Previous => {
            // Bidirectional navigation may wrap to a sole nonempty match using only this scan's bounded candidates.
            navigation.navigation_target(true)
        }
        MarkdownFindAction::Next => {
            // Selection is not replacement; wrapping a nonempty match neither edits
            // the document nor searches newly inserted text.
            navigation.navigation_target(false)
        }
        MarkdownFindAction::ReplaceNext => {
            let target = navigation.next.or(navigation.first);
            if let Some(found) = navigation.current.filter(|found| found.found.precise) {
                let found = found.found; // Replacement consumes the original hit, not a pre-edit total for new text.
                // Map the next candidate from the original snapshot before commit;
                // do not search the replacement text inserted by this action.
                let mut replacement = ReplacementSpool::new(&template)?;
                replacement.push(&found)?;
                let before = canonical_before; // Undo restores direction; only the explicit new target uses start-to-end.
                let after = if let Some(target) = &target {
                    // Canonical text is unchanged; map both candidate endpoints from old coordinates.
                    replacement.map_selection(MarkdownTextSelection {
                        anchor_utf16: target.found.range.start,
                        head_utf16: target.found.range.end,
                    })?
                } else {
                    // After replacing the last match, leave the caret at the new
                    // fragment's end rather than treating inserted text as an old match.
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
                            None => captured, // Actual multi-selection invents no prior primary range; Find owns this target.
                        }
                    },
                    replacement.into_replacements()?,
                    generation,
                    before,
                    after,
                )?;
                drop(pending_tail.replace(pending));
                // Storage returns the same Rc created during pure publication;
                // reveal reuses it rather than wrapping another receipt identity.
                if !current() || !committed.current() {
                    return Ok(()); // Text is committed; successor input revokes only these old presentation effects.
                }
                // The original pending guard covers synchronous Native effects.
                // Install the exact Core after-selection through the same Rc before
                // reveal; the old Native before-selection cannot impersonate completion.
                if !view.apply_committed_selection_if(&committed, current) {
                    return Ok(()); // Preserve committed text; do not recapture authority or replay replacement.
                }
                let start = after.anchor_utf16.min(after.head_utf16);
                let end = after.anchor_utf16.max(after.head_utf16);
                // The committed new-generation target still uses original input
                // authority; published selection does not establish visibility.
                let revealed = committed.native_step(|| {
                    view.begin_find_reveal(
                        bar,
                        controller,
                        intent,
                        NativeMarkdownFindRevealTarget {
                            document: Rc::downgrade(&document),
                            // Each actual Core transaction advances one generation;
                            // an edit from Native notification reentry cannot claim this receipt.
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
            target // Without an exact current match, only navigate; never delete unmatched parts of an NFKD scalar.
        }
        MarkdownFindAction::ReplaceAll => {
            let mut spool = spool.expect("ReplaceAll creates one operation-local spool");
            let count = spool.count();
            let before = canonical_before; // Map endpoints independently without losing direction through ordered NSRange.
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
            // Retain the same pending guard through selection installation,
            // feedback-string release, and the last status setter. Native selection
            // changes only after atomic commit; failed edits preserve fields/text/selection.
            if !view.apply_committed_selection_if(&committed, current) {
                return Ok(());
            }
            let Ok(status) =
                committed.native_step(|| NSString::from_str(&format!("已替换 {count} 处匹配")))
            else {
                return Ok(()); // Revoked commit feedback cannot overwrite a successor query with an error.
            };
            if current() {
                let _ = committed.native_step(|| bar.status.setStringValue(&status));
            }
            drop(status); // Native temporary destruction cannot reacquire old feedback authority from a new epoch.
            return Ok(());
        }
        MarkdownFindAction::SelectAll => {
            if let [target] = matches.as_slice() {
                // The explicit command authorizes this sole directed target;
                // a single-caret notification must not inadvertently revoke the action.
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
                    return Ok(()); // New input revokes old visual effects, not the already published explicit target.
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
                        // SAFETY: Matches were checked against Core UTF-16
                        // boundaries; NSValue synchronously copies this bounded range.
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
                // Keep Native multi-range presentation. Once temporary claims end,
                // revoke directed selection without guessing a primary head from the array.
                view.ivars().folds.reset_selection();
            }
            return Ok(());
        }
        MarkdownFindAction::Hide => return Ok(()), // The owning layer handles hide before reading query text.
    };
    if let Some(target) = target {
        let feedback = if matches!(
            action,
            MarkdownFindAction::Next | MarkdownFindAction::Previous
        ) {
            NativeMarkdownFindRevealFeedback::Numbered(NativeMarkdownFindPosition {
                ordinal: target.ordinal,
                total: std::num::NonZeroUsize::new(total)
                    .expect("存在真实导航目标就必有完整正数总量"),
            })
        } else {
            NativeMarkdownFindRevealFeedback::Found // ReplaceNext without an edit preserves its original feedback semantics.
        };
        // After one coarse relocation, the existing bounded executor verifies
        // actual segment visibility under the same original target.
        view.begin_find_reveal(
            bar,
            controller,
            intent,
            NativeMarkdownFindRevealTarget {
                document: Rc::downgrade(&document),
                generation,
                range: target.found.range,
                visibility,
                bar_revision,
                table_epoch,
                selection: NativeMarkdownFindRevealSelection::Captured {
                    capture: selection_capture.clone(),
                    native_before,
                },
                feedback,
            },
        )?;
    } else {
        bar.status.setStringValue(ns_string!("没有匹配结果"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
