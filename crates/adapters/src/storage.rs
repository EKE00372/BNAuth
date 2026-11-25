//! Durable single-writer repository. All methods and Drop stay on its worker thread.
use crate::{native, operation_io_error, temp_cleanup_error};
use bnauth_core::{ErrorCode, VaultSnapshot};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug)]
pub struct CommitOutcome {
    pub snapshot: VaultSnapshot,
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    identity: FileIdentity,
    hash: [u8; 32],
}

// The UI owns the decoded vault. The header checks candidate revisions and the
// stamp identifies the exact file and bytes that were decoded and acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AcknowledgedPrimary {
    vault_id: Uuid,
    revision: u64,
    stamp: FileStamp,
}

impl AcknowledgedPrimary {
    fn new(snapshot: &VaultSnapshot, stamp: FileStamp) -> Self {
        Self {
            vault_id: snapshot.vault_id,
            revision: snapshot.revision,
            stamp,
        }
    }
}

#[derive(Clone, Debug)]
struct OwnedFile {
    path: PathBuf,
    stamp: FileStamp,
}
struct RollbackGuard {
    file: File,
    can_delete: bool,
}
impl RollbackGuard {
    fn into_delete_handle(self) -> Option<File> {
        self.can_delete.then_some(self.file)
    }
}
struct CleanupFailure {
    code: ErrorCode,
    changed: Option<String>,
}
impl CleanupFailure {
    fn retry(code: ErrorCode) -> Self {
        Self {
            code,
            changed: None,
        }
    }
    fn changed(message: String) -> Self {
        Self {
            code: ErrorCode::DeleteFailed,
            changed: Some(message),
        }
    }
}

// The caller holds a handle that denies outside writes and renames throughout
// identity capture and bounded reading.
fn inspect_open(file: &mut File) -> Result<(Zeroizing<Vec<u8>>, FileStamp), String> {
    let identity = platform::file_identity(file)?;
    let bytes = read_open(file)?;
    let hash = Sha256::digest(&*bytes).into();
    Ok((bytes, FileStamp { identity, hash }))
}

