//! Readable UTF-8 JSON working files with strict schema and size validation.
use bnauth_core::VaultSnapshot;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fmt,
    io::{self, Write},
};
#[cfg(test)]
use std::{fs::File, io::Read, path::Path};
use zeroize::Zeroizing;

pub const MAX_FILE: usize = 32 * 1024 * 1024;

#[cfg(all(test, windows))]
std::thread_local! {
    static JSON_DECODE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, windows))]
pub(crate) fn json_decode_calls() -> usize {
    JSON_DECODE_CALLS.with(std::cell::Cell::get)
}

struct LimitedBuffer(Zeroizing<Vec<u8>>);
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_FILE {
            return Err(io::Error::other("資料大小超過 32 MiB 上限。"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn encode(snapshot: &VaultSnapshot) -> Result<Zeroizing<Vec<u8>>, String> {
    snapshot.validate()?;
    encode_validated(snapshot)
}

/// Internal serialization after the repository has validated the full candidate.
/// Advancing its revision and timestamp preserves that validation; file-size
/// limits still apply here, and installed bytes are independently read back.
pub(crate) fn encode_validated(snapshot: &VaultSnapshot) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut body = LimitedBuffer(Zeroizing::new(Vec::new()));
    serde_json::to_writer_pretty(&mut body, snapshot)
        .map_err(|_| "無法準備要保存的資料，或資料大小超過 32 MiB 上限。".to_owned())?;
    body.write_all(b"\n")
        .map_err(|_| "資料大小超過 32 MiB 上限。".to_owned())?;
    Ok(body.0)
}

pub fn decode(bytes: &[u8]) -> Result<VaultSnapshot, String> {
    if bytes.len() > MAX_FILE {
        return Err("資料檔超過 32 MiB 上限。".into());
    }
    // Editors may add a UTF-8 BOM or change line endings. Neither changes the DTO.
    let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    decode_json(body)
}

fn decode_json(body: &[u8]) -> Result<VaultSnapshot, String> {
    #[cfg(all(test, windows))]
    JSON_DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let mut parser = serde_json::Deserializer::from_slice(body);
    CheckJson { depth: 0 }
        .deserialize(&mut parser)
        .and_then(|_| parser.end())
        .map_err(|_| "資料檔內容有重複欄位、層次或項目過多，或格式錯誤。".to_owned())?;
    let snapshot: VaultSnapshot = serde_json::from_slice(body)
        .map_err(|_| "資料檔的格式或部分內容不受支援，無法讀取；原檔不會被覆寫。".to_owned())?;
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg(test)]
pub(crate) fn read(path: &Path) -> Result<(VaultSnapshot, [u8; 32]), String> {
    let file = File::open(path).map_err(|e| format!("無法讀取資料檔（{}）。", e))?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("讀取資料檔失敗（{}）。", e))?;
    let snapshot = decode(&bytes)?;
    Ok((snapshot, Sha256::digest(&*bytes).into()))
}

// This pass checks bounds while parsing, before constructing collections in the DTO.
struct CheckJson {
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for CheckJson {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for CheckJson {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<(), E> {
        if value.len() > 64 * 1024 {
            Err(E::custom("string exceeds64KiB"))
        } else {
            Ok(())
        }
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<(), E> {
        self.visit_str(&value)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.depth >= 32 {
            return Err(de::Error::custom("JSON depth exceeds32"));
        }
        let mut count = 0;
        while seq
            .next_element_seed(CheckJson {
                depth: self.depth + 1,
            })?
            .is_some()
        {
            count += 1;
            if count > 1000 {
                return Err(de::Error::custom("array exceeds1000"));
            }
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        if self.depth >= 32 {
            return Err(de::Error::custom("JSON depth exceeds32"));
        }
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > 256 || keys.len() >= 128 || !keys.insert(key) {
                return Err(de::Error::custom("duplicate or excessive object key"));
            }
            map.next_value_seed(CheckJson {
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnauth_core::{
        Group, LegacyMetadata, Provenance, RecoveryEvent, TimeSyncRecord, Uuid, parse_manual,
        prepare_import,
    };

    fn full_snapshot() -> VaultSnapshot {
        const NOW: i64 = 1_800_000_000_000;
        let mut vault = VaultSnapshot::new_empty(NOW);
        let group = Uuid::new_v4();
        vault.groups.push(Group {
            id: group,
            name: "測試分組".into(),
            sort_order: 1,
            is_ungrouped: false,
        });
        // Public RFC 6238 test seed; this fixture never contains account credentials.
        let material = parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AB12CD34EF",
        )
        .unwrap();
        vault = prepare_import(
            &vault,
            "人工驗證器",
            "  中文 / Кириллица / 한글\n註記\t  ",
            group,
            material,
            NOW + 10,
        )
        .unwrap()
        .candidate;
        vault.revision = 7;
        vault.updated_at_ms = NOW + 20;
        let entry = &mut vault.authenticators[0];
        entry.recovery_history.push(RecoveryEvent {
            at_ms: NOW + 15,
            operation: "fixture".into(),
            outcome: "retained".into(),
            confirmation: Some("人工紀錄".into()),
        });
        entry.legacy_metadata.push(LegacyMetadata {
            source: "fixture".into(),
            source_version: Some("1".into()),
            field: "original_text".into(),
            original_value: "  保留原文\n  ".into(),
            unit: Some("text".into()),
        });
        let mut no_secret = entry.clone();
        no_secret.id = Uuid::new_v4();
        no_secret.group_id = vault.groups[0].id;
        no_secret.name = "無密鑰紀錄".into();
        no_secret.secret = None;
        no_secret.serial = None;
        no_secret.restore_code = None;
        no_secret.restore_code_source = None;
        no_secret.recovery_history.clear();
        no_secret.legacy_metadata.clear();
        vault.authenticators.push(no_secret);
        let sync = TimeSyncRecord {
            source: bnauth_core::TIME_SOURCE.into(),
            server_time_ms: NOW + 42,
            local_wall_ms_at_sample: NOW,
            offset_ms: 42,
            rtt_ms: 100,
        };
        vault.service_time_sync.battlenet = Some(sync.clone());
        vault.provenance.push(Provenance {
            source_vault_id: Uuid::new_v4(),
            source_revision: 3,
            scope: "full".into(),
            imported_at_ms: NOW,
            source_time_sync: Some(sync),
        });
        vault.validate().unwrap();
        vault
    }

    #[test]
    fn readable_json_preserves_complete_snapshot_and_unicode() {
        let vault = full_snapshot();
        let bytes = encode(&vault).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("{\n  \"schema_version\": 1,"));
        assert!(text.ends_with("}\n"));
        assert!(text.contains("人工驗證器"));
        assert!(text.contains("未分組"));
        assert!(!text.chars().any(|c| c == '\0'));
        assert_eq!(
            serde_json::from_slice::<VaultSnapshot>(&bytes).unwrap(),
            vault
        );
        assert_eq!(decode(&bytes).unwrap(), vault);
    }

    #[test]
    fn editor_bom_crlf_whitespace_and_name_edit_preserve_other_data() {
        let vault = full_snapshot();
        let encoded = encode(&vault).unwrap();
        let text = std::str::from_utf8(&encoded).unwrap();
        let edited = format!(
            "\u{feff} \r\n{}\r\n\t",
            text.replace("人工驗證器", "文字編輯器改名")
                .replace('\n', "\r\n")
        );
        let mut expected = vault.clone();
        expected.authenticators[0].name = "文字編輯器改名".into();
        assert_eq!(decode(edited.as_bytes()).unwrap(), expected);
        assert_eq!(
            bnauth_core::totp(expected.authenticators[0].secret.as_ref().unwrap(), 59_000).unwrap(),
            "94287082"
        );
    }

    #[test]
    fn unknown_duplicate_schema_scope_and_malformed_json_are_rejected() {
        let mut partial = full_snapshot();
        partial.scope = "group".into();
        assert!(encode(&partial).is_err());
        let json = serde_json::to_string(&full_snapshot()).unwrap();
        let duplicate = json.replacen('{', "{\"schema_version\":1,", 1);
        let nested_duplicate = json.replacen(
            "\"name\":\"人工驗證器\"",
            "\"name\":\"重複\",\"name\":\"人工驗證器\"",
            1,
        );
        let unknown = json.replacen('{', "{\"unrecognized\":true,", 1);
        let schema = json.replace("\"schema_version\":1", "\"schema_version\":2");
        let scope = json.replace("\"scope\":\"full\"", "\"scope\":\"group\"");
        let truncated = json[..json.len() - 1].to_owned();
        let trailing = json.clone() + " false";
        let excessive = format!("[{}]", vec!["0"; 1001].join(","));
        for body in [
            duplicate,
            nested_duplicate,
            unknown,
            schema,
            scope,
            truncated,
            trailing,
            excessive,
            "[".repeat(34) + &"]".repeat(34),
        ] {
            assert!(decode(body.as_bytes()).is_err());
        }
        assert!(decode(&[0xff, 0xfe]).is_err());
        assert!(decode(&[]).is_err());
        assert!(decode(b" \r\n\t").is_err());
    }

    #[test]
    fn json_file_size_limit_is_enforced() {
        let vault = VaultSnapshot::new_empty(1_800_000_000_000);
        let mut body = serde_json::to_vec(&vault).unwrap();
        body.resize(MAX_FILE, b' ');
        assert_eq!(decode(&body).unwrap(), vault);
        body.push(b' ');
        assert!(decode(&body).unwrap_err().contains("32 MiB"));
    }

    #[test]
    fn writer_size_and_parser_depth_limits_are_enforced() {
        let mut writer = LimitedBuffer(Zeroizing::new(Vec::new()));
        writer.write_all(&vec![b' '; MAX_FILE]).unwrap();
        assert_eq!(writer.0.len(), MAX_FILE);
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(writer.0.len(), MAX_FILE);
        for (depth, expected) in [(32, true), (33, false)] {
            let json = "[".repeat(depth) + "0" + &"]".repeat(depth);
            let mut parser = serde_json::Deserializer::from_str(&json);
            assert_eq!(
                CheckJson { depth: 0 }.deserialize(&mut parser).is_ok(),
                expected
            );
        }
    }
}
