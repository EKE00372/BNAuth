//! Single-authenticator WinAuth 2 XML, independently written from its file format.
//! The original reader ignores our namespaced metadata; its writer drops it.
use bnauth_core::{Authenticator, MAX_DESCRIPTION_BYTES, MAX_NAME_BYTES, display_serial};
use std::fmt::Write;
use zeroize::Zeroizing;

pub const METADATA_NAMESPACE: &str = "urn:bnauth:metadata:1";

/// Only committed data should be supplied here. A missing serial stays missing;
/// the 40-digit seed alone is also a valid WinAuth 2 SecretData value.
pub fn encode(
    record: &Authenticator,
    time_difference_ms: Option<i64>,
) -> Result<Zeroizing<String>, String> {
    let secret = record
        .secret
        .as_ref()
        .ok_or("尚未取得密鑰，無法匯出可產碼的 WinAuth2 XML。")?;
    if record.digits != 8 || record.algorithm != "SHA1" || record.period_secs != 30 {
        return Err("WinAuth2 匯出需要八位碼、SHA1 與 30 秒週期。".into());
    }
    bnauth_core::valid_name(&record.name, MAX_NAME_BYTES, "標題")?;
    bnauth_core::bounded(&record.description, MAX_DESCRIPTION_BYTES, "備註")?;
    let serial = match &record.serial {
        Some(serial) => {
            if bnauth_core::normalize_serial(serial)?.as_deref() != Some(serial.as_str()) {
                return Err("序號尚未正規化，無法匯出。".into());
            }
            display_serial(serial)
        }
        None => String::new(),
    };
    if let Some(code) = &record.restore_code {
        bnauth_core::valid_name(code, 64, "還原碼")?;
    }

    let mut xml = Zeroizing::new(String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>\n\
         <WinAuth version=\"2.0\">\n\
         \x20 <autorefresh>true</autorefresh>\n\
         \x20 <allowcopy>true</allowcopy>\n\
         \x20 <authenticator type=\"WindowsAuthenticator.BattleNetAuthenticator\">\n",
    ));
    // WinAuth 2 treats numeric zero as "sync before producing a code" and
    // calls its legacy HTTP service. Keep local/zero-offset exports usable
    // offline with a +1 ms compatibility adjustment; this is not a server
    // measurement. Preserve every nonzero supplied session offset exactly.
    let compatible_offset = time_difference_ms
        .filter(|offset| *offset != 0)
        .unwrap_or(1);
    writeln!(
        xml,
        "    <servertimediff>{}</servertimediff>",
        compatible_offset
    )
    .unwrap();
    xml.push_str("    <secretdata encrypted=\"\">");
    // Write bytes directly into the zeroizing output instead of retaining a
    // second unprotected hexadecimal copy of the seed.
    for byte in secret.bytes().iter().chain(serial.as_bytes()) {
        write!(xml, "{byte:02X}").unwrap();
    }
    xml.push_str("</secretdata>\n  </authenticator>\n");
    writeln!(
        xml,
        "  <bnauth:metadata xmlns:bnauth=\"{METADATA_NAMESPACE}\" version=\"1\" xml:space=\"preserve\">"
    )
    .unwrap();
    for (field, label, value) in [
        ("title", "標題", record.name.as_str()),
        ("note", "備註", record.description.as_str()),
        ("serial", "序號", serial.as_str()),
        (
            "restoreCode",
            "還原碼",
            record.restore_code.as_deref().unwrap_or(""),
        ),
    ] {
        write!(xml, "    <bnauth:{field}>").unwrap();
        escape_text(&mut xml, value, label)?;
        writeln!(xml, "</bnauth:{field}>").unwrap();
    }
    xml.push_str("  </bnauth:metadata>\n</WinAuth>\n");
    Ok(xml)
}

fn escape_text(xml: &mut String, value: &str, label: &str) -> Result<(), String> {
    for ch in value.chars() {
        match ch {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\'' => xml.push_str("&apos;"),
            // Character references preserve CR/LF and tabs through XML readers'
            // line-ending normalization, including notes edited outside the GUI.
            '\r' => xml.push_str("&#13;"),
            '\n' => xml.push_str("&#10;"),
            '\t' => xml.push_str("&#9;"),
            '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}' => {
                xml.push(ch)
            }
            _ => {
                return Err(format!(
                    "{label}含 XML 1.0 無法保存的字元（U+{:04X}）。",
                    ch as u32
                ));
            }
        }
    }
    Ok(())
}

pub fn suggested_filename(record: &Authenticator) -> String {
    crate::export_file::suggested_filename(&record.name, crate::export_file::Format::WinAuth2Xml)
}

#[cfg(test)]
#[path = "winauth2_tests.rs"]
mod tests;