fn read_open(file: &mut File) -> Result<Zeroizing<Vec<u8>>, String> {
    file.seek(SeekFrom::Start(0)).map_err(primary_io_error)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(native::MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(primary_io_error)?;
    if bytes.len() > native::MAX_FILE {
        return Err(ErrorCode::FileTooLarge.notice().to_string());
    }
    Ok(bytes)
}

fn stamp_open(file: &mut File) -> Result<FileStamp, String> {
    stamp_and_compare(file, None).map(|(stamp, _)| stamp)
}

fn stamp_and_compare(
    file: &mut File,
    expected: Option<&[u8]>,
) -> Result<(FileStamp, bool), String> {
    let identity = platform::file_identity(file)?;
    file.rewind().map_err(primary_io_error)?;
    let mut reader = file.take(native::MAX_FILE as u64 + 1);
    let mut buffer = Zeroizing::new([0_u8; 16 * 1024]);
    let mut hash = Sha256::new();
    let mut remaining = expected;
    loop {
        match reader.read(&mut buffer[..]) {
            Ok(0) => break,
            Ok(_) if reader.limit() == 0 => {
                return Err(ErrorCode::FileTooLarge.notice().to_string());
            }
            Ok(n) => {
                hash.update(&buffer[..n]);
                remaining = remaining.and_then(|bytes| bytes.strip_prefix(&buffer[..n]));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(primary_io_error(error)),
        }
    }
    Ok((
        FileStamp {
            identity,
            hash: hash.finalize().into(),
        },
        remaining == Some(&[]),
    ))
}

struct OpenedPrimary<T> {
    file: File,
    decoded: T,
    stamp: FileStamp,
}

fn open_primary(
    path: &Path,
    allow_rename: bool,
) -> Result<Option<OpenedPrimary<VaultSnapshot>>, String> {
    read_primary(path, allow_rename, |bytes, _| {
        native::decode(bytes).map_err(|error| {
            if ErrorCode::GroupBackupSelected.matches_message(&error) {
                ErrorCode::GroupBackupSelected
                    .with_detail("這是分組備份。\n請先用完整設定檔開啟 BNAuth，再使用「匯入單組備份」。")
                    .to_string()
            } else {
                error
            }
        })
    })
}

fn read_primary<T>(
    path: &Path,
    allow_rename: bool,
    decode: impl FnOnce(&[u8], FileStamp) -> Result<T, String>,
) -> Result<Option<OpenedPrimary<T>>, String> {
    let Some(mut file) = platform::open_guarded(path, allow_rename)? else {
        return Ok(None);
    };
    let (bytes, stamp) = inspect_open(&mut file)?;
    Ok(Some(OpenedPrimary {
        file,
        decoded: decode(&bytes, stamp)?,
        stamp,
    }))
}

fn read_stamp_locked(path: &Path) -> Result<Option<FileStamp>, String> {
    let Some(mut file) = platform::open_guarded(path, false)? else {
        return Ok(None);
    };
    stamp_open(&mut file).map(Some)
}

fn delete_owned(owned: &OwnedFile, inject_failure: bool) -> Result<(), CleanupFailure> {
    let Some(mut file) = platform::open_guarded_io(&owned.path, true).map_err(cleanup_io_error)? else {
        return Ok(());
    };
    let actual = stamp_open(&mut file).map_err(|_| {
        CleanupFailure::changed(format!("無法核對暫存檔。請保留檔案：{}", owned.path.display()))
    })?;
    if actual != owned.stamp {
        return Err(CleanupFailure::changed(format!(
            "暫存檔已被外部修改或替換，已保留在原位置，不會刪除：{}",
            owned.path.display()
        )));
    }
    delete_verified(&file, inject_failure)
}

fn delete_verified(file: &File, inject_failure: bool) -> Result<(), CleanupFailure> {
    if inject_failure {
        return Err(CleanupFailure::retry(ErrorCode::FileInUse));
    }
    // Keep this exact verified handle open through disposition; a path-based
    // delete after closing it would reintroduce a check/delete race.
    platform::delete_open(file).map_err(cleanup_io_error)
}

fn primary_is_read_only(primary: Option<&File>) -> Result<bool, String> {
    let Some(primary) = primary else {
        return Ok(false);
    };
    let metadata = primary.metadata().map_err(primary_io_error)?;
    Ok(metadata.permissions().readonly())
}

struct NtfsDirectory {
    path: PathBuf,
    read_only_volume: bool,
}

pub struct Repository {
    path: PathBuf,
    original_parent: PathBuf,
    canonical_parent: PathBuf,
    current: Option<AcknowledgedPrimary>,
    write_access_denied: bool,
    primary_read_only: bool,
    blocked: bool,
    cleanup_pending: Vec<OwnedFile>,
    // Release the deny-write/delete handle before the named mutex on Drop.
    primary_guard: Option<File>,
    _lock: platform::Lock,
    _thread_bound: PhantomData<Rc<()>>,
    #[cfg(test)]
    fault: Option<Fault>,
}
impl Repository {
    pub fn open(path: PathBuf) -> Result<(Self, Option<VaultSnapshot>), String> {
        let original_parent = path
            .parent()
            .ok_or("未指定資料檔所在的資料夾。")?
            .to_path_buf();
        let NtfsDirectory {
            path: canonical_parent,
            read_only_volume,
        } = platform::canonical_ntfs_directory(&original_parent)?;
        let path = canonical_parent.join("BNAuth.bnauth");
        let lock = platform::Lock::acquire(&path)?;
        if platform::canonical_directory(&original_parent)? != canonical_parent {
            return Err("開啟資料檔時，所在的資料夾已變更。請重新開啟 BNAuth。".into());
        }
        // Our primary guard denies deletion, so check replacement access before acquiring it.
        let write_access_denied =
            read_only_volume || platform::save_access_denied(&canonical_parent, &path);
        let primary = open_primary(&path, false);
        let remnants = find_remnants(&canonical_parent)?;
        let fallback_read_only = matches!(primary, Ok(None)) && write_access_denied;
        let primary = match primary {
            Ok(None) => recover_startup_primary(&path, &remnants, write_access_denied),
            primary => primary,
        };
        let acknowledged = primary
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|opened| AcknowledgedPrimary::new(&opened.decoded, opened.stamp));
        let cleanup = clean_startup_remnants(
            &remnants,
            &original_parent,
            write_access_denied,
            acknowledged.is_some() && !fallback_read_only,
        );
        let (loaded, primary_guard) = match primary? {
            Some(opened) => (Some(opened.decoded), Some(opened.file)),
            None => (None, None),
        };
        cleanup?;
        let primary_read_only = primary_is_read_only(primary_guard.as_ref())?;
        Ok((
            Self {
                path,
                original_parent,
                canonical_parent,
                current: acknowledged,
                write_access_denied,
                primary_read_only,
                blocked: false,
                cleanup_pending: vec![],
                primary_guard,
                _lock: lock,
                _thread_bound: PhantomData,
                #[cfg(test)]
                fault: None,
            },
            loaded,
        ))
    }
    pub fn cleanup_required(&self) -> bool {
        !self.cleanup_pending.is_empty()
    }
    pub fn defer_cleanup(&mut self) {
        if !self.blocked {
            self.cleanup_pending.clear();
        }
    }
    pub fn is_blocked(&self) -> bool {
        self.blocked
    }
    pub fn is_read_only(&self) -> bool {
        self.write_access_denied || self.primary_read_only
    }

    /// The backup reader checked the records and the preview checked the target vault ID.
    pub fn restore_from_preview(
        &mut self,
        candidate: &VaultSnapshot,
    ) -> Result<CommitOutcome, String> {
        let revision = self.validate_candidate(candidate, true)?;
        self.commit_validated(candidate, revision)
    }

    /// Candidate uses the last committed revision. This method owns revision advancement.
    pub fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        let revision = self.validate_candidate(candidate, false)?;
        self.commit_validated(candidate, revision)
    }

    /// Save an edit whose changed records, groups and ordering were checked
    /// against the acknowledged vault by the operation that prepared it.
    pub fn commit_edit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        let revision = self.validate_candidate(candidate, self.current.is_some())?;
        self.commit_validated(candidate, revision)
    }

    fn validate_candidate(
        &mut self,
        candidate: &VaultSnapshot,
        contents_checked: bool,
    ) -> Result<u64, String> {
        if self.blocked {
            return Err(ErrorCode::WriteFailed
                .with_detail("資料檔狀態需要檢查。請保留檔案並重新開啟 BNAuth。")
                .to_string());
        }
        if !self.cleanup_pending.is_empty() {
            return Err(
                ErrorCode::DeleteFailed
                    .with_detail("上次保存留下的暫存檔尚未移除，請先按「清理暫存檔」。")
                    .to_string(),
            );
        }
        self.primary_read_only = primary_is_read_only(self.primary_guard.as_ref())?;
        if self.is_read_only() {
            return Err(ErrorCode::PermissionDenied.notice().to_string());
        }
        if contents_checked {
            candidate.validate_edit_metadata()?;
        } else {
            candidate.validate()?;
        }
        match &self.current {
            Some(old) => {
                if candidate.vault_id != old.vault_id || candidate.revision != old.revision {
                    return Err("目前資料與上次讀取的資料檔不符，請重新讀取後再操作。".into());
                }
            }
            None if candidate.revision != 0 => {
                return Err("新資料檔的版本紀錄不正確，無法保存。".into());
            }
            None => (),
        }
        candidate
            .revision
            .checked_add(1)
            .ok_or_else(|| "資料檔的保存次數已達上限，無法繼續保存。".into())
    }

    fn commit_validated(
        &mut self,
        candidate: &VaultSnapshot,
        revision: u64,
    ) -> Result<CommitOutcome, String> {
        // The existing primary stays locked against writes and replacement while staging.
        self.check_primary_path()?;
        let mut next = candidate.file_view();
        next.revision = revision;
        let encoded = native::encode_validated(&next)?;
        let transaction = Uuid::new_v4();
        let temp = self
            .canonical_parent
            .join(format!(".BNAuth.bnauth.{transaction}.tmp"));
        let rollback = self
            .canonical_parent
            .join(format!(".BNAuth.bnauth.{transaction}.rollback"));
        let previous = self.current.map(|primary| primary.stamp);
        let mut staged: Option<(OwnedFile, File)> = None;

        let stage_result = (|| -> Result<(), String> {
            self.inject(FaultPoint::BeforeWrite)?;
            #[cfg(test)]
            if self.fault == Some(Fault::TempAlreadyExists) {
                fs::write(&temp, b"external pre-existing temp").map_err(write_io_error)?;
            }
            let create_result = {
                #[cfg(test)]
                if let Some(Fault::TempCreate(kind)) = self.fault {
                    Err(std::io::Error::new(kind, "模擬暫存檔建立失敗"))
                } else {
                    platform::create_temp(&temp)
                }
                #[cfg(not(test))]
                platform::create_temp(&temp)
            };
            let mut file = create_result.map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    self.blocked = true;
                    ErrorCode::WriteFailed
                        .with_detail(format!(
                            "下列暫存檔由其他程式建立，已保留在原位置：{}",
                            temp.display()
                        ))
                        .to_string()
                } else {
                    write_io_error(error)
                }
            })?;
            let write_result = (|| -> Result<(), String> {
                #[cfg(test)]
                if matches!(
                    self.fault,
                    Some(Fault::PartialWrite | Fault::PartialWriteAndCleanup)
                ) {
                    file.write_all(&encoded[..encoded.len() / 2])
                        .map_err(write_io_error)?;
                    return Err(ErrorCode::WriteFailed.notice().to_string());
                }
                file.write_all(&encoded).map_err(write_io_error)?;
                self.inject(FaultPoint::BeforeFlush)?;
                file.sync_all().map_err(write_io_error)
            })();
            // The original creation handle owns even a partial write. Only read
            // a failed write if cleanup fails and we will need its stamp to retry.
            if let Err(reason) = write_result {
                let failed = self.inject(FaultPoint::Cleanup).is_err();
                let Err(error) = delete_verified(&file, failed) else {
                    return Err(reason);
                };
                let (owned, _) = self.inspect_stage(&mut file, &temp, None)?;
                self.retain_for_cleanup(&owned, error);
                return Err(reason);
            }
            let (owned, matches) = self.inspect_stage(&mut file, &temp, Some(&encoded))?;
            staged = Some((owned, file));
            if !matches {
                return Err(ErrorCode::WriteFailed.notice().to_string());
            }
            self.inject(FaultPoint::AfterStage)?;
            // The guard keeps the acknowledged bytes stable while rechecking the path.
            self.check_primary_path()?;
            self.inject(FaultPoint::BeforeReplace)?;
            Ok(())
        })();
        drop(encoded);
        if let Err(error) = stage_result {
            return Err(match staged {
                Some((owned, file)) => self.discard_uncommitted(&owned, Some(file), error),
                None => error,
            });
        }
        let (staged, file) = staged.expect("successful stage owns its created file");
        drop(file);
        // ReplaceFileW needs DELETE access even in this process. Keep the main
        // guard throughout staging, then release it only for the atomic install.
        drop(self.primary_guard.take());
        let replace_result = self.install(&temp, &rollback);
        // Verification and the new lifetime guard are the same open handle;
        // there is no verified-but-unlocked success state to acknowledge.
        let opened = if self.inject(FaultPoint::ReadBack).is_ok() {
            self.inject(FaultPoint::ReacquirePrimary).and_then(|()| {
                read_primary(&self.path, false, |bytes, stamp| {
                    if stamp == staged.stamp {
                        native::verify_written(bytes, candidate, revision)
                    } else if Some(stamp) == previous {
                        // This handle guards exactly the previously validated bytes.
                        Ok(())
                    } else {
                        native::decode(bytes).map(drop)
                    }
                })
            })
        } else {
            Ok(None)
        };
        let verified_primary = match opened {
            Ok(Some(opened)) if opened.stamp == staged.stamp => {
                self.primary_guard = Some(opened.file);
                let mut saved = candidate.clone();
                saved.revision = revision;
                Some((saved, opened.stamp))
            }
            Ok(Some(opened)) if previous == Some(opened.stamp) => {
                // Replace was refused and the complete readback found the
                // acknowledged original. Keep this guard through failure cleanup.
                self.primary_guard = Some(opened.file);
                None
            }
            Ok(_) => None,
            Err(error) => {
                return self.block_for_recovery(
                    &temp,
                    &rollback,
                    verification_error_code(&error),
                    "無法讀回設定檔核對或恢復檔案保護。",
                );
            }
        };
        let rollback_owned = previous.map(|stamp| OwnedFile {
            path: rollback.clone(),
            stamp,
        });
        let mut rollback_verified = previous.is_none();
        let mut rollback_handle = None;
        if let Some(old_stamp) = previous {
            // Keep the verified rollback locked for cleanup. A reader can deny
            // DELETE while still allowing verification of the completed save.
            let mut can_delete = true;
            let file = platform::open_guarded(&rollback, true).or_else(|_| {
                can_delete = false;
                platform::open_guarded(&rollback, false)
            });
            let stamp = file.and_then(|file| {
                file.map(|mut file| {
                    let stamp = stamp_open(&mut file);
                    rollback_handle = Some(RollbackGuard { file, can_delete });
                    stamp
                })
                .transpose()
            });
            match stamp {
                Ok(Some(stamp)) if stamp == old_stamp => rollback_verified = true,
                Ok(None) => (),
                result => {
                    let code = result.as_ref().err().map_or(ErrorCode::WriteFailed, |error| {
                        verification_error_code(error)
                    });
                    return self.block_for_recovery(
                        &temp,
                        &rollback,
                        code,
                        "保存期間原資料檔被外部修改，或無法確認原資料備份是否完整；已保留所有檔案，沒有刪除原資料備份。",
                    );
                }
            }
        }
        // API status alone is insufficient. The held, verified candidate and
        // verified previous-file rollback are the commit point.
        if let Some((snapshot, stamp)) = verified_primary {
            if !rollback_verified {
                return self.block_for_recovery(
                    &temp,
                    &rollback,
                    ErrorCode::WriteFailed,
                    "新的資料檔已寫入，但找不到原資料備份，無法確認保存期間是否有外部修改。",
                );
            }
            self.current = Some(AcknowledgedPrimary::new(&snapshot, stamp));
            // The installed primary has the staged file's verified identity and
            // single link. Its former .tmp name cannot still refer to that file.
            // Ordinary deletion failures can wait for the next successful open.
            let warning = if let Some(owned) = &rollback_owned
                && let Err(error) = self.remove_transaction_file(
                    owned,
                    rollback_handle.and_then(RollbackGuard::into_delete_handle),
                )
                && error.changed.is_some()
            {
                let detail = self.retain_for_cleanup(owned, error);
                Some(format!("資料已保存，但暫存檔已變動。\n詳細原因：{detail}"))
            } else {
                None
            };
            return Ok(CommitOutcome { snapshot, warning });
        }
        let reason = replace_result
            .err()
            .unwrap_or_else(|| ErrorCode::WriteFailed.notice().to_string());
        let displaced = if self.primary_guard.is_none() {
            self.recover_failed_commit(&staged, &mut rollback_handle, &rollback, &reason)?
        } else {
            None
        };
        let mut error = reason;
        for (owned, file) in std::iter::once((&staged, None))
            .chain(
                rollback_owned
                    .as_ref()
                    .zip(rollback_handle)
                    .map(|(owned, guard)| (owned, guard.into_delete_handle())),
            )
            .chain(displaced.as_ref().map(|owned| (owned, None)))
        {
            error = self.discard_uncommitted(owned, file, error);
            if self.blocked {
                break;
            }
        }
        Err(error)
    }

    pub fn retry_cleanup(&mut self) -> Result<(), String> {
        if self.blocked {
            return Err(ErrorCode::DeleteFailed
                .with_detail("檔案已被外部修改或狀態無法確認。請保留檔案以供檢查。")
                .to_string());
        }
        let pending = std::mem::take(&mut self.cleanup_pending);
        let mut failures = Vec::new();
        let mut blocked_error = None;
        for owned in pending {
            if self.blocked {
                self.cleanup_pending.push(owned);
                continue;
            }
            if let Err(error) = self.remove_transaction_file(&owned, None) {
                if error.changed.is_some() {
                    blocked_error = Some(self.retain_for_cleanup(&owned, error));
                } else {
                    failures.push((error.code, owned.path.clone()));
                    self.cleanup_pending.push(owned);
                }
            }
        }
        if let Some(error) = blocked_error {
            return Err(error);
        }
        if failures.is_empty() {
            return Ok(());
        }
        let display_directory = if self.check_current_directory().is_ok() {
            &self.original_parent
        } else {
            &self.canonical_parent
        };
        cleanup_failures(failures, display_directory)
    }

    fn reacquire_current(&mut self) -> Result<(), String> {
        self.inject(FaultPoint::ReacquirePrimary)?;
        match (self.current, open_primary(&self.path, false)?) {
            (Some(expected), Some(opened))
                if AcknowledgedPrimary::new(&opened.decoded, opened.stamp) == expected =>
            {
                self.primary_guard = Some(opened.file);
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err("恢復後的資料檔與上次確認的內容不符，無法恢復檔案保護。".into()),
        }
    }

    fn check_current_directory(&self) -> Result<(), String> {
        if platform::canonical_directory(&self.original_parent)? != self.canonical_parent {
            return Err("資料檔所在的資料夾已變更，已停止保存。".into());
        }
        Ok(())
    }

    fn check_primary_path(&self) -> Result<(), String> {
        self.check_current_directory()?;
        let matches = match (self.current, platform::open_guarded(&self.path, false)?) {
            (Some(expected), Some(file)) => platform::file_identity(&file)? == expected.stamp.identity,
            (None, None) => true,
            (None, Some(_)) => {
                return Err("資料檔已由其他程式建立，已停止保存；請重新開啟 BNAuth。".into());
            }
            (Some(_), None) => false,
        };
        if matches {
            Ok(())
        } else {
            Err("資料檔已被其他程式修改，已停止保存；請重新開啟 BNAuth。".into())
        }
    }
    fn install(&self, temp: &Path, rollback: &Path) -> Result<(), String> {
        #[cfg(test)]
        match self.fault {
            Some(Fault::Replace1176) => {
                return Err(write_io_error(std::io::Error::from_raw_os_error(1176)));
            }
            Some(Fault::Replace1177) => {
                platform::move_no_replace(&self.path, rollback)?;
                return Err(write_io_error(std::io::Error::from_raw_os_error(1177)));
            }
            Some(Fault::ExternalChangeBeforeReplace) => {
                let mut external = native::read(&self.path)?.0;
                external.revision += 1;
                let sort_order = external.groups.len() as u32;
                external.groups.push(bnauth_core::Group {
                    id: Uuid::new_v4(),
                    name: "外部修改".into(),
                    sort_order,
                    is_ungrouped: false,
                });
                fs::write(&self.path, &*native::encode(&external)?).map_err(write_io_error)?;
            }
            Some(Fault::ExternalReformatBeforeReplace) => {
                let compact = serde_json::to_vec(&native::read(&self.path)?.0).unwrap();
                fs::write(&self.path, compact).map_err(write_io_error)?;
            }
            Some(Fault::ExternalFirstCreate) => {
                fs::write(&self.path, b"external first-create content").map_err(write_io_error)?;
            }
            _ => (),
        }
        if self.current.is_some() {
            platform::replace(&self.path, temp, Some(rollback))
        } else {
            platform::move_no_replace(temp, &self.path)
        }
    }

    fn remove_transaction_file(
        &self,
        owned: &OwnedFile,
        verified_file: Option<File>,
    ) -> Result<(), CleanupFailure> {
        #[cfg(test)]
        if self.fault == Some(Fault::ExternalChangeBeforeCleanup)
            && owned.path.extension().is_some_and(|s| s == "rollback")
        {
            assert!(fs::write(&owned.path, b"external cleanup content").is_err());
        }
        let inject_failure = self.inject(FaultPoint::Cleanup).is_err();
        match verified_file {
            Some(file) => delete_verified(&file, inject_failure),
            None => delete_owned(owned, inject_failure),
        }
    }
    fn cleanup(&mut self, owned: &OwnedFile, verified_file: Option<File>) -> Result<(), String> {
        self.remove_transaction_file(owned, verified_file)
            .map_err(|error| self.retain_for_cleanup(owned, error))
    }
    fn inspect_stage(
        &mut self,
        file: &mut File,
        path: &Path,
        expected: Option<&[u8]>,
    ) -> Result<(OwnedFile, bool), String> {
        let (stamp, matches) = stamp_and_compare(file, expected).map_err(|error| {
            self.blocked = true;
            if ErrorCode::FileTooLarge.matches_message(&error) {
                error
            } else {
                ErrorCode::WriteFailed
                    .with_detail(format!(
                        "無法確認本次保存的暫存檔狀態，請保留檔案以供檢查：{}",
                        path.display()
                    ))
                    .to_string()
            }
        })?;
        Ok((
            OwnedFile {
                path: path.to_owned(),
                stamp,
            },
            matches,
        ))
    }
    fn retain_for_cleanup(&mut self, owned: &OwnedFile, error: CleanupFailure) -> String {
        self.blocked |= error.changed.is_some();
        self.cleanup_pending.push(owned.clone());
        match error.changed {
            Some(detail) => error.code.with_detail(detail).to_string(),
            None => error.code.notice().to_string(),
        }
    }
    fn discard_uncommitted(
        &mut self,
        owned: &OwnedFile,
        verified_file: Option<File>,
        reason: String,
    ) -> String {
        match self.cleanup(owned, verified_file) {
            Ok(()) => reason,
            Err(error) if self.blocked && reason != ErrorCode::DiskFull.notice().to_string() => error,
            Err(_) => reason,
        }
    }
    fn block_for_recovery<T>(
        &mut self,
        temp: &Path,
        rollback: &Path,
        code: ErrorCode,
        reason: &str,
    ) -> Result<T, String> {
        self.blocked = true;
        if matches!(code, ErrorCode::DiskFull | ErrorCode::FileTooLarge) {
            return Err(code.notice().to_string());
        }
        Err(code
            .with_detail(format!(
                "{reason}\n請保留下列位置的檔案供檢查：\n{}\n{}\n{}\n{}",
                self.path.display(),
                temp.display(),
                rollback.display(),
                rollback.with_extension("recovery").display()
            ))
            .to_string())
    }
    fn recover_failed_commit(
        &mut self,
        staged: &OwnedFile,
        rollback: &mut Option<RollbackGuard>,
        rollback_path: &Path,
        reason: &str,
    ) -> Result<Option<OwnedFile>, String> {
        debug_assert!(self.primary_guard.is_none());
        // Expected stamps identify already validated bytes. Reacquiring the
        // acknowledged primary below still validates its full contents and guard.
        let actual = read_stamp_locked(&self.path);
        let actual_stamp = actual.as_ref().ok().copied().flatten();
        let main_missing = matches!(actual, Ok(None));
        let mut displaced: Option<OwnedFile> = None;
        let restored = match self.current.map(|primary| primary.stamp) {
            Some(old_stamp) if actual_stamp == Some(old_stamp) => true,
            Some(_) if rollback.is_some() => {
                let can_restore = if main_missing {
                    true
                } else if actual_stamp == Some(staged.stamp) {
                    #[cfg(test)]
                    if self.fault == Some(Fault::ExternalChangeBeforeRecovery) {
                        fs::write(&self.path, b"external recovery content")
                            .map_err(write_io_error)?;
                    }
                    // Never overwrite a name after only checking its old bytes.
                    // Isolate whichever file currently occupies it without deleting
                    // anything, then verify the captured file before restoring.
                    let path = rollback_path.with_extension("recovery");
                    if platform::move_no_replace(&self.path, &path).is_ok() {
                        let captured = read_stamp_locked(&path).ok().flatten();
                        displaced = Some(OwnedFile {
                            path,
                            stamp: staged.stamp,
                        });
                        captured == Some(staged.stamp)
                    } else {
                        false
                    }
                } else {
                    false
                };
                // reacquire_current verifies the restored identity and contents
                // under the same handle that will guard the next save.
                can_restore && {
                    drop(rollback.take());
                    platform::move_no_replace(rollback_path, &self.path).is_ok()
                }
            }
            None if main_missing => true,
            None if actual_stamp == Some(staged.stamp) => {
                #[cfg(test)]
                if self.fault == Some(Fault::ExternalChangeBeforeRecovery) {
                    fs::write(&self.path, b"external recovery content").map_err(write_io_error)?;
                }
                delete_owned(
                    &OwnedFile {
                        path: self.path.clone(),
                        stamp: staged.stamp,
                    },
                    false,
                )
                .is_ok()
            }
            _ => false,
        };
        let detail = if !restored {
            "無法確認原資料是否已恢復，已停止保存。"
        } else if self.reacquire_current().is_err() {
            "原資料已恢復，但無法確認內容或恢復檔案保護。"
        } else {
            return Ok(displaced);
        };
        let code = [
            ErrorCode::PermissionDenied,
            ErrorCode::FileInUse,
            ErrorCode::DiskFull,
        ]
        .into_iter()
        .find(|code| code.matches_message(reason))
        .unwrap_or(ErrorCode::WriteFailed);
        self.block_for_recovery(&staged.path, rollback_path, code, detail)
    }
    fn inject(&self, point: FaultPoint) -> Result<(), String> {
        #[cfg(test)]
        if self.fault == Some(Fault::At(point))
            || (point == FaultPoint::Cleanup && self.fault == Some(Fault::PartialWriteAndCleanup))
            || (point == FaultPoint::ReadBack
                && self.fault == Some(Fault::ExternalChangeBeforeRecovery))
            || (matches!(point, FaultPoint::ReadBack | FaultPoint::ReacquirePrimary)
                && self.fault == Some(Fault::ReadBackAndReacquire))
        {
            return Err(ErrorCode::WriteFailed.notice().to_string());
        }
        let _ = point;
        Ok(())
    }
}
fn write_io_error(error: std::io::Error) -> String {
    operation_io_error(&error, ErrorCode::WriteFailed).notice().to_string()
}

