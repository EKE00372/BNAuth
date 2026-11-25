use crate::{LegacyField, MAX_ENTRIES, Shared};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::{collections::HashSet, fmt, marker::PhantomData};

/// Stop an oversized array while decoding, without building its remaining records.
pub(super) fn rows<'de, D, T>(d: D) -> Result<Shared<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Rows<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Rows<T> {
        type Value = Shared<Vec<T>>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a native record array")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut rows = Vec::new();
            while rows.len() < MAX_ENTRIES {
                let Some(row) = seq.next_element()? else {
                    return Ok(rows.into());
                };
                rows.push(row);
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("too many records"));
            }
            Ok(rows.into())
        }
    }
    d.deserialize_seq(Rows(PhantomData))
}

// These obsolete scalar fields formerly accepted arbitrary bounded JSON. The
// history fields are independently ignored, as in earlier versions.
pub(super) fn vault_legacy<'de, D: Deserializer<'de>>(d: D) -> Result<LegacyField, D::Error> {
    LegacyValue(1).deserialize(d)
}

pub(super) fn row_legacy<'de, D: Deserializer<'de>>(d: D) -> Result<LegacyField, D::Error> {
    LegacyValue(3).deserialize(d)
}

struct LegacyValue(usize);

impl<'de> DeserializeSeed<'de> for LegacyValue {
    type Value = LegacyField;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for LegacyValue {
    type Value = LegacyField;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a bounded legacy field")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<LegacyField, E> {
        Ok(LegacyField)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<LegacyField, E> {
        Ok(LegacyField)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<LegacyField, E> {
        Ok(LegacyField)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<LegacyField, E> {
        Ok(LegacyField)
    }
    fn visit_unit<E: de::Error>(self) -> Result<LegacyField, E> {
        Ok(LegacyField)
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<LegacyField, E> {
        if value.len() > 64 * 1024 {
            return Err(E::custom("legacy string too long"));
        }
        Ok(LegacyField)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LegacyField, A::Error> {
        if self.0 >= 32 {
            return Err(de::Error::custom("legacy field nested too deeply"));
        }
        let mut count = 0;
        while seq.next_element_seed(LegacyValue(self.0 + 1))?.is_some() {
            count += 1;
            if count > 1000 {
                return Err(de::Error::custom("legacy array too long"));
            }
        }
        Ok(LegacyField)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LegacyField, A::Error> {
        if self.0 >= 32 {
            return Err(de::Error::custom("legacy field nested too deeply"));
        }
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            let history =
                self.0 == 2 && matches!(key.as_str(), "recovery_history" | "legacy_metadata");
            if key.len() > 256 || keys.len() >= 128 || !keys.insert(key) {
                return Err(de::Error::custom(
                    "duplicate or excessive legacy object key",
                ));
            }
            if history {
                map.next_value::<de::IgnoredAny>()?;
            } else {
                map.next_value_seed(LegacyValue(self.0 + 1))?;
            }
        }
        Ok(LegacyField)
    }
}
