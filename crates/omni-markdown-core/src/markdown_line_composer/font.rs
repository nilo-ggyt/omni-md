//! 有限字体槽只保存弱值；原生palette持有实际注册owner，过期槽不能给旧字形重签身份。

use std::sync::{Arc, Mutex, Weak};

use super::{MarkdownLineComposeError as Error, item::Result};
use crate::{MarkdownLayoutFont, MarkdownLayoutFontAuthority};

pub const MARKDOWN_LINE_FONT_SLOTS: usize = 64;

struct Value {
    font: MarkdownLayoutFont,
}
struct Slot {
    generation: u64,
    value: Weak<Value>,
}
struct State {
    slots: [Slot; MARKDOWN_LINE_FONT_SLOTS],
}
struct Catalog {
    state: Mutex<State>,
}

/// 不含Font强边的固定表；不因为文档item数增长而扩容或追加历史字体列表。
#[derive(Clone)]
pub struct MarkdownLineFontCatalog(Arc<Catalog>);

/// 实际palette保留的不可变注册值；它只拥有Core Font，没有Native对象或正文回链。
#[derive(Clone)]
pub struct MarkdownLineRegisteredFont {
    catalog: Weak<Catalog>,
    key: FontKey,
    value: Arc<Value>,
}

/// 可跨调度边界的纯弱许可；公开数字坐标不能构造它，也不能使已过期Font复活。
#[derive(Clone)]
pub struct MarkdownLineFontRegistration {
    catalog: Weak<Catalog>,
    key: FontKey,
    value: Weak<Value>,
}

/// 只能在同一个catalog的私有输出目录中编码；不单独授予读取或绘制字体的权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FontKey {
    pub slot: u16,
    pub generation: u64,
}

impl Default for MarkdownLineFontCatalog {
    fn default() -> Self {
        Self::new()
    }
}
impl MarkdownLineFontCatalog {
    pub fn new() -> Self {
        Self(Arc::new(Catalog {
            state: Mutex::new(State {
                slots: std::array::from_fn(|_| Slot {
                    generation: 0,
                    value: Weak::new(),
                }),
            }),
        }))
    }

    /// 同一个活Font payload复用实际值；相同PSName或相同字节的新resolve不是同一身份。
    pub fn register(&self, font: &MarkdownLayoutFont) -> Result<MarkdownLineRegisteredFont> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| Error::FontCatalogUnavailable)?;
        for (ordinal, slot) in state.slots.iter().enumerate() {
            if let Some(value) = slot.value.upgrade()
                && font.permits(&value.font.authority())
            {
                return Ok(MarkdownLineRegisteredFont {
                    catalog: Arc::downgrade(&self.0),
                    key: FontKey {
                        slot: ordinal as u16,
                        generation: slot.generation,
                    },
                    value,
                });
            }
        } // 只做固定64槽扫描；注册不复制字体、不读完整font hash，也不触碰Native。
        let (ordinal, slot) = state
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.value.upgrade().is_none())
            .ok_or(Error::FontBudgetExceeded)?;
        let generation = slot.generation.checked_add(1).ok_or(Error::Overflow)?;
        let value = Arc::new(Value { font: font.clone() });
        slot.generation = generation;
        slot.value = Arc::downgrade(&value);
        Ok(MarkdownLineRegisteredFont {
            catalog: Arc::downgrade(&self.0),
            key: FontKey {
                slot: ordinal as u16,
                generation,
            },
            value,
        })
    }

    /// 私有目录只带slot与代次；必须仍借原catalog和该代次的实际活值才能解释这些整数。
    pub(super) fn resolve(&self, key: FontKey) -> Result<MarkdownLineRegisteredFont> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| Error::FontCatalogUnavailable)?;
        let slot = state
            .slots
            .get(usize::from(key.slot))
            .ok_or(Error::FontExpired)?;
        if slot.generation != key.generation {
            return Err(Error::FontExpired); // 回收后的同一slot不得把旧glyph映射到另一个字体。
        }
        let value = slot.value.upgrade().ok_or(Error::FontExpired)?;
        Ok(MarkdownLineRegisteredFont {
            catalog: Arc::downgrade(&self.0),
            key,
            value,
        })
    }

    pub fn require_registration(
        &self,
        registration: &MarkdownLineFontRegistration,
    ) -> Result<MarkdownLineRegisteredFont> {
        if !Weak::ptr_eq(&registration.catalog, &Arc::downgrade(&self.0)) {
            return Err(Error::FontExpired);
        }
        let registered = self.resolve(registration.key())?;
        if !Weak::ptr_eq(&registration.value, &Arc::downgrade(&registered.value)) {
            return Err(Error::FontExpired);
        }
        Ok(registered)
    }
}

