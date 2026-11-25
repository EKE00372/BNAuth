//! Generate public RFC 6238 test data for the original WinAuth2 compatibility test.
//! No account data or service requests are used.
use bnauth_adapters::{
    export_file::{self, Format},
    native, winauth2,
};
use bnauth_core::{VaultSnapshot, parse_manual, prepare_import};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Supply an output directory")?,
    );
    fs::create_dir_all(&directory)?;
    let empty = VaultSnapshot::new_empty();
    let mut vault = prepare_import(
        &empty,
        " 繁體 & <title> \"' 😀 ",
        "備註\r\n第二行\t<&>\"' -- ]]> ",
        empty.groups[0].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )?,
    )?
    .candidate(&empty)?;
    let record = &mut vault.authenticators[0];
    for (name, offset) in [
        ("with-serial", Some(-1234)),
        ("with-serial-local", None),
        ("with-serial-zero", Some(0)),
    ] {
        let xml = winauth2::encode(record, offset)?;
        export_file::write(
            &directory.join(format!("{name}.xml")),
            xml.as_bytes(),
            Format::WinAuth2Xml,
            None,
        )?;
    }
    // Also make an isolated vault for manual testing of the desktop export flow.
    fs::write(directory.join("BNAuth.bnauth"), &*native::encode(&vault)?)?;
    let record = &mut vault.authenticators[0];
    record.serial = None;
    record.restore_code = None;
    for (name, offset) in [
        ("seed-only", None),
        ("seed-only-zero", Some(0)),
        ("seed-only-offset", Some(-1234)),
    ] {
        let xml = winauth2::encode(record, offset)?;
        export_file::write(
            &directory.join(format!("{name}.xml")),
            xml.as_bytes(),
            Format::WinAuth2Xml,
            None,
        )?;
    }
    println!(
        "Generated six XML fixtures and an isolated vault using only the public RFC 6238 test key."
    );
    Ok(())
}
