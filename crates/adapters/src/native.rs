//! Readable UTF-8 JSON working files with strict schema and size validation.
use crate::operation_io_error;
use bnauth_core::{
    AuthenticatorFields, AuthenticatorRecord, ErrorCode, Group, GroupFields, ServiceTimeSync, Uuid,
    ValidatedBackup, VaultFile, VaultSnapshot,
};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    fmt,
    io::{self, Write},
};
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

#[derive(Default)]
struct LimitedWriter {
    bytes: Zeroizing<Vec<u8>>,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FILE - self.bytes.len() {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn encode(snapshot: &VaultSnapshot) -> Result<Zeroizing<Vec<u8>>, String> {
    snapshot.validate()?;
    encode_validated(&snapshot.file_view())
}

/// Encode a committed full vault or its validated group export after replacing
/// only the shared clock. Use `encode` or `encode_group` for unvalidated input.
pub fn encode_backup_from_validated(
    snapshot: &VaultFile<'_>,
) -> Result<Zeroizing<Vec<u8>>, String> {
    snapshot.service_time_sync.validate()?;
    encode_validated(snapshot)
}

pub fn encode_group(snapshot: &VaultSnapshot) -> Result<Zeroizing<Vec<u8>>, String> {
    snapshot.validate_group_export()?;
    encode_validated(&snapshot.file_view())
}

/// Serialize validated data with the native file-size limit.
pub(crate) fn encode_validated(snapshot: &VaultFile<'_>) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut body = LimitedWriter::default();
    let encode_error = |kind| {
        let code = if kind == Some(io::ErrorKind::FileTooLarge) {
            ErrorCode::FileTooLarge
        } else {
            ErrorCode::WriteFailed
        };
        code.notice().to_string()
    };
    serde_json::to_writer_pretty(&mut body, snapshot)
        .map_err(|error| encode_error(error.io_error_kind()))?;
    body.write_all(b"\n")
        .map_err(|error| encode_error(Some(error.kind())))?;
    Ok(body.bytes)
}

pub fn decode(bytes: &[u8]) -> Result<VaultSnapshot, String> {
    decode_scoped(bytes, false).map(ValidatedBackup::into_snapshot)
}

/// Inspect a group export without allowing it to be loaded as a working vault.
pub fn decode_group(bytes: &[u8]) -> Result<VaultSnapshot, String> {
    decode_scoped(bytes, true).map(ValidatedBackup::into_snapshot)
}

pub fn read_group(path: &Path) -> Result<VaultSnapshot, String> {
    read_group_for_import(path).map(ValidatedBackup::into_snapshot)
}

/// Read a full backup without accepting a partial group export as a database.
pub fn read_full(path: &Path) -> Result<VaultSnapshot, String> {
    read_full_for_restore(path).map(ValidatedBackup::into_snapshot)
}

pub fn read_group_for_import(path: &Path) -> Result<ValidatedBackup, String> {
    decode_scoped(&read_bytes(path)?, true)
}

pub fn read_full_for_restore(path: &Path) -> Result<ValidatedBackup, String> {
    decode_scoped(&read_bytes(path)?, false)
}

fn read_bytes(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let report = |error: io::Error| {
        operation_io_error(&error, ErrorCode::ReadFailed)
            .notice()
            .to_string()
    };
    let file = File::open(path).map_err(report)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(report)?;
    Ok(bytes)
}

fn decode_scoped(bytes: &[u8], group: bool) -> Result<ValidatedBackup, String> {
    let snapshot = serde_json::from_slice(json_body(bytes)?)
        .map_err(|_| ErrorCode::CorruptFile.notice().to_string())?;
    ValidatedBackup::new(snapshot, group)
}

#[derive(Deserialize)]
struct FieldText<'a>(#[serde(borrow)] Cow<'a, str>);

fn json_body(bytes: &[u8]) -> Result<&[u8], String> {
    if bytes.len() > MAX_FILE {
        return Err(ErrorCode::FileTooLarge.notice().to_string());
    }
    // Editors may add a UTF-8 BOM or change line endings. Neither changes the DTO.
    #[cfg(all(test, windows))]
    JSON_DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
    Ok(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
}

/// Read back the file written by our encoder, comparing each decoded row with
/// the already validated candidate while retaining only one row at a time.
pub(crate) fn verify_written(
    bytes: &[u8],
    candidate: &VaultSnapshot,
    revision: u64,
) -> Result<(), String> {
    let failure = || ErrorCode::WriteFailed.notice().to_string();
    let mut parser = serde_json::Deserializer::from_slice(json_body(bytes)?);
    parser
        .deserialize_map(WrittenVault {
            candidate,
            revision,
        })
        .and_then(|_| parser.end())
        .map_err(|_| failure())
}

struct WrittenVault<'a> {
    candidate: &'a VaultSnapshot,
    revision: u64,
}

impl<'de> Visitor<'de> for WrittenVault<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the saved full vault")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let expected = self.candidate;
        let mut seen = [false; 7];
        while let Some(key) = map.next_key::<&str>()? {
            let (field, matches) = match key {
                "schema_version" => (0, map.next_value::<u32>()? == expected.schema_version),
                "scope" => (1, map.next_value::<&str>()? == expected.scope),
                "vault_id" => (2, map.next_value::<Uuid>()? == expected.vault_id),
                "revision" => (3, map.next_value::<u64>()? == self.revision),
                "groups" => (
                    4,
                    map.next_value_seed(MatchingRows(expected.groups.iter()))?,
                ),
                "authenticators" => (
                    5,
                    map.next_value_seed(MatchingRows(
                        expected.authenticators.iter().map(|record| &**record),
                    ))?,
                ),
                "service_time_sync" => (
                    6,
                    map.next_value::<ServiceTimeSync>()? == expected.service_time_sync,
                ),
                _ => return Err(de::Error::custom("unexpected saved field")),
            };
            if seen[field] || !matches {
                return Err(de::Error::custom("saved field differs"));
            }
            seen[field] = true;
        }
        if seen.contains(&false) {
            return Err(de::Error::custom("missing saved field"));
        }
        Ok(())
    }
}

