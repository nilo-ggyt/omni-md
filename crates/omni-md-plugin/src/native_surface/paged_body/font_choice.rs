//! 同一Body内复用完整小GCB的真实字体选择；不缓存Source、probe、Job、缺字计数或旧响应。

use std::{
    ops::Range,
    rc::{Rc, Weak},
};

use omni_markdown_core::{MarkdownLineComposeStyle, MarkdownLineFontProbe, MarkdownShapingOptions};

use super::{NativeMarkdownLineFont, NativeMarkdownPagedBodyError as Error, Result};

const CAPACITY: usize = 128;
const GRAPHEME_BYTES: usize = 32;
const CONTEXT_BYTES: usize = 20;
// 原Key的唯一最大Source包络；Producer只借此事实决定当前量子是否还能完整领取Key。
pub(super) const MAXIMUM_CHOICE_SOURCE_BYTES: usize = GRAPHEME_BYTES + 2 * CONTEXT_BYTES;

/// 原GCB全量与原shaper前后各20byte raw窗；相等必有相同的原五标量上下文，无需另一decoder。
#[derive(Eq, PartialEq)]
struct ChoiceInput {
    // 三段在原Source上连续，单个固定包络保留完全相同的raw上下文；长度仍区分各段边界。
    // 未使用尾部保持零，不从另一位置的相同字节串推断同一字素或BOT/EOT语义。
    bytes: [u8; MAXIMUM_CHOICE_SOURCE_BYTES],
    before_len: usize,
    grapheme_len: usize,
    after_len: usize,
}

/// 只持主字体弱身份、固定输入及有预算的原options；不以字号或PSName猜另一个实际Font。
pub(super) struct FontChoiceKey {
    primary: Weak<NativeMarkdownLineFont>,
    primary_points: u64,
    options: MarkdownShapingOptions,
    // feature的起止以全局UTF16 cluster寻址；非空时必须保原位置，不能跨位置套同一掩码。
    feature_range: Option<Range<usize>>,
    input: ChoiceInput,
}

impl FontChoiceKey {
    /// 在原probe/Source票下读取完整有限输入；大GCB不截断、不入缓存，继续原完整试次。
    /// 返回已发生的真实源读量，失败也不能漏记；miss的这份≤72byte成本不伪称原Job已读。
    pub(super) fn capture(
        probe: &MarkdownLineFontProbe,
        style: &MarkdownLineComposeStyle,
        primary: &Rc<NativeMarkdownLineFont>,
        current: &(impl Fn() -> bool + ?Sized),
    ) -> (usize, Result<Option<Self>>) {
        let mut read = 0_usize;
        let result = (|| {
            require_current_probe(probe, current)?;
            if !probe.shaping_segment_ready()? || !probe.complete_single_segment()? {
                // 整个外 GCB 可能包含多个真实 UBA/Script 段；不能把首段的选项或结果缓存成整簇事实。
                return Ok(None);
            }
            let range = probe.byte_range()?;
            if range.len() > GRAPHEME_BYTES {
                return Ok(None); // 不把超长组合字的前32byte冒充完整输入或已选Font。
            }
            let options = probe.shaping_options(style)?;
            let feature_range = (!options.features.is_empty()).then(|| probe.source_range());
            let length = probe.source()?.byte_len();
            let before = range.start.saturating_sub(CONTEXT_BYTES)..range.start;
            let after = range.end..range.end.saturating_add(CONTEXT_BYTES).min(length);
            let mut input = ChoiceInput {
                bytes: [0; MAXIMUM_CHOICE_SOURCE_BYTES],
                before_len: before.len(),
                grapheme_len: range.len(),
                after_len: after.len(),
            };
            // 原三段恰好相接，一次≤72B访问即可取得同样输入，避免每个缓存命中字素做三次pread。
            // 沿已认证byte端点读取，不能把远端UTF16再定位、整段扫描或新缓存混入这次优化。
            let envelope = before.start..after.end;
            require_current_probe(probe, current)?;
            let (used, copied) =
                probe.copy_choice_bytes(envelope.clone(), &mut input.bytes[..envelope.len()]);
            read = read.checked_add(used).ok_or(Error::CounterExhausted)?;
            copied?; // 首错仍带原typed链；跨piece后失败也只按真实已交付字节收费。
            require_current_probe(probe, current)?; // 复制期间无Native重入，回调撤权仍须在发布Key前拒绝。
            Ok(Some(Self {
                primary: Rc::downgrade(primary),
                primary_points: primary.native().point_size().to_bits(),
                options,
                feature_range,
                input,
            }))
        })();
        (read, result)
    }

