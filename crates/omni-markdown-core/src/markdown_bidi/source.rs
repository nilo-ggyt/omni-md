//! 只遍历冻结piece属性；同一固定4KiB窗口跨await保留，原lease已真实归还。
use super::*;
use prepare::{io::CooperativeStore, source::Reader};
use store::DiskStack;

pub(super) async fn capture_prepared(
    snapshot: &MarkdownTextSnapshot,
    source: &Range<usize>,
    bytes: Range<usize>,
    default_base: BidiBaseDirection,
    store: &mut CooperativeStore,
) -> Result<u32, BidiError> {
    snapshot.original_backing_health()?;
    store.healthy()?;
    let gate = std::rc::Rc::clone(&store.gate);
    let mut reader = Reader::new(snapshot, bytes.clone());
    let mut state = Capture {
        store,
        units: 0,
        byte: bytes.start,
        paragraph_start: 0,
        paragraphs: 0,
        base: default_base.level(),
        default_base: default_base.level(),
        isolates: DiskStack::new(),
    };
    while let Some((scalar, _)) = reader.next(&gate).await? {
        state.scalar(scalar).await?;
    }
    snapshot.original_backing_health()?;
    state.store.healthy()?;
    if state.units as usize != source.len()
        || state.byte != bytes.end
        || reader.position() != bytes.end
    {
        return Err(BidiError::CorruptMetadata);
    }
    if state.paragraph_start < state.units {
        state.finish_paragraph().await?;
    }
    Ok(state.paragraphs)
}

struct Capture<'a> {
    store: &'a mut CooperativeStore,
    units: u32,
    byte: usize,
    paragraph_start: u32,
    paragraphs: u32,
    base: Option<u8>,
    default_base: Option<u8>,
    isolates: DiskStack,
}

impl Capture<'_> {
    async fn scalar(&mut self, scalar: char) -> Result<(), BidiError> {
        use BidiClass::*;
        self.store.healthy()?; // 一个scalar最多4个源字节；不在类别计算后才发现已取消。
        // 属性来自已逐scalar核实的Unicode18增量；后续UBA分页规则不随数据替换而改变。
        let data = UnicodeBidiData;
        let class = data.bidi_class(scalar);
        let bracket = data.bidi_matched_opening_bracket(scalar);
        let length = scalar.len_utf16() as u32;
        let first = self.units;
        // 每UTF16单位一条元数据，使低代理项和原unicode-bidi UTF16语义逐项可比较。
        for offset in 0..length {
            Text {
                byte: u32::try_from(self.byte).map_err(|_| BidiError::CoordinateOverflow)?,
                bracket: bracket.map_or(0, |value| value.opening as u32),
                pair_end: NONE,
                pair_start_run: NONE,
                pair_end_run: NONE,
                original: class,
                processing: class,
                level: 0,
                flags: if offset != 0 {
                    2
                } else {
                    u8::from(length == 2)
                } | bracket.map_or(0, |value| if value.is_open { 4 } else { 8 })
                    | ((scalar.len_utf8() as u8 - 1) << 4),
                // bit4..5 保存本次原捕获已经解码的真实 UTF8 宽度，头/低代理记录共享该值。
                // 仍写同一条 32B 私有临时记录；段末消费者无需借下一段或回源重定位 byte 尾。
            }
            .write_prepared(self.store, self.units + offset)
            .await?;
        }
        self.units = self
            .units
            .checked_add(length)
            .ok_or(BidiError::CoordinateOverflow)?;
        self.byte += scalar.len_utf8();
        match class {
            B => self.finish_paragraph().await?,
            L | R | AL => {
                if let Some((start, _)) = self.store.stack_top(&self.isolates).await? {
                    let mut isolate = Text::read_prepared(self.store, start).await?;
                    if isolate.original == FSI {
                        isolate.original = if class == L { LRI } else { RLI };
                        isolate.processing = isolate.original;
                        isolate.write_prepared(self.store, start).await?;
                    }
                } else if self.base.is_none() {
                    self.base = Some(u8::from(class != L)); // P2只观察isolate之外的首个strong。
                }
            }
            RLI | LRI | FSI => {
                self.store
                    .stack_push(&mut self.isolates, first, NONE)
                    .await?
            }
            PDI => {
                self.store.stack_pop(&mut self.isolates).await?;
            }
            _ => {} // 弱、中性和格式字符不决定P2，但记录仍原样参与后续所有规则。
        }
        Ok(())
    }

    async fn finish_paragraph(&mut self) -> Result<(), BidiError> {
        Paragraph {
            start: self.paragraph_start,
            end: self.units,
            base: self.base.unwrap_or(0),
        }
        .write_prepared(self.store, self.paragraphs)
        .await?;
        self.paragraphs += 1;
        self.paragraph_start = self.units;
        self.base = self.default_base;
        self.isolates.clear(); // B结束整个UBA段；未匹配的isolate不能泄漏到下一段。
        Ok(())
    }
}