fn verification_error_code(error: &str) -> ErrorCode {
    if ErrorCode::FileTooLarge.matches_message(error) {
        ErrorCode::FileTooLarge
    } else {
        ErrorCode::WriteFailed
    }
}

fn primary_io_error(error: std::io::Error) -> String {
    operation_io_error(&error, ErrorCode::ReadFailed).notice().to_string()
}

fn cleanup_io_error(error: std::io::Error) -> CleanupFailure {
    CleanupFailure::retry(temp_cleanup_error(&error))
}

fn recover_startup_primary(
    primary: &Path,
    remnants: &[PathBuf],
    read_only: bool,
) -> Result<Option<OpenedPrimary<VaultSnapshot>>, String> {
    let mut best: Option<(_, OpenedPrimary<VaultSnapshot>)> = None;
    let mut failure = None;
    let in_use = ErrorCode::FileInUse.notice().to_string();
    for path in remnants {
        let extension = path.extension().expect("recognized remnant extension");
        let rollback = extension.eq_ignore_ascii_case("rollback");
        if !rollback && !extension.eq_ignore_ascii_case("recovery") {
            continue;
        }
        match open_primary(path, !read_only) {
            Ok(Some(opened)) => {
                let priority = (opened.decoded.revision, rollback, path);
                if best
                    .as_ref()
                    .is_none_or(|(previous, _)| priority > *previous)
                {
                    best = Some((priority, opened));
                }
            }
            Ok(None) => (),
            Err(error) if failure.is_none() || error == in_use => failure = Some(error),
            Err(_) => (),
        }
    }
    let Some((_, opened)) = best else {
        return failure.map_or(Ok(None), Err);
    };
    if read_only {
        return Ok(Some(opened));
    }
    // Install the verified source by handle, then reacquire the usual main
    // guard and verify the bytes before acknowledging the recovered snapshot.
    crate::export_file::rename_open_file_without_replace(&opened.file, primary)
        .map_err(primary_io_error)?;
    let OpenedPrimary {
        file,
        decoded,
        stamp,
    } = opened;
    drop(file);
    let changed = "資料檔已被其他程式修改，已停止保存；請重新開啟 BNAuth。";
    let mut file = platform::open_guarded(primary, false)?.ok_or(changed)?;
    if stamp_open(&mut file)? != stamp {
        return Err(changed.into());
    }
    Ok(Some(OpenedPrimary {
        file,
        decoded,
        stamp,
    }))
}

