//! A bound on what a document's aliases can expand to.
//!
//! An alias replays the node its anchor names, so a short document can stand
//! for a value many times its size. The document is walked once first,
//! borrowing every scalar, and refused if the value it describes would cost
//! more than the budget. Only then is the value built.

use std::cell::Cell;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

/// What a node costs besides its text, close to the memory a deserialized
/// value takes per node.
const NODE_COST: usize = 32;

/// The value may cost this many times the document's length.
const EXPANSION_FACTOR: usize = 16;

/// Allowance on top, so a short document is not held to a tiny budget.
const EXPANSION_FLOOR: usize = 1 << 20;

/// What a document of `len` bytes may expand to.
pub(super) fn budget(len: usize) -> usize {
    len.saturating_mul(EXPANSION_FACTOR)
        .saturating_add(EXPANSION_FLOOR)
}

/// Walks a value without building it, charging each node to `remaining`.
#[derive(Clone, Copy)]
pub(super) struct Measure<'a> {
    pub(super) remaining: &'a Cell<usize>,
}

impl Measure<'_> {
    fn charge<E: de::Error>(self, text: usize) -> Result<(), E> {
        let cost = NODE_COST.saturating_add(text);
        match self.remaining.get().checked_sub(cost) {
            Some(left) => {
                self.remaining.set(left);
                Ok(())
            }
            None => Err(E::custom(
                "the document expands to a value too large for its size: its aliases \
                 repeat too much",
            )),
        }
    }
}

impl<'de> DeserializeSeed<'de> for Measure<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Measure<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        self.charge(v.len())
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<(), E> {
        self.charge(v.len())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        self.charge(0)
    }

    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_newtype_struct<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<(), D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        self.charge(0)?;
        while seq.next_element_seed(self)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.charge(0)?;
        while map.next_key_seed(self)?.is_some() {
            map.next_value_seed(self)?;
        }
        Ok(())
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, data: A) -> Result<(), A::Error> {
        let ((), variant) = data.variant_seed(self)?;
        de::VariantAccess::newtype_variant_seed(variant, self)
    }
}