impl MarkdownLineRegisteredFont {
    pub fn font(&self) -> &MarkdownLayoutFont {
        &self.value.font
    }
    pub fn registration(&self) -> MarkdownLineFontRegistration {
        MarkdownLineFontRegistration {
            catalog: self.catalog.clone(),
            key: self.key,
            value: Arc::downgrade(&self.value),
        }
    }
}
impl MarkdownLineFontRegistration {
    /// 升级时先核同catalog/slot代次，再核同一个不可变注册值；没有按名称复活的恢复分支。
    pub fn lease(&self) -> Result<MarkdownLineRegisteredFont> {
        let catalog = self.catalog.upgrade().ok_or(Error::FontExpired)?;
        MarkdownLineFontCatalog(catalog).require_registration(self)
    }
    pub fn authority(&self) -> Result<MarkdownLayoutFontAuthority> {
        Ok(self.lease()?.font().authority())
    }
    pub(super) fn key(&self) -> FontKey {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> MarkdownLayoutFont {
        let bytes: &'static [u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
        ));
        MarkdownLayoutFont::resolve(Arc::new(bytes), "Roboto-Regular", &[]).unwrap()
    }
    #[test]
    fn weak_slots_reuse_same_payload_but_never_reauthorize_an_expired_generation() {
        let catalog = MarkdownLineFontCatalog::new();
        let actual = font();
        let first = catalog.register(&actual).unwrap();
        let token = first.registration();
        let duplicate = catalog.register(&actual).unwrap();
        assert_eq!(token.key(), duplicate.registration().key());
        let original_observer = actual.resource_observer();
        drop(duplicate);
        drop(first);
        drop(actual);
        assert!(original_observer.payload_released()); // Catalog和旧token都只保弱值。
        assert!(matches!(token.lease(), Err(Error::FontExpired)));
        let next = catalog.register(&font()).unwrap();
        assert_eq!(next.registration().key().slot, token.key().slot);
        assert_ne!(next.registration().key().generation, token.key().generation);
        assert!(matches!(token.lease(), Err(Error::FontExpired)));
    }
    #[test]
    fn catalog_is_fixed_and_foreign_catalog_or_poison_cannot_sign_font_permission() {
        let catalog = MarkdownLineFontCatalog::new();
        let mut keep = Vec::new(); // 有限测试oracle明确保64个不同Font；产品目录没有这个Vec。
        for _ in 0..MARKDOWN_LINE_FONT_SLOTS {
            keep.push(catalog.register(&font()).unwrap());
        }
        assert!(matches!(
            catalog.register(&font()),
            Err(Error::FontBudgetExceeded)
        ));
        let other = MarkdownLineFontCatalog::new();
        assert!(matches!(
            other.require_registration(&keep[0].registration()),
            Err(Error::FontExpired)
        ));
        let poisoned = catalog.clone();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = poisoned.0.state.lock().unwrap();
                panic!("有限故障控制：真实Mutex poison");
            }))
            .is_err()
        );
        assert!(matches!(
            keep[0].registration().lease(),
            Err(Error::FontCatalogUnavailable)
        ));
    }
}