struct MatchingRows<I>(I);

// The normal row types borrow unescaped text directly from the readback buffer.

trait WrittenRow {
    fn matches_next<'de, A: SeqAccess<'de>>(&self, seq: &mut A) -> Result<bool, A::Error>;
}

impl WrittenRow for Group {
    fn matches_next<'de, A: SeqAccess<'de>>(&self, seq: &mut A) -> Result<bool, A::Error> {
        Ok(seq
            .next_element::<GroupFields<FieldText>>()?
            .is_some_and(|row| {
                row.id == self.id
                    && row.name.0 == self.name
                    && row.sort_order == self.sort_order
                    && row.is_ungrouped == self.is_ungrouped
            }))
    }
}

impl WrittenRow for AuthenticatorRecord {
    fn matches_next<'de, A: SeqAccess<'de>>(&self, seq: &mut A) -> Result<bool, A::Error> {
        Ok(seq
            .next_element::<AuthenticatorFields<FieldText>>()?
            .is_some_and(|row| {
                row.id == self.id
                    && row.group_id == self.group_id
                    && row.sort_order == self.sort_order
                    && row.name.0 == self.name
                    && row.description.0 == self.description
                    && row.secret == self.secret
                    && row.serial.as_ref().map(|text| text.0.as_ref()) == self.serial.as_deref()
                    && row.restore_code.as_ref().map(|text| text.0.as_ref())
                        == self.restore_code.as_deref()
                    && row.digits == self.digits
                    && row.algorithm.0 == self.algorithm
                    && row.period_secs == self.period_secs
            }))
    }
}

impl<'de, I> DeserializeSeed<'de> for MatchingRows<I>
where
    Self: Visitor<'de, Value = bool>,
{
    type Value = bool;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'a, 'de, T, I> Visitor<'de> for MatchingRows<I>
where
    T: 'a + WrittenRow,
    I: Iterator<Item = &'a T>,
{
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the saved rows in order")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
        for expected in self.0 {
            if !expected.matches_next(&mut seq)? {
                return Ok(false);
            }
        }
        Ok(seq.next_element::<de::IgnoredAny>()?.is_none())
    }
}

