use super::*;
use std::os::windows::fs::OpenOptionsExt;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("BNAuth-owned-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        // Only recursively remove this freshly generated fixture beneath the
        // resolved temp directory. remove_dir_all removes junction entries,
        // rather than traversing their targets.
        let Ok(root) = fs::canonicalize(&self.0) else {
            return;
        };
        let Ok(parent) = fs::canonicalize(std::env::temp_dir()) else {
            return;
        };
        if root.parent() == Some(parent.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("BNAuth-owned-export-"))
            && fs::symlink_metadata(&self.0)
                .is_ok_and(|metadata| !metadata.file_type().is_symlink())
        {
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[test]
fn verified_export_handle_blocks_replacement_and_stays_protected_through_install_and_readback() {
    let directory = Directory::new();
    let path = directory.0.join("owned.tmp");
    let destination = directory.0.join("匯出.xml");
    let other = directory.0.join("other.tmp");
    fs::write(&destination, b"old export").unwrap();
    fs::write(&other, b"ordinary other file").unwrap();
    let mut temporary = ExportTemporary::create(&path).unwrap();
    let bytes = b"verified new export";
    temporary.file_mut().write_all(bytes).unwrap();
    temporary.file_mut().sync_all().unwrap();
    temporary.verify(bytes).unwrap();
    assert!(fs::write(&path, b"another write").is_err());
    assert!(fs::rename(&path, directory.0.join("renamed.tmp")).is_err());
    assert!(fs::rename(&other, &path).is_err());
    assert!(fs::remove_file(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);

    temporary.install(&destination).unwrap();
    temporary.verify(bytes).unwrap();
    assert!(!path.exists());
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(fs::write(&destination, b"concurrent write").is_err());
    assert!(fs::rename(&destination, directory.0.join("moved.xml")).is_err());
    // Even a later verification failure must not arm temporary cleanup again.
    assert!(temporary.verify(b"wrong expected bytes").is_err());
    drop(temporary);
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    fs::write(&destination, b"ordinary later edit").unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"ordinary later edit");
    assert_eq!(fs::read(&other).unwrap(), b"ordinary other file");
}

#[test]
fn failed_handle_install_preserves_locked_destination_and_deletes_only_its_owned_temporary() {
    let directory = Directory::new();
    let path = directory.0.join("owned.tmp");
    let destination = directory.0.join("locked.xml");
    fs::write(&destination, b"old export").unwrap();
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&destination)
        .unwrap();
    let mut temporary = ExportTemporary::create(&path).unwrap();
    temporary.file_mut().write_all(b"new export").unwrap();
    temporary.file_mut().sync_all().unwrap();
    temporary.verify(b"new export").unwrap();
    assert!(temporary.install(&destination).is_err());
    temporary.verify(b"new export").unwrap();
    assert!(path.exists());
    drop(temporary);
    assert!(!path.exists());
    drop(locked);
    assert_eq!(fs::read(&destination).unwrap(), b"old export");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

fn junction(alias: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let quoted = |path: &Path| path.to_string_lossy().replace('\'', "''");
    let command = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        quoted(alias),
        quoted(target)
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn retarget_junction(directory: &Directory, alias: &Path, target: &Path) {
    assert_eq!(alias.parent(), Some(directory.0.as_path()));
    assert!(
        fs::symlink_metadata(alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir(alias).unwrap();
    junction(alias, target);
}

#[test]
fn retargeting_the_selected_parent_cannot_replace_an_unselected_destination() {
    let directory = Directory::new();
    let selected = directory.0.join("selected");
    let unselected = directory.0.join("unselected");
    let alias = directory.0.join("alias");
    fs::create_dir(&selected).unwrap();
    fs::create_dir(&unselected).unwrap();
    junction(&alias, &selected);
    let filename = "完整備份.bnauth";
    fs::write(selected.join(filename), b"original selected file").unwrap();
    fs::write(unselected.join(filename), b"unselected sentinel").unwrap();
    let bytes = b"verified new export";
    let mut temporary = ExportTemporary::create(&alias.join("owned.tmp")).unwrap();
    temporary.file_mut().write_all(bytes).unwrap();
    temporary.file_mut().sync_all().unwrap();
    temporary.verify(bytes).unwrap();
    retarget_junction(&directory, &alias, &unselected);
    temporary.install(&alias.join(filename)).unwrap();
    temporary.verify(bytes).unwrap();
    let installed = temporary.installed_path().unwrap();
    assert_eq!(
        installed,
        fs::canonicalize(selected.join(filename)).unwrap()
    );
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    assert_eq!(
        fs::read(alias.join(filename)).unwrap(),
        b"unselected sentinel"
    );
    assert!(fs::write(selected.join(filename), b"concurrent write").is_err());
    drop(temporary);
    assert_eq!(fs::read(selected.join(filename)).unwrap(), bytes);
    assert_eq!(
        fs::read(unselected.join(filename)).unwrap(),
        b"unselected sentinel"
    );
    assert!(!selected.join("owned.tmp").exists());
    assert!(!unselected.join("owned.tmp").exists());
}

#[test]
fn public_export_returns_the_installed_physical_path_after_the_selected_alias_changes() {
    let directory = Directory::new();
    let selected = directory.0.join("selected");
    let unselected = directory.0.join("unselected");
    let alias = directory.0.join("alias");
    fs::create_dir(&selected).unwrap();
    fs::create_dir(&unselected).unwrap();
    junction(&alias, &selected);
    let filename = "繁體標題.xml";
    fs::write(unselected.join(filename), b"unselected sentinel").unwrap();
    let bytes = b"new XML export";
    let installed = write(&alias.join(filename), bytes, Format::WinAuth2Xml, None).unwrap();
    assert_eq!(
        installed,
        fs::canonicalize(selected.join(filename)).unwrap()
    );
    retarget_junction(&directory, &alias, &unselected);
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    assert_eq!(
        fs::read(alias.join(filename)).unwrap(),
        b"unselected sentinel"
    );
    assert_eq!(fs::read_dir(&selected).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&unselected).unwrap().count(), 1);
}

#[test]
fn basename_only_exports_and_relative_primary_protection_work_from_the_current_directory() {
    const CHILD: &str = "BNAUTH_EXPORT_BASENAME_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let current = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(current.parent(), Some(temp.as_path()));
        assert!(
            current
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("BNAuth-owned-export-")
        );
        assert_eq!(fs::read_dir(".").unwrap().count(), 0);
        let primary = Path::new("BNAuth.bnauth");
        fs::write(primary, b"protected primary").unwrap();
        for (name, format) in [
            ("繁體標題.xml", Format::WinAuth2Xml),
            ("遊戲分組.bnauth", Format::Group),
            ("完整備份.bnauth", Format::Full),
        ] {
            let destination = Path::new(name);
            let installed = write(destination, b"new export", format, Some(primary)).unwrap();
            assert!(installed.is_absolute());
            assert_eq!(installed, fs::canonicalize(destination).unwrap());
            assert_eq!(fs::read(installed).unwrap(), b"new export");
        }
        let bound = ExportDirectory::open(Path::new("")).unwrap();
        assert!(bound.protects(primary, primary).unwrap());
        drop(bound);
        for destination in [primary, Path::new(r".\BNAuth.bnauth")] {
            assert!(
                write(destination, b"overwrite", Format::Full, Some(primary))
                    .unwrap_err()
                    .contains("不能覆蓋")
            );
        }
        assert_eq!(fs::read(primary).unwrap(), b"protected primary");
        assert_eq!(fs::read_dir(".").unwrap().count(), 4);
        return;
    }
    // Give only this exact child test its own working directory. Changing the
    // parent process CWD would race unrelated tests running on other threads.
    let directory = Directory::new();
    let thread = std::thread::current();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", thread.name().unwrap(), "--nocapture"])
        .env(CHILD, "1")
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "basename export child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(directory.0.join("完整備份.bnauth").exists());
}

#[test]
fn rooted_creation_and_primary_protection_use_the_bound_parent_after_alias_retarget() {
    let directory = Directory::new();
    let selected = directory.0.join("selected");
    let unselected = directory.0.join("unselected");
    let alias = directory.0.join("alias");
    fs::create_dir(&selected).unwrap();
    fs::create_dir(&unselected).unwrap();
    junction(&alias, &selected);
    let primary = selected.join("BNAuth.bnauth");
    fs::write(&primary, b"protected primary").unwrap();
    let bound = ExportDirectory::open(&alias).unwrap();
    retarget_junction(&directory, &alias, &unselected);
    let mut file = bound.create(std::ffi::OsStr::new("private.tmp")).unwrap();
    file.write_all(b"sensitive export bytes").unwrap();
    file.sync_all().unwrap();
    assert!(selected.join("private.tmp").exists());
    assert!(!unselected.join("private.tmp").exists());
    assert!(
        bound
            .protects(&alias.join("BNAuth.bnauth"), &primary)
            .unwrap()
    );
    assert!(
        !bound
            .protects(&alias.join("BNAuth-full.bnauth"), &primary)
            .unwrap()
    );
    drop(file);
    // The public write path must also reject the actually bound primary parent,
    // including a normal alias to that directory, without changing its bytes.
    retarget_junction(&directory, &alias, &selected);
    assert!(
        write(
            &alias.join("BNAuth.bnauth"),
            b"new",
            Format::Full,
            Some(&primary)
        )
        .is_err()
    );
    assert_eq!(fs::read(&primary).unwrap(), b"protected primary");
}

#[test]
fn an_open_parent_cannot_be_renamed_and_replaced_until_installation_finishes() {
    let directory = Directory::new();
    let selected = directory.0.join("selected");
    let moved = directory.0.join("moved");
    fs::create_dir(&selected).unwrap();
    let destination = selected.join("驗證器.xml");
    fs::write(&destination, b"selected old export").unwrap();
    let mut temporary = ExportTemporary::create(&selected.join("owned.tmp")).unwrap();
    temporary.file_mut().write_all(b"new export").unwrap();
    temporary.file_mut().sync_all().unwrap();
    temporary.verify(b"new export").unwrap();
    assert!(fs::rename(&selected, &moved).is_err());
    assert!(fs::create_dir(&selected).is_err());
    temporary.install(&destination).unwrap();
    temporary.verify(b"new export").unwrap();
    drop(temporary);
    fs::rename(&selected, &moved).unwrap();
    fs::create_dir(&selected).unwrap();
    fs::write(&destination, b"replacement sentinel").unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"replacement sentinel");
    assert_eq!(fs::read(moved.join("驗證器.xml")).unwrap(), b"new export");
}

#[test]
fn native_relative_names_reject_paths_streams_and_uncompleted_statuses() {
    for value in [
        "",
        ".",
        "..",
        "a/b",
        r"a\b",
        r"\absolute",
        "file:stream",
        "invalid\0name",
    ] {
        assert!(
            relative_name(std::ffi::OsStr::new(value)).is_err(),
            "accepted {value:?}"
        );
    }
    assert!(relative_name(std::ffi::OsStr::new(&"a".repeat(32767))).is_err());
    let valid = relative_name(std::ffi::OsStr::new("繁體分組.bnauth")).unwrap();
    assert_eq!(valid.last(), Some(&0));
    assert!(native_result(windows_sys::Win32::Foundation::STATUS_PENDING).is_err());
}
