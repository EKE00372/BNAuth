use bnauth_adapters::{
    export_file::{self, Format},
    winauth2,
};
use bnauth_core::{VaultSnapshot, parse_manual, prepare_import};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-xml-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove only the flat directory created by this test.
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn exports_unicode_file_replaces_existing_xml_and_never_overwrites_native_data() {
    let dir = Fixture::new();
    let vault = VaultSnapshot::new_empty(1_800_000_000_000);
    let vault = prepare_import(
        &vault,
        "測試 & 測試",
        "完整備註",
        vault.groups[0].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AB12CD34EF",
        )
        .unwrap(),
        1_800_000_000_000,
    )
    .unwrap()
    .candidate;
    let original = vault.clone();
    let xml = winauth2::encode(&vault.authenticators[0], None).unwrap();
    let path = dir.0.join("人工 匯出.XML");
    export_file::write(&path, xml.as_bytes(), Format::WinAuth2Xml, None).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), xml.as_str());
    fs::write(&path, b"old export").unwrap();
    export_file::write(&path, xml.as_bytes(), Format::WinAuth2Xml, None).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), xml.as_str());
    let native = dir.0.join("BNAuth.bnauth");
    fs::write(&native, b"original working data").unwrap();
    assert!(export_file::write(&native, xml.as_bytes(), Format::WinAuth2Xml, None).is_err());
    assert_eq!(fs::read(&native).unwrap(), b"original working data");
    assert_eq!(vault, original);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[cfg(windows)]
#[test]
fn locked_destination_fails_without_damaging_it_or_leaving_plaintext_temporary_files() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    let dir = Fixture::new();
    let path = dir.0.join("locked.xml");
    fs::write(&path, b"previous export").unwrap();
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    assert!(export_file::write(&path, b"<WinAuth />", Format::WinAuth2Xml, None).is_err());
    drop(locked);
    assert_eq!(fs::read(&path).unwrap(), b"previous export");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}
