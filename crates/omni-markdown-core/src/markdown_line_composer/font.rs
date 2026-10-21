//! Fixed font slots retain weak values; the native palette owns actual registered
//! fonts. Reused slots cannot reauthorize glyphs from an expired registration.

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

/// A fixed catalog with no strong font edge, independent of document item count
/// and without an accumulating historical-font list.
#[derive(Clone)]
pub struct MarkdownLineFontCatalog(Arc<Catalog>);

/// An immutable registration owned by the actual palette. Retain only a Core
/// font, never a native object or a link back to document content.
#[derive(Clone)]
pub struct MarkdownLineRegisteredFont {
    catalog: Weak<Catalog>,
    key: FontKey,
    value: Arc<Value>,
}

/// Weak authority suitable for scheduling boundaries. Public numeric coordinates
/// cannot construct it or resurrect a font whose actual registration ended.
#[derive(Clone)]
pub struct MarkdownLineFontRegistration {
    catalog: Weak<Catalog>,
    key: FontKey,
    value: Weak<Value>,
}

/// Encode only inside this catalog's private output directory; these numbers
/// alone confer no font-reading or drawing authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FontKey {
    pub slot: u16,
    pub generation: u64,
}

impl Default for MarkdownLineFontCatalog {
    /// Create the same empty fixed catalog as explicit construction.
    fn default() -> Self {
        Self::new()
    }
}
impl MarkdownLineFontCatalog {
    /// Allocate only the fixed weak slots; no font or native resource is acquired.
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

    /// Reuse only the same live font payload. Equal PostScript names or bytes
    /// from a separate resolve are not the same registration identity.
    pub fn register(&self, font: &MarkdownLayoutFont) -> Result<MarkdownLineRegisteredFont> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| Error::FontCatalogUnavailable)?;
        for (ordinal, slot) in state.slots.iter().enumerate() {
            // A live identical payload may share its actual registered owner;
            // neither weak-slot coordinates nor font names prove this identity.
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
        }
        // Only the fixed 64 slots are scanned. Registration copies no font data,
        // computes no whole-font hash, and performs no native operation.
        let (ordinal, slot) = state
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.value.upgrade().is_none())
            .ok_or(Error::FontBudgetExceeded)?;
        // Advance generation before reuse so an older weak ticket cannot acquire
        // a different font merely because it occupied the same numeric slot.
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

    /// Interpret a private directory's slot/generation only through this same
    /// catalog and the actual still-live value for that generation.
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
            // Reusing a slot must not redirect old glyph output to a different font.
            return Err(Error::FontExpired);
        }
        let value = slot.value.upgrade().ok_or(Error::FontExpired)?;
        Ok(MarkdownLineRegisteredFont {
            catalog: Arc::downgrade(&self.0),
            key,
            value,
        })
    }

    /// Require both the original catalog and exact immutable registered value;
    /// a matching slot/generation alone cannot authorize a foreign registration.
    pub fn require_registration(
        &self,
        registration: &MarkdownLineFontRegistration,
    ) -> Result<MarkdownLineRegisteredFont> {
        if !Weak::ptr_eq(&registration.catalog, &Arc::downgrade(&self.0)) {
            // A different catalog has independent slot meanings and ownership.
            return Err(Error::FontExpired);
        }
        let registered = self.resolve(registration.key())?;
        if !Weak::ptr_eq(&registration.value, &Arc::downgrade(&registered.value)) {
            // Retain exact payload identity after resolving the original slot generation.
            return Err(Error::FontExpired);
        }
        Ok(registered)
    }
}

impl MarkdownLineRegisteredFont {
    /// Borrow the actual Core font while this registration keeps its payload alive.
    pub fn font(&self) -> &MarkdownLayoutFont {
        &self.value.font
    }
    /// Lend weak scheduling authority without extending this registered font's lifetime.
    pub fn registration(&self) -> MarkdownLineFontRegistration {
        MarkdownLineFontRegistration {
            catalog: self.catalog.clone(),
            key: self.key,
            value: Arc::downgrade(&self.value),
        }
    }
}
impl MarkdownLineFontRegistration {
    /// Upgrade only through the same catalog, slot generation, and immutable
    /// registered value; there is no name-based revival fallback.
    pub fn lease(&self) -> Result<MarkdownLineRegisteredFont> {
        let catalog = self.catalog.upgrade().ok_or(Error::FontExpired)?;
        MarkdownLineFontCatalog(catalog).require_registration(self)
    }
    /// Derive font authority only from a successfully leased current registration.
    pub fn authority(&self) -> Result<MarkdownLayoutFontAuthority> {
        Ok(self.lease()?.font().authority())
    }
    /// Encode this ticket's private directory key without granting a font lease.
    pub(super) fn key(&self) -> FontKey {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve the real bundled finite test font through the production font loader.
    fn font() -> MarkdownLayoutFont {
        let bytes: &'static [u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
        ));
        MarkdownLayoutFont::resolve(Arc::new(bytes), "Roboto-Regular", &[]).unwrap()
    }
    #[test]
    fn weak_slots_reuse_same_payload_but_never_reauthorize_an_expired_generation() {
        // Real registrations for the same payload may coexist, but neither the
        // catalog nor an escaped weak ticket may keep that payload alive afterward.
        let catalog = MarkdownLineFontCatalog::new();
        let actual = font();
        let first = catalog.register(&actual).unwrap();
        let token = first.registration();
        let duplicate = catalog.register(&actual).unwrap();
        let original_observer = actual.resource_observer();
        drop(duplicate);
        drop(first);
        drop(actual);
        // The original font is actually destroyed although catalog/token remain.
        assert!(original_observer.payload_released());
        assert!(matches!(token.lease(), Err(Error::FontExpired)));
        let next = catalog.register(&font()).unwrap();
        // A later real registration cannot revive the earlier expired ticket.
        assert!(next.registration().lease().is_ok());
        assert!(matches!(token.lease(), Err(Error::FontExpired)));
    }
    #[test]
    fn catalog_is_fixed_and_foreign_catalog_cannot_sign_font_permission() {
        let catalog = MarkdownLineFontCatalog::new();
        // These finite test owners retain 64 distinct real fonts; the product
        // catalog itself has no strong-font vector or growing payload inventory.
        let mut keep = Vec::new();
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
    }
}