fn clean_startup_remnants(
    remnants: &[PathBuf],
    display_directory: &Path,
    write_access_denied: bool,
    primary_readable: bool,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for path in remnants {
        let extension = path.extension().expect("recognized remnant extension");
        if extension.eq_ignore_ascii_case("cleanup") {
            let _ = platform::delete_startup_remnant(path);
            continue;
        }
        if extension.eq_ignore_ascii_case("tmp") || primary_readable {
            match platform::delete_startup_remnant(path) {
                Ok(()) => (),
                Err(_) if write_access_denied => (), // Allow read-only browsing.
                Err(error) => {
                    failures.push((temp_cleanup_error(&error), path.clone()));
                }
            }
        }
    }
    cleanup_failures(failures, display_directory)
}

fn cleanup_failures(
    failures: Vec<(ErrorCode, PathBuf)>,
    display_directory: &Path,
) -> Result<(), String> {
    let mut grouped: Vec<(ErrorCode, Vec<String>)> = Vec::new();
    for (code, path) in failures {
        let path = display_directory
            .join(path.file_name().expect("transaction file name"))
            .display()
            .to_string();
        if let Some((_, paths)) = grouped.iter_mut().find(|(other, _)| *other == code) {
            paths.push(path);
        } else {
            grouped.push((code, vec![path]));
        }
    }
    let messages: Vec<_> = grouped
        .into_iter()
        .map(|(code, paths)| match code {
            ErrorCode::FileInUse => code.manual_delete(paths.join("\n")).to_string(),
            _ => code.notice().to_string(),
        })
        .collect();
    if messages.is_empty() {
        Ok(())
    } else {
        Err(messages.join("\n\n"))
    }
}