#[cfg(test)]
pub(crate) fn read(path: &Path) -> Result<(VaultSnapshot, [u8; 32]), String> {
    let bytes = read_bytes(path)?;
    let snapshot = decode(&bytes)?;
    Ok((snapshot, Sha256::digest(&*bytes).into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnauth_core::{Group, TimeSyncRecord, Uuid, parse_manual, prepare_import};

    fn full_snapshot() -> VaultSnapshot {
        const NOW: i64 = 1_800_000_000_000;
        let mut vault = VaultSnapshot::new_empty();
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
        )
        .unwrap()
        .candidate(&vault)
        .unwrap();
        vault.revision = 7;
        let entry = &mut vault.authenticators[0];
        let mut copy = entry.clone();
        copy.id = Uuid::new_v4();
        copy.group_id = vault.groups[0].id;
        copy.name = "純密鑰紀錄".into();
        copy.serial = None;
        copy.restore_code = None;
        vault.authenticators.push(copy);
        let sync = TimeSyncRecord {
            source: bnauth_core::TIME_SOURCE.into(),
            server_time_ms: NOW + 42,
            local_wall_ms_at_sample: NOW,
            offset_ms: 42,
            rtt_ms: 100,
        };
        vault.service_time_sync.battlenet = Some(sync);
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
        assert!(text.contains("預設"));
        assert!(!text.chars().any(|c| c == '\0'));
        assert_eq!(
            serde_json::from_slice::<VaultSnapshot>(&bytes).unwrap(),
            vault
        );
        assert_eq!(decode(&bytes).unwrap(), vault);
        verify_written(&bytes, &vault, vault.revision).unwrap();
        assert!(verify_written(&bytes, &vault, vault.revision + 1).is_err());
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
        assert!(verify_written(edited.as_bytes(), &vault, vault.revision).is_err());
        assert_eq!(
            bnauth_core::totp(&expected.authenticators[0].secret, 59_000).unwrap(),
            "94287082"
        );
    }

    #[test]
    fn unknown_duplicate_schema_scope_and_malformed_json_are_rejected() {
        let mut partial = full_snapshot();
        partial.scope = "group".into();
        assert!(encode(&partial).is_err());
        let vault = full_snapshot();
        let json = serde_json::to_string(&vault).unwrap();
        assert_eq!(
            decode_group(json.as_bytes()).unwrap_err(),
            ErrorCode::FullBackupSelected.notice().to_string()
        );
        // The scope can appear after the records in JSON saved by a text editor.
        let reordered = json.replacen("\"scope\":\"full\",", "", 1);
        let reordered = format!("{},\"scope\":\"full\"}}", &reordered[..reordered.len() - 1]);
        assert_eq!(decode(reordered.as_bytes()).unwrap(), vault);
        assert_eq!(
            decode_group(reordered.as_bytes()).unwrap_err(),
            ErrorCode::FullBackupSelected.notice().to_string()
        );
        let duplicate = json.replacen('{', "{\"schema_version\":1,", 1);
        let nested_duplicate = json.replacen(
            "\"name\":\"人工驗證器\"",
            "\"name\":\"重複\",\"name\":\"人工驗證器\"",
            1,
        );
        let unknown = json.replacen('{', "{\"unrecognized\":true,", 1);
        let unknown_entry = json.replacen("\"digits\":8", "\"unrecognized\":true,\"digits\":8", 1);
        let schema = json.replace("\"schema_version\":1", "\"schema_version\":2");
        let scope = json.replace("\"scope\":\"full\"", "\"scope\":\"group\"");
        let truncated = json[..json.len() - 1].to_owned();
        let trailing = json.clone() + " false";
        let excessive = format!("[{}]", vec!["0"; 1001].join(","));
        for body in [
            duplicate,
            nested_duplicate,
            unknown,
            unknown_entry,
            schema,
            scope,
            truncated,
            trailing,
            excessive,
            "[".repeat(34) + &"]".repeat(34),
        ] {
            assert!(decode(body.as_bytes()).is_err());
            assert!(verify_written(body.as_bytes(), &vault, vault.revision).is_err());
        }
        assert!(decode(&[0xff, 0xfe]).is_err());
        assert_eq!(
            decode(&[]).unwrap_err(),
            ErrorCode::CorruptFile.notice().to_string()
        );
        assert!(decode(b" \r\n\t").is_err());
    }

    #[test]
    fn json_file_size_limit_is_enforced() {
        let vault = VaultSnapshot::new_empty();
        let mut body = serde_json::to_vec(&vault).unwrap();
        body.resize(MAX_FILE, b' ');
        assert_eq!(decode(&body).unwrap(), vault);
        body.push(b' ');
        assert_eq!(
            decode(&body).unwrap_err(),
            "E15：檔案過大\n超過 32 MiB 上限。"
        );
    }

    #[test]
    fn writer_size_and_parser_depth_limits_are_enforced() {
        let mut writer = LimitedWriter::default();
        writer.write_all(&vec![b' '; MAX_FILE]).unwrap();
        assert_eq!(writer.bytes.len(), MAX_FILE);
        assert_eq!(
            writer.write_all(b"x").unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(writer.bytes.len(), MAX_FILE);
        let vault = serde_json::to_string(&VaultSnapshot::new_empty()).unwrap();
        for (depth, expected) in [(31, true), (32, false)] {
            let value = "[".repeat(depth) + "0" + &"]".repeat(depth);
            let json = vault.replacen('{', &format!("{{\"created_at_ms\":{value},"), 1);
            assert_eq!(decode(json.as_bytes()).is_ok(), expected);
        }
    }
}
