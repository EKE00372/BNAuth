use super::*;
use bnauth_core::{VaultSnapshot, parse_manual, prepare_import};

fn record() -> Authenticator {
    let vault = VaultSnapshot::new_empty(1_800_000_000_000);
    prepare_import(
        &vault,
        " 繁體 & <title> \"' 😀 ",
        "備註\r\n第二行\t<&>\"' -- ]]> ",
        vault.groups[0].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
        1_800_000_000_000,
    )
    .unwrap()
    .candidate
    .authenticators
    .remove(0)
}

#[test]
fn exports_original_secretdata_and_separate_escaped_metadata_without_deriving_restore_code() {
    let record = record();
    let xml = encode(&record, Some(-1234)).unwrap();
    let packed = format!(
        "3132333435363738393031323334353637383930{}",
        b"US-1234-5678-9012"
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>()
    );
    assert!(xml.contains(&format!("<secretdata encrypted=\"\">{packed}</secretdata>")));
    assert!(xml.contains("<servertimediff>-1234</servertimediff>"));
    assert!(xml.contains(&format!("xmlns:bnauth=\"{METADATA_NAMESPACE}\"")));
    assert!(
        xml.contains("<bnauth:title> 繁體 &amp; &lt;title&gt; &quot;&apos; 😀 </bnauth:title>")
    );
    assert!(xml.contains(
        "<bnauth:note>備註&#13;&#10;第二行&#9;&lt;&amp;&gt;&quot;&apos; -- ]]&gt; </bnauth:note>"
    ));
    assert!(xml.contains("<bnauth:serial>US-1234-5678-9012</bnauth:serial>"));
    assert!(xml.contains("<bnauth:restoreCode>AbCd123456</bnauth:restoreCode>"));
    assert!(!xml.contains("restorecodeverified"));
    assert_eq!(xml.matches("<authenticator ").count(), 1);
}

#[test]
fn missing_serial_and_restore_code_stay_empty_with_a_local_clock_compatibility_offset() {
    let mut record = record();
    record.serial = None;
    record.restore_code = None;
    record.restore_code_source = None;
    let xml = encode(&record, None).unwrap();
    assert!(xml.contains(
        "<secretdata encrypted=\"\">3132333435363738393031323334353637383930</secretdata>"
    ));
    assert!(xml.contains("<servertimediff>1</servertimediff>"));
    assert!(xml.contains("<bnauth:serial></bnauth:serial>"));
    assert!(xml.contains("<bnauth:restoreCode></bnauth:restoreCode>"));
}

#[test]
fn missing_and_exact_zero_offsets_never_request_winauth2_legacy_sync_but_measured_values_survive() {
    for serial_present in [true, false] {
        let mut record = record();
        if !serial_present {
            record.serial = None;
            record.restore_code = None;
            record.restore_code_source = None;
        }
        let original = record.clone();
        for (offset, expected) in [
            (None, 1),
            (Some(0), 1),
            (Some(1234), 1234),
            (Some(-1234), -1234),
        ] {
            let xml = encode(&record, offset).unwrap();
            assert!(xml.contains(&format!("<servertimediff>{expected}</servertimediff>")));
            assert_eq!(xml.matches("<servertimediff>").count(), 1);
            assert!(!xml.contains("<servertimediff>0</servertimediff>"));
            assert!(xml.contains("<autorefresh>true</autorefresh>"));
            assert_eq!(record, original, "the compatibility offset is XML-only");
        }
    }
}

#[test]
fn preserves_whitespace_and_maximum_stored_note_instead_of_display_truncation() {
    let mut record = record();
    record.name = "  原始標題\r\n  ".into();
    record.description = format!(" {}\r\n\t ", "長".repeat((MAX_DESCRIPTION_BYTES - 6) / 3));
    let xml = encode(&record, None).unwrap();
    assert!(xml.contains("<bnauth:title>  原始標題&#13;&#10;  </bnauth:title>"));
    assert!(xml.contains(&"長".repeat((MAX_DESCRIPTION_BYTES - 6) / 3)));
    assert!(xml.contains("&#13;&#10;&#9; </bnauth:note>"));
}

#[test]
fn rejects_missing_seed_and_unrepresentable_xml_instead_of_dropping_information() {
    let mut record = record();
    record.description = "有效 JSON 備註中的控制字元\u{1}".into();
    assert!(encode(&record, None).unwrap_err().contains("U+0001"));
    record.description = "\u{fffe}".into();
    assert!(encode(&record, None).unwrap_err().contains("U+FFFE"));
    record.description.clear();
    record.secret = None;
    assert!(encode(&record, None).unwrap_err().contains("尚未取得密鑰"));
}

#[test]
fn filename_defaults_to_title_and_stays_safe_on_windows() {
    let mut record = record();
    record.name = "主帳號".into();
    assert_eq!(suggested_filename(&record), "主帳號.xml");
    record.serial = None;
    record.id = uuid::Uuid::new_v4();
    assert_eq!(suggested_filename(&record), "主帳號.xml");
    record.name = "CON.foo/<>:\"\\|?*\r\n".into();
    let filename = suggested_filename(&record);
    assert!(filename.starts_with("_CON.foo"));
    assert!(filename.ends_with(".xml"));
    assert!(!filename.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*', '\r', '\n']));
    record.name = "😀".repeat(1000);
    assert!(suggested_filename(&record).encode_utf16().count() < 160);
}