    /// 浮点按原协议编码比较，features保原顺序/完整范围；只复用完全相同的真实整形选择输入。
    fn same(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.primary, &other.primary)
            && self.primary_points == other.primary_points
            && self.options.points.to_bits() == other.options.points.to_bits()
            && self.options.direction == other.options.direction
            && self.options.script == other.options.script
            && self.options.language == other.options.language
            && self.options.beginning_of_text == other.options.beginning_of_text
            && self.options.end_of_text == other.options.end_of_text
            && self.options.features == other.options.features
            && self.feature_range == other.feature_range
            && self.input == other.input
    }
}

/// 每次Native/current调用前后核同一原probe；Source错误不能被回调触发的普通撤权覆盖。
fn require_current_probe(
    probe: &MarkdownLineFontProbe,
    current: &(impl Fn() -> bool + ?Sized),
) -> Result<()> {
    probe.validate()?;
    let permitted = current();
    probe.validate()?;
    if !permitted {
        return Err(Error::PreparationRevoked);
    }
    Ok(())
}

struct Entry {
    key: FontChoiceKey,
    selected: Weak<NativeMarkdownLineFont>,
}

/// 固定128项，满后依次替换；没有全源目录、第二条异步队列或进程级字体状态。
pub(super) struct FontChoices {
    entries: Vec<Entry>,
    next: usize,
}

impl FontChoices {
    /// 空Body不预留输入载荷，首次真实Selected才分配固定槽位；生命周期始终归同一个Body。
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: 0,
        }
    }

    /// 返回仍由原palette保活的真实注册Font；过期弱值只是miss，不重建字体或借旧结果补票。
    /// 调用方仍须用当前probe、Source与Selection核验并沿同一个生产响应函数提交。
    pub(super) fn get(&self, key: &FontChoiceKey) -> Option<Rc<NativeMarkdownLineFont>> {
        self.entries.iter().find_map(|entry| {
            if entry.key.same(key) {
                entry.selected.upgrade()
            } else {
                None
            }
        })
    }

    /// 仅在原完整trial已Selected且真实glyph/FD/map归还、Font注册完成后调用。
    /// 不接收TrialResult或缺字计数，缓存不会伪造成功收据；弱Font不延长映射/Native资源寿命。
    pub(super) fn remember(&mut self, key: FontChoiceKey, selected: &Rc<NativeMarkdownLineFont>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key.same(&key)) {
            entry.selected = Rc::downgrade(selected);
            return; // 原输入已相同，丢弃新key的有限metadata，不增加重复历史。
        }
        let entry = Entry {
            key,
            selected: Rc::downgrade(selected),
        };
        if self.entries.len() < CAPACITY {
            if self.entries.capacity() == 0 {
                self.entries = Vec::with_capacity(CAPACITY);
            }
            self.entries.push(entry);
            return; // 容量只在第一次Selected取得，此后永远不扩到第129项。
        }
        self.entries[self.next] = entry;
        self.next = (self.next + 1) % CAPACITY;
    }

    /// 清理同时释放所有key及槽位分配；关闭后的Body不保上下文字节、字体弱控制块或容量。
    pub(super) fn clear(&mut self) {
        self.entries = Vec::new();
        self.next = 0;
    }
}
