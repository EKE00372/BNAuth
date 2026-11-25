use hmac::{Hmac, Mac};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha1::Sha1;
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

/// A fixed Battle.net seed. Debug never reveals bytes, and owned memory is cleared on drop.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Secret([u8; 20]);

impl Secret {
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(text: &str) -> Result<Self, String> {
        if text.len() != 40 || !text.is_ascii() {
            return Err("密鑰必須為40位十六進位字串。".into());
        }
        let mut bytes = Zeroizing::new([0_u8; 20]);
        for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            bytes[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(*bytes))
    }

    pub fn bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Explicitly exposes hex for the user; never use in logging.
    pub fn hex(&self) -> String {
        self.with_hex(str::to_owned)
    }

    fn with_hex<T>(&self, use_hex: impl FnOnce(&str) -> T) -> T {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut value = Zeroizing::new([0_u8; 40]);
        for (pair, &byte) in value.chunks_exact_mut(2).zip(&self.0) {
            pair[0] = DIGITS[(byte >> 4) as usize];
            pair[1] = DIGITS[(byte & 15) as usize];
        }
        use_hex(std::str::from_utf8(&value[..]).expect("hex digits are ASCII"))
    }
}

pub(crate) fn nibble(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("密鑰含非十六進位字元。".into()),
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.with_hex(|value| serializer.serialize_str(value))
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SecretVisitor;
        impl<'de> de::Visitor<'de> for SecretVisitor {
            type Value = Secret;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("驗證器必須有40位十六進位密鑰")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Secret, E> {
                Secret::from_hex(value).map_err(E::custom)
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Secret, E> {
                let value = Zeroizing::new(value);
                self.visit_str(&value)
            }
        }
        deserializer.deserialize_str(SecretVisitor)
    }
}

/// RFC6238 using the Battle.net profile: HMAC-SHA1, 30 seconds, eight digits, T0=0.
/// The time input is UTC Unix milliseconds from the single shared application clock.
pub fn totp(secret: &Secret, unix_ms: i64) -> Result<String, String> {
    let unix_ms = u64::try_from(unix_ms).map_err(|_| "系統時間早於Unix epoch，無法產碼。")?;
    let counter = unix_ms / 30_000;
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.bytes()).map_err(|_| "無法初始化產碼。")?;
    mac.update(&counter.to_be_bytes());
    let mut digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let value = u32::from_be_bytes(
        digest[offset..offset + 4]
            .try_into()
            .expect("SHA1 truncation range"),
    ) & 0x7fff_ffff;
    digest.as_mut_slice().zeroize();
    Ok(format!("{:08}", value % 100_000_000))
}