fn find_remnants(parent: &Path) -> Result<Vec<PathBuf>, String> {
    const PREFIX: &[u8] = b".BNAuth.bnauth.";
    let mut result = vec![];
    for entry in fs::read_dir(parent).map_err(primary_io_error)? {
        let entry = entry.map_err(primary_io_error)?;
        let name = entry.file_name();
        let Some((prefix, tail)) = name.as_encoded_bytes().split_at_checked(PREFIX.len()) else {
            continue;
        };
        if !prefix.eq_ignore_ascii_case(PREFIX) {
            continue;
        }
        let Some((id, extension)) = tail.split_at_checked(uuid::fmt::Hyphenated::LENGTH) else {
            continue;
        };
        if [b".tmp".as_slice(), b".rollback", b".recovery", b".cleanup"]
            .into_iter()
            .any(|suffix| extension.eq_ignore_ascii_case(suffix))
            && Uuid::try_parse_ascii(id).is_ok()
        {
            result.push(entry.path());
        }
    }
    Ok(result)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultPoint {
    BeforeWrite,
    BeforeFlush,
    AfterStage,
    BeforeReplace,
    ReadBack,
    ReacquirePrimary,
    Cleanup,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    At(FaultPoint),
    PartialWrite,
    PartialWriteAndCleanup,
    Replace1176,
    Replace1177,
    ExternalChangeBeforeReplace,
    ExternalReformatBeforeReplace,
    ExternalFirstCreate,
    TempAlreadyExists,
    TempCreate(std::io::ErrorKind),
    ExternalChangeBeforeCleanup,
    ExternalChangeBeforeRecovery,
    ReadBackAndReacquire,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        collections::HashSet,
        ffi::{OsStr, OsString},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
        sync::{Mutex, OnceLock},
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
            WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::*,
        System::{
            SystemServices::FILE_READ_ONLY_VOLUME,
            Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
        },
    };

    fn wide(value: &OsStr) -> Result<Vec<u16>, String> {
        let mut value: Vec<u16> = value.encode_wide().collect();
        if value.contains(&0) {
            return Err("路徑含無效字元。".into());
        }
        value.push(0);
        Ok(value)
    }
    fn last_read_error() -> String {
        primary_io_error(std::io::Error::last_os_error())
    }
    fn last_write_error() -> String {
        write_io_error(std::io::Error::last_os_error())
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn open_attributes(path: &Path, flags: u32) -> Result<Handle, String> {
        let path = wide(path.as_os_str())?;
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                flags,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(last_read_error())
        } else {
            Ok(Handle(handle))
        }
    }
    fn open_directory(path: &Path) -> Result<Handle, String> {
        let handle = open_attributes(path, FILE_FLAG_BACKUP_SEMANTICS)?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
            return Err(last_read_error());
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(ErrorCode::ReadFailed.notice().to_string());
        }
        Ok(handle)
    }
    pub(super) fn canonical_ntfs_directory(path: &Path) -> Result<NtfsDirectory, String> {
        let handle = open_directory(path)?;
        let mut fsname = [0u16; 32];
        let mut volume_flags = 0;
        if unsafe {
            GetVolumeInformationByHandleW(
                handle.0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut volume_flags,
                fsname.as_mut_ptr(),
                fsname.len() as u32,
            )
        } == 0
        {
            return Err(last_read_error());
        }
        let end = fsname.iter().position(|&v| v == 0).unwrap_or(fsname.len());
        if String::from_utf16_lossy(&fsname[..end]) != "NTFS" {
            return Err(ErrorCode::UnsupportedFilesystem.notice().to_string());
        }
        Ok(NtfsDirectory {
            path: volume_directory_path(&handle)?,
            read_only_volume: volume_flags & FILE_READ_ONLY_VOLUME != 0,
        })
    }
    pub(super) fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
        volume_directory_path(&open_directory(path)?)
    }
    fn volume_directory_path(handle: &Handle) -> Result<PathBuf, String> {
        let mut buffer = vec![0u16; 1024];
        loop {
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    handle.0,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    VOLUME_NAME_GUID | FILE_NAME_NORMALIZED,
                )
            };
            if length == 0 {
                return Err(last_read_error());
            }
            if length as usize >= buffer.len() {
                if length > 32768 {
                    return Err("資料檔路徑過長。".into());
                }
                buffer.resize(length as usize + 1, 0);
                continue;
            }
            buffer.truncate(length as usize);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
    }
    pub fn save_access_denied(directory: &Path, primary: &Path) -> bool {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_WRITE_PROTECT};

        let denied = |path: &Path, access, flags| {
            OpenOptions::new()
                .read(true)
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(flags)
                .open(path)
                .is_err_and(|error| {
                    matches!(
                        error.raw_os_error().map(|code| code as u32),
                        Some(ERROR_ACCESS_DENIED | ERROR_WRITE_PROTECT)
                    )
                })
        };
        // Stage a new file in the directory, then replace the existing primary. Opening for
        // DELETE lets NTFS check both the file's DELETE and its parent's FILE_DELETE_CHILD.
        denied(directory, FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS)
            || denied(primary, DELETE, FILE_FLAG_OPEN_REPARSE_POINT)
    }
    pub fn create_temp(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    pub fn open_guarded(path: &Path, delete: bool) -> Result<Option<File>, String> {
        open_guarded_io(path, delete).map_err(primary_io_error)
    }
    pub(super) fn open_guarded_io(path: &Path, delete: bool) -> std::io::Result<Option<File>> {
        open_guarded_wide(&wide(path.as_os_str()).map_err(std::io::Error::other)?, delete)
    }
    fn open_guarded_wide(path: &[u16], delete: bool) -> std::io::Result<Option<File>> {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | if delete { DELETE } else { 0 },
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            }
        } else {
            // File owns this handle; no Handle wrapper may close it a second time.
            Ok(Some(unsafe { File::from_raw_handle(handle) }))
        }
    }
    pub fn file_identity(file: &File) -> Result<FileIdentity, String> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(last_read_error());
        }
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return Err("資料檔必須是一般檔案，不能是資料夾或指向其他位置的連結。".into());
        }
        if info.nNumberOfLinks != 1 {
            return Err("這份資料檔與其他位置共用同一份內容，請先複製成獨立檔案再開啟。".into());
        }
        Ok(FileIdentity {
            volume: info.dwVolumeSerialNumber,
            index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
    pub fn delete_open(file: &File) -> std::io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub(super) fn delete_startup_remnant(path: &Path) -> std::io::Result<()> {
        if let Some(file) = open_guarded_io(path, true)? {
            file_identity(&file).map_err(std::io::Error::other)?;
            delete_open(&file)?;
        }
        Ok(())
    }
    static HELD: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    pub struct Lock {
        handle: Handle,
        name: String,
    }
    impl Lock {
        pub fn acquire(path: &Path) -> Result<Self, String> {
            let digest = Sha256::digest(path.to_string_lossy().to_lowercase().as_bytes());
            let name = format!("Global\\BNAuth.{digest:x}");
            let mut held = HELD
                .get_or_init(|| Mutex::new(HashSet::new()))
                .lock()
                .map_err(|_| "無法確認資料檔是否已被開啟。")?;
            if held.contains(&name) {
                return Err(ErrorCode::FileInUse.notice().to_string());
            }
            let wide_name = wide(OsStr::new(&name))?;
            let handle = unsafe { CreateMutexW(ptr::null(), 0, wide_name.as_ptr()) };
            if handle.is_null() {
                return Err(last_read_error());
            }
            let handle = Handle(handle);
            match unsafe { WaitForSingleObject(handle.0, 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => {}
                WAIT_TIMEOUT => return Err(ErrorCode::FileInUse.notice().to_string()),
                _ => return Err(last_read_error()),
            }
            held.insert(name.clone());
            Ok(Self { handle, name })
        }
    }
    impl Drop for Lock {
        fn drop(&mut self) {
            unsafe {
                ReleaseMutex(self.handle.0);
            }
            if let Ok(mut held) = HELD.get_or_init(|| Mutex::new(HashSet::new())).lock() {
                held.remove(&self.name);
            }
        }
    }
    pub fn move_no_replace(source: &Path, target: &Path) -> Result<(), String> {
        let source = wide(source.as_os_str())?;
        let target = wide(target.as_os_str())?;
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            Err(last_write_error())
        } else {
            Ok(())
        }
    }
    pub fn replace(target: &Path, source: &Path, backup: Option<&Path>) -> Result<(), String> {
        let source = wide(source.as_os_str())?;
        let target = wide(target.as_os_str())?;
        let backup = backup.map(|p| wide(p.as_os_str())).transpose()?;
        if unsafe {
            ReplaceFileW(
                target.as_ptr(),
                source.as_ptr(),
                backup.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
                0,
                ptr::null(),
                ptr::null(),
            )
        } == 0
        {
            Err(last_write_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct Lock;
    pub(super) fn canonical_ntfs_directory(_: &Path) -> Result<NtfsDirectory, String> {
        Err(ErrorCode::UnsupportedFilesystem.notice().to_string())
    }
    pub(super) fn canonical_directory(_: &Path) -> Result<PathBuf, String> {
        Err("不支援的平台。".into())
    }
    pub fn save_access_denied(_: &Path, _: &Path) -> bool {
        true
    }
    pub fn create_temp(_: &Path) -> std::io::Result<File> {
        Err(std::io::Error::other("不支援的平台。"))
    }
    pub fn open_guarded(_: &Path, _: bool) -> Result<Option<File>, String> {
        Err("不支援的平台。".into())
    }
    pub(super) fn open_guarded_io(_: &Path, _: bool) -> std::io::Result<Option<File>> {
        Err(std::io::Error::other("不支援的平台。"))
    }
    pub fn file_identity(_: &File) -> Result<FileIdentity, String> {
        Err("不支援的平台。".into())
    }
    pub fn delete_open(_: &File) -> std::io::Result<()> {
        Err(std::io::Error::other("不支援的平台。"))
    }
    pub(super) fn delete_startup_remnant(_: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("不支援的平台。"))
    }
    impl Lock {
        pub fn acquire(_: &Path) -> Result<Self, String> {
            Err("不支援的平台。".into())
        }
    }
    pub fn move_no_replace(_: &Path, _: &Path) -> Result<(), String> {
        Err("不支援的平台。".into())
    }
    pub fn replace(_: &Path, _: &Path, _: Option<&Path>) -> Result<(), String> {
        Err("不支援的平台。".into())
    }
}

#[cfg(all(test, windows))]
#[path = "storage_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "storage_restore_tests.rs"]
mod restore_tests;
