//! whole Parser 与源帧标题投影共用的 HTML/autolink 词法续体；不保存正文、属性或 URI。
//! 只有原调用者能确认容器前缀与真实输入终点；本模块只解释已经交来的同一词法序列。
//! 无效候选返回 Rejected，让原 inline 主游标从 '<' 后继续，而不吞掉可能可见的文字。

use super::{is_ascii_whitespace, is_ascii_whitespace_no_nl, is_valid_unquoted_attr_value_char};
use crate::parse::HtmlScanGuard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InlineHtmlMode {
    /// 原 inline '<' 分支同时判 tag、autolink 和特殊 HTML，不能把失败类型提前猜成另一类。
    #[cfg(any(test, feature = "omni-block-frontier"))]
    Any,
    /// whole tag入口不接 comment/PI，也不从普通属性文本推断URI。
    Tag,
    Autolink,
    Comment,
    Processing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InlineHtmlKind {
    Html,
    UriAutolink,
    EmailAutolink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InlineHtmlProgress {
    Progress,
    Accepted {
        kind: InlineHtmlKind,
        byte_count: usize,
    },
    Rejected,
    /// 完整逻辑换行已消费，下一源字节尚未消费；scope owner必须回答原prefix。
    NeedLine,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InlineHtmlStep {
    pub consumed: usize,
    pub byte_reads: usize,
    pub transitions: usize,
    pub progress: InlineHtmlProgress,
}

#[derive(Clone, Copy, Debug)]
enum Html {
    Start,
    Open,
    NameFirst,
    Name { closing: bool },
    Gap { closing: bool },
    // 属性名后的空白暂不决定它是布尔属性还是有值属性；后续'='才选择值分支。
    // 这个事实只需一个枚举，不重扫属性串，也不保存属性名或潜在超长quoted值。
    Attribute,
    AttributeGap,
    ValueGap,
    Quoted(u8),
    Unquoted,
    AfterValue,
    Slash,
    Bang,
    CommentDash,
    Comment(u8),
    CdataPrefix(u8),
    // 保原CDATA scanner的首个']'组闭合约定；不把HTML内容中的普通'>'当标签终点。
    Cdata,
    CdataClose,
    Declaration,
    Processing(bool),
    Rejected,
    Accepted,
}

#[derive(Clone, Copy, Debug)]
enum Uri {
    Start,
    First,
    Scheme(u8),
    Body,
    Rejected,
    Accepted,
}

#[derive(Clone, Copy, Debug)]
enum Email {
    Start,
    // local-part只需知道非空；域label才有原63字节限制，不能累计整封邮件地址。
    Local(bool),
    Domain { length: u8, dash: bool },
    Rejected,
    Accepted,
}

#[derive(Clone, Copy, Debug)]
enum GuardKind {
    Comment,
    Cdata,
    Declaration,
    Processing,
}

/// 任意输入长度只持有限词法状态。候选在合法 EOL 后停住，调用者必须先依据自己的
/// 原容器权限跳过 prefix，再继续；词法游标本身不签 Source、scope 或真实 EOF 权限。
#[derive(Debug)]
pub(crate) struct InlineHtmlScanner {
    mode: InlineHtmlMode,
    html: Html,
    uri: Uri,
    email: Email,
    next: usize,
    pending_cr: bool,
    need_line: bool,
    terminal: Option<InlineHtmlProgress>,
    guard_boundary: Option<usize>,
    guard_kind: Option<GuardKind>,
    source_start: usize,
    limits: HtmlScanGuard,
}

impl InlineHtmlScanner {
    pub(crate) fn new(mode: InlineHtmlMode, source_start: usize, guard: &HtmlScanGuard) -> Self {
        let any = match mode {
            #[cfg(any(test, feature = "omni-block-frontier"))]
            InlineHtmlMode::Any => true,
            _ => false,
        };
        Self {
            mode,
            html: if any || mode != InlineHtmlMode::Autolink {
                Html::Start
            } else {
                Html::Rejected
            },
            uri: if any || mode == InlineHtmlMode::Autolink {
                Uri::Start
            } else {
                Uri::Rejected
            },
            email: if any || mode == InlineHtmlMode::Autolink {
                Email::Start
            } else {
                Email::Rejected
            },
            next: 0,
            pending_cr: false,
            need_line: false,
            terminal: None,
            guard_boundary: None,
            guard_kind: None,
            source_start,
            // 只借同一原HTML负事实的四个标量；相对位置便于不同SourceFrame继续同一候选。
            // 这不是第二个历史目录，当前候选结束后仍由原cursor持唯一HtmlScanGuard。
            limits: HtmlScanGuard {
                comment: guard.comment.saturating_sub(source_start),
                cdata: guard.cdata.saturating_sub(source_start),
                declaration: guard.declaration.saturating_sub(source_start),
                processing: guard.processing.saturating_sub(source_start),
            },
        }
    }

    /// skip 是原调用者已经验证的物理 prefix 长度；不读这些字节，也不把它们拼入 HTML。
    /// 只能回答当前 NeedLine，一次消费后即失效；溢出直接退休本候选，不能回绕源坐标。
    pub(crate) fn continue_line(&mut self, skip: usize) -> bool {
        if !self.need_line {
            return false;
        }
        let Some(next) = self.next.checked_add(skip) else {
            self.terminal = Some(InlineHtmlProgress::Rejected);
            return false;
        };
        self.next = next;
        self.need_line = false;
        true
    }

    /// 原 HtmlScanGuard 只记同次失败的已检查位置；不存标题、URI 或下一次查询权限。
    pub(crate) fn record_failure(&self, guard: &mut HtmlScanGuard) {
        if self.terminal != Some(InlineHtmlProgress::Rejected) {
            return;
        }
        let Some((kind, boundary)) = self.guard_kind.zip(self.guard_boundary) else {
            return;
        };
        let Some(boundary) = self.source_start.checked_add(boundary) else {
            return;
        };
        match kind {
            GuardKind::Comment => guard.comment = boundary,
            GuardKind::Cdata => guard.cdata = boundary,
            GuardKind::Declaration => guard.declaration = boundary,
            GuardKind::Processing => guard.processing = boundary,
        }
    }

    /// 每个字节和实际推进的有限分支都消耗当前 fuel；用尽只归还状态，不制造片段 EOF。
    /// CR 结尾保一个布尔，直到下一片可判 CRLF 才交 NeedLine，容器不会在 LF 之前插入 prefix。
    pub(crate) fn feed(&mut self, input: &[u8], real_end: bool, fuel: usize) -> InlineHtmlStep {
        let mut step = InlineHtmlStep {
            consumed: 0,
            byte_reads: 0,
            transitions: 0,
            progress: InlineHtmlProgress::Progress,
        };
        if let Some(terminal) = self.terminal {
            step.progress = terminal;
            return step;
        }
        if self.need_line {
            step.progress = InlineHtmlProgress::NeedLine;
            return step;
        }
        loop {
            // 最多三个实际识别分支，加一次EOL/终态处理；预留覆盖本轮真实读取与转换。
            if step
                .byte_reads
                .saturating_add(step.transitions)
                .saturating_add(8)
                > fuel
            {
                return step;
            }
            if self.pending_cr {
                let Some(&byte) = input.get(step.consumed) else {
                    if !real_end {
                        return step;
                    }
                    self.pending_cr = false;
                    self.need_line = true;
                    step.transitions += 1;
                    step.progress = InlineHtmlProgress::NeedLine;
                    return step;
                };
                step.byte_reads += 1;
                step.transitions += 1;
                if byte == b'\n' {
                    if self.next == usize::MAX {
                        self.terminal = Some(InlineHtmlProgress::Rejected);
                        step.progress = InlineHtmlProgress::Rejected;
                        return step;
                    }
                    step.consumed += 1;
                    self.next += 1;
                }
                self.pending_cr = false;
                self.need_line = true;
                step.progress = InlineHtmlProgress::NeedLine;
                return step;
            }
            let Some(&byte) = input.get(step.consumed) else {
                if real_end {
                    if matches!(
                        self.html,
                        Html::Cdata | Html::CdataClose | Html::Declaration
                    ) {
                        self.guard_boundary = Some(self.next);
                    }
                    self.terminal = Some(InlineHtmlProgress::Rejected);
                    step.transitions += 1;
                    step.progress = InlineHtmlProgress::Rejected;
                }
                return step;
            };
            // prefix坐标也由本游标累计；来自内部调用者的skip一旦达到usize边界，
            // 后续字节必须拒绝，不能在debug panic或release回绕成另一个源位置。
            if self.next == usize::MAX {
                self.terminal = Some(InlineHtmlProgress::Rejected);
                step.progress = InlineHtmlProgress::Rejected;
                return step;
            }
            step.byte_reads += 1;
            if !matches!(self.html, Html::Rejected) {
                self.html = self.html_byte(byte);
                step.transitions += 1;
            }
            if !matches!(self.uri, Uri::Rejected) {
                self.uri = uri_byte(self.uri, byte);
                step.transitions += 1;
            }
            if !matches!(self.email, Email::Rejected) {
                self.email = email_byte(self.email, byte);
                step.transitions += 1;
            }
            self.next += 1;
            step.consumed += 1;
            let kind = if matches!(self.uri, Uri::Accepted) {
                Some(InlineHtmlKind::UriAutolink)
            } else if matches!(self.email, Email::Accepted) {
                Some(InlineHtmlKind::EmailAutolink)
            } else if matches!(self.html, Html::Accepted) {
                Some(InlineHtmlKind::Html)
            } else {
                None
            };
            if let Some(kind) = kind {
                let accepted = InlineHtmlProgress::Accepted {
                    kind,
                    byte_count: self.next,
                };
                self.terminal = Some(accepted);
                step.progress = accepted;
                return step;
            }
            if matches!(self.html, Html::Rejected)
                && matches!(self.uri, Uri::Rejected)
                && matches!(self.email, Email::Rejected)
            {
                self.terminal = Some(InlineHtmlProgress::Rejected);
                step.progress = InlineHtmlProgress::Rejected;
                return step;
            }
            if !matches!(self.html, Html::Rejected) && matches!(byte, b'\r' | b'\n') {
                if byte == b'\r' {
                    self.pending_cr = true;
                } else {
                    self.need_line = true;
                    step.progress = InlineHtmlProgress::NeedLine;
                    return step;
                }
            }
        }
    }

    /// 与原扫描器共用属性名、非引号值及空白字符表；只有真实语法分隔才改变阶段。
    /// 引号中的 '>' 不结束标签，注释/CDATA/PI 各保原闭合条件，不把任意片段末尾当闭合。
    fn html_byte(&mut self, byte: u8) -> Html {
        use Html::*;
        let any = match self.mode {
            #[cfg(any(test, feature = "omni-block-frontier"))]
            InlineHtmlMode::Any => true,
            _ => false,
        };
        let alpha = byte.is_ascii_alphabetic();
        let name = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-');
        let attribute = alpha || matches!(byte, b'_' | b':');
        let gap = is_ascii_whitespace(byte);
        match self.html {
            Start if byte == b'<' => Open,
            Open => match byte {
                b'!' if any || self.mode == InlineHtmlMode::Comment => Bang,
                b'?' if any || self.mode == InlineHtmlMode::Processing => {
                    if self.next + 1 <= self.limits.processing {
                        return Rejected;
                    }
                    self.guard_kind = Some(GuardKind::Processing);
                    self.guard_boundary = Some(self.next + 1);
                    Processing(false)
                }
                b'/' if any || self.mode == InlineHtmlMode::Tag => NameFirst,
                _ if alpha && (any || self.mode == InlineHtmlMode::Tag) => Name { closing: false },
                _ => Rejected,
            },
            NameFirst if alpha => Name { closing: true },
            Name { closing } if byte.is_ascii_alphanumeric() || byte == b'-' => Name { closing },
            // close tag没有属性，且原whole入口只准许不含换行的尾部空白。
            // opening tag才可以在真实EOL后请求容器前缀，并继续解释剩余属性。
            Name { .. } if byte == b'>' => Accepted,
            Name { closing: false } if byte == b'/' => Slash,
            Name { closing } if gap && (!closing || is_ascii_whitespace_no_nl(byte)) => {
                Gap { closing }
            }
            Gap { closing } if gap && (!closing || is_ascii_whitespace_no_nl(byte)) => {
                Gap { closing }
            }
            Gap { .. } if byte == b'>' => Accepted,
            Gap { closing: false } if byte == b'/' => Slash,
            Gap { closing: false } if attribute => Attribute,
            // 布尔属性之后的空白可以引出下一属性，也可以被后继'='收作赋值间隙。
            // quote闭合后仍要求真正分隔，不能错误接受<a x='y'z>这种拼接。
            Attribute if name => Attribute,
            Attribute if byte == b'=' => ValueGap,
            Attribute if gap => AttributeGap,
            Attribute if byte == b'>' => Accepted,
            Attribute if byte == b'/' => Slash,
            AttributeGap if gap => AttributeGap,
            AttributeGap if byte == b'=' => ValueGap,
            AttributeGap if attribute => Attribute,
            AttributeGap if byte == b'>' => Accepted,
            AttributeGap if byte == b'/' => Slash,
            ValueGap if gap => ValueGap,
            ValueGap if matches!(byte, b'\'' | b'"') => Quoted(byte),
            ValueGap if is_valid_unquoted_attr_value_char(byte) => Unquoted,
            Quoted(quote) if byte == quote => AfterValue,
            Quoted(quote) => Quoted(quote),
            Unquoted if is_valid_unquoted_attr_value_char(byte) => Unquoted,
            Unquoted if gap => Gap { closing: false },
            Unquoted if byte == b'>' => Accepted,
            AfterValue if gap => Gap { closing: false },
            AfterValue if byte == b'/' => Slash,
            AfterValue if byte == b'>' => Accepted,
            Slash if byte == b'>' => Accepted,
            // 特殊HTML各有自己的闭合条件；这些内容不能按普通tag的首个'>'截断。
            // 缺失闭合只保持候选未知，直到真实EOF才拒绝，不把某一片source frame当EOF。
            Bang if byte == b'-' => {
                if self.next + 1 <= self.limits.comment {
                    return Rejected;
                }
                self.guard_kind = Some(GuardKind::Comment);
                CommentDash
            }
            Bang if byte == b'[' => {
                if self.next + 1 <= self.limits.cdata {
                    return Rejected;
                }
                self.guard_kind = Some(GuardKind::Cdata);
                CdataPrefix(0)
            }
            Bang if alpha => {
                if self.next + 1 <= self.limits.declaration {
                    return Rejected;
                }
                self.guard_kind = Some(GuardKind::Declaration);
                Declaration
            }
            CommentDash if byte == b'-' => {
                self.guard_boundary = Some(self.next + 1);
                Comment(2)
            }
            Comment(dashes) if byte == b'>' && dashes >= 2 => Accepted,
            Comment(dashes) if byte == b'-' => {
                self.guard_boundary = Some(self.next + 1);
                Comment((dashes + 1).min(2))
            }
            Comment(_) => Comment(0),
            CdataPrefix(at) if b"CDATA[".get(usize::from(at)) == Some(&byte) => {
                if at == 5 {
                    Cdata
                } else {
                    CdataPrefix(at + 1)
                }
            }
            Cdata if byte == b']' => CdataClose,
            Cdata => Cdata,
            CdataClose if byte == b']' => CdataClose,
            CdataClose if byte == b'>' => Accepted,
            CdataClose => {
                self.guard_boundary = Some(self.next);
                Rejected
            }
            Declaration if byte == b'>' => Accepted,
            Declaration => Declaration,
            Processing(true) if byte == b'>' => Accepted,
            Processing(_) => {
                if byte == b'?' {
                    self.guard_boundary = Some(self.next + 1);
                }
                Processing(byte == b'?')
            }
            _ => Rejected,
        }
    }
}

/// URI 与邮件保原 scanner 的接受条件；只计有限scheme/域label，任意正文长度不占额外内存。
fn uri_byte(state: Uri, byte: u8) -> Uri {
    use Uri::*;
    match state {
        Start if byte == b'<' => First,
        First if byte.is_ascii_alphabetic() => Scheme(1),
        // 原scheme只接ASCII并保持2..32；冒号之后的原UTF8目标由上层Source保证。
        // 此处不解码实体、转义或percent，不把URI文本复制成另一个规范化实现。
        Scheme(length) if byte == b':' && (2..=32).contains(&length) => Body,
        Scheme(length)
            if length < 32
                && (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+')) =>
        {
            Scheme(length + 1)
        }
        Body if byte == b'>' => Accepted,
        Body if byte > b' ' && byte != b'<' => Body,
        _ => Rejected,
    }
}

fn email_byte(state: Email, byte: u8) -> Email {
    use Email::*;
    match state {
        Start if byte == b'<' => Local(false),
        Local(true) if byte == b'@' => Domain {
            length: 0,
            dash: false,
        },
        Local(_)
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'.' | b'!'
                        | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'/'
                        | b'='
                        | b'?'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                        | b'~'
                        | b'-'
                ) =>
        {
            Local(true)
        }
        // 每个域label独立核首尾'-'与63字节；'.'重置当前label，不保域名集合。
        // 第64字节即拒绝，不能靠总长度截断后仍给原邮件地址签一个有效前缀。
        Domain { length, .. } if length < 63 && byte.is_ascii_alphanumeric() => Domain {
            length: length + 1,
            dash: false,
        },
        Domain { length, .. } if (1..63).contains(&length) && byte == b'-' => Domain {
            length: length + 1,
            dash: true,
        },
        Domain {
            length,
            dash: false,
        } if length != 0 && byte == b'.' => Domain {
            length: 0,
            dash: false,
        },
        Domain {
            length,
            dash: false,
        } if length != 0 && byte == b'>' => Accepted,
        _ => Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, LinkType, Parser, Tag};

    #[test]
    /// 同一正式词法内核在单字节、UTF8半scalar及常规帧下均须给完整真实token，
    /// 再核whole Parser实际交出的HTML/URI事件；不制造SourceFrame或语义proof。
    fn fragmented_html_and_autolinks_keep_original_parser_values_and_real_end() {
        let long = format!("<a title='{}中文😀>' data-x=v>", "x".repeat(70_000));
        for (value, kind, destination) in [
            ("<a title='>中文😀' data-x=v>", InlineHtmlKind::Html, ""),
            ("<!-- first > and -- > until -->", InlineHtmlKind::Html, ""),
            ("<![CDATA[<&>]]>", InlineHtmlKind::Html, ""),
            ("<? first > last ?>", InlineHtmlKind::Html, ""),
            ("<!DECL first>", InlineHtmlKind::Html, ""),
            (long.as_str(), InlineHtmlKind::Html, ""),
            (
                "<https://example.invalid/中文😀?a=b>",
                InlineHtmlKind::UriAutolink,
                "https://example.invalid/中文😀?a=b",
            ),
            (
                "<mail+one@example.invalid>",
                InlineHtmlKind::EmailAutolink,
                "mail+one@example.invalid",
            ),
        ] {
            for width in [1, 7, 1024] {
                let mut scanner =
                    InlineHtmlScanner::new(InlineHtmlMode::Any, 0, &HtmlScanGuard::default());
                let mut next = 0;
                loop {
                    let end = (next + width).min(value.len());
                    let step = scanner.feed(&value.as_bytes()[next..end], end == value.len(), 96);
                    assert!(step.byte_reads + step.transitions <= 96);
                    next += step.consumed;
                    match step.progress {
                        InlineHtmlProgress::Progress => {}
                        InlineHtmlProgress::NeedLine => assert!(scanner.continue_line(0)),
                        InlineHtmlProgress::Accepted {
                            kind: actual,
                            byte_count,
                        } => {
                            assert_eq!(actual, kind);
                            assert_eq!(byte_count, value.len());
                            assert_eq!(next, value.len());
                            break;
                        }
                        InlineHtmlProgress::Rejected => panic!("真实固定HTML/URI未完整匹配"),
                    }
                }
            }
            // 前后保普通正文，确保验证的是inline事件，而非另一个HTML block入口。
            let source = format!("before {value} after\n");
            let mut html = Vec::new();
            let mut links = Vec::new();
            for event in Parser::new(&source) {
                match event {
                    Event::InlineHtml(value) => html.push(value.into_string()),
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        ..
                    }) => links.push((link_type, dest_url.into_string())),
                    _ => {}
                }
            }
            if kind == InlineHtmlKind::Html {
                assert_eq!(html, [value]);
                assert!(links.is_empty());
            } else {
                assert!(html.is_empty());
                assert_eq!(
                    links,
                    [(
                        if kind == InlineHtmlKind::EmailAutolink {
                            LinkType::Email
                        } else {
                            LinkType::Autolink
                        },
                        destination.to_owned()
                    )]
                );
                #[cfg(feature = "html")]
                if kind == InlineHtmlKind::EmailAutolink {
                    // Parser 的 Email 事件保原邮箱；正式 HTML writer 才添加 mailto:。
                    // 先独立核原标签与目标，再核固定用户输出，避免把词法值和呈现 URI 混作一层。
                    let mut rendered = String::new();
                    crate::html::push_html(&mut rendered, Parser::new(&source));
                    assert_eq!(
                        rendered,
                        "<p>before <a href=\"mailto:mail+one@example.invalid\">mail+one@example.invalid</a> after</p>\n"
                    );
                }
            }
        }
    }

    #[test]
    /// 真实容器前缀仅在完整CRLF之后跳过；quoted值内的'>'不能提前结束HTML。
    /// 输入中断不授EOF，后续有效后缀仍须闭合原token，而不是从第二行另建解析器。
    fn container_line_continuation_preserves_original_physical_html_range() {
        let source = b"<a title='one\r\n> two>'>";
        let mut scanner = InlineHtmlScanner::new(InlineHtmlMode::Any, 0, &HtmlScanGuard::default());
        let mut next = 0;
        loop {
            let end = (next + 1).min(source.len());
            let step = scanner.feed(&source[next..end], end == source.len(), 4096);
            next += step.consumed;
            match step.progress {
                InlineHtmlProgress::Progress => {}
                InlineHtmlProgress::NeedLine => {
                    assert!(source[..next].ends_with(b"\r\n"));
                    assert!(source[next..].starts_with(b"> "));
                    assert!(scanner.continue_line(2));
                    next += 2; // 跳的是同一固定原字节，不凭fixture外的数字补物理范围。
                }
                InlineHtmlProgress::Accepted { kind, byte_count } => {
                    assert_eq!(kind, InlineHtmlKind::Html);
                    assert_eq!(byte_count, source.len());
                    assert_eq!(next, source.len());
                    break;
                }
                InlineHtmlProgress::Rejected => panic!("完整quoted HTML被分片截断"),
            }
        }
        let html: Vec<_> = Parser::new("> before <a title='one\r\n> two>'> after\n")
            .filter_map(|event| match event {
                Event::InlineHtml(html) => Some(html.into_string()),
                _ => None,
            })
            .collect();
        assert_eq!(html, ["<a title='one\r\ntwo>'>"]);
        for value in [
            "<a x='unfinished>",
            "</a\n>",
            "<a@-invalid>",
            "<https: invalid>",
            "<!-- unfinished",
        ] {
            let mut scanner =
                InlineHtmlScanner::new(InlineHtmlMode::Any, 0, &HtmlScanGuard::default());
            let step = scanner.feed(value.as_bytes(), true, 4096);
            assert_eq!(step.progress, InlineHtmlProgress::Rejected);
        }
    }
}
