//! Durable single-writer repository. All methods and Drop stay on its worker thread.
use crate::native;
use bnauth_core::{VaultSnapshot, now_ms};
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
#[derive(Clone, Debug)]
struct OwnedFile {
    path: PathBuf,
    stamp: FileStamp,
}
struct CleanupFailure {
    message: String,
    changed: bool,
}
impl CleanupFailure {
    fn retry(message: String) -> Self {
        Self {
            message,
            changed: false,
        }
    }
    fn changed(message: String) -> Self {
        Self {
            message,
            changed: true,
        }
    }
}

// The caller holds a handle that denies outside writes and renames throughout
// identity capture and bounded reading. Partial staged files need no JSON decode.
fn inspect_open(file: &mut File) -> Result<(Zeroizing<Vec<u8>>, FileStamp), String> {
    let identity = platform::file_identity(file)?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(native::MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > native::MAX_FILE {
        return Err("資料檔過大，超過 32 MiB 上限。".into());
    }
    let hash = Sha256::digest(&*bytes).into();
    Ok((bytes, FileStamp { identity, hash }))
}
struct OpenedPrimary {
    file: File,
    snapshot: VaultSnapshot,
    stamp: FileStamp,
}

fn open_primary(path: &Path) -> Result<Option<OpenedPrimary>, String> {
    let Some(mut file) = platform::open_guarded(path, false)? else {
        return Ok(None);
    };
    let (bytes, stamp) = inspect_open(&mut file)?;
    Ok(Some(OpenedPrimary {
        file,
        snapshot: native::decode(&bytes)?,
        stamp,
    }))
}

fn read_locked(path: &Path) -> Result<Option<(VaultSnapshot, FileStamp)>, String> {
    Ok(open_primary(path)?.map(|opened| (opened.snapshot, opened.stamp)))
}

fn read_stamp_locked(path: &Path) -> Result<Option<FileStamp>, String> {
    let Some(mut file) = platform::open_guarded(path, false)? else {
        return Ok(None);
    };
    let (_, stamp) = inspect_open(&mut file)?;
    Ok(Some(stamp))
}

fn delete_owned(owned: &OwnedFile, inject_failure: bool) -> Result<(), CleanupFailure> {
    let Some(mut file) =
        platform::open_guarded(&owned.path, true).map_err(CleanupFailure::retry)?
    else {
        return Ok(());
    };
    let (bytes, actual) = inspect_open(&mut file).map_err(CleanupFailure::retry)?;
    drop(bytes);
    if actual != owned.stamp {
        return Err(CleanupFailure::changed(format!(
            "暫存檔已被外部修改或替換，已保留在原位置，不會刪除：{}",
            owned.path.display()
        )));
    }
    if inject_failure {
        return Err(CleanupFailure::retry("模擬清理失敗。".into()));
    }
    // Keep this exact verified handle open through disposition; a path-based
    // delete after closing it would reintroduce a check/delete race.
    platform::delete_open(&file).map_err(CleanupFailure::retry)
}

pub struct Repository {
    path: PathBuf,
    original_parent: PathBuf,
    canonical_parent: PathBuf,
    current: Option<VaultSnapshot>,
    fingerprint: Option<FileStamp>,
    blocked: bool,
    cleanup_pending: Vec<OwnedFile>,
    restore_copy: Option<(FileStamp, OwnedFile)>,
    last_restore_backup: Option<PathBuf>,
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
        let canonical_parent = platform::canonical_ntfs_directory(&original_parent)?;
        let path = canonical_parent.join("BNAuth.bnauth");
        let lock = platform::Lock::acquire(&path)?;
        if platform::canonical_ntfs_directory(&original_parent)? != canonical_parent {
            return Err("開啟資料檔時，所在的資料夾已變更。請重新開啟 BNAuth。".into());
        }
        let remnants = find_remnants(&canonical_parent)?;
        if !remnants.is_empty() {
            let names = remnants
                .iter()
                .map(|p| {
                    let status = read_locked(p)
                        .ok()
                        .flatten()
                        .map(|(v, _)| format!("資料完整且可讀取，資料版本 {}", v.revision))
                        .unwrap_or_else(|| "無法確認資料是否完整".into());
                    format!("{}（{status}）", p.display())
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!(
                "發現上次保存留下的檔案，無法確認上次是否保存完成，已停止保存。請保留並檢查下列檔案，程式不會自動使用或刪除它們：\n{names}"
            ));
        }
        let (current, fingerprint, primary_guard) = match open_primary(&path)? {
            Some(opened) => (Some(opened.snapshot), Some(opened.stamp), Some(opened.file)),
            None => (None, None, None),
        };
        let loaded = current.clone();
        Ok((
            Self {
                path,
                original_parent,
                canonical_parent,
                current,
                fingerprint,
                blocked: false,
                cleanup_pending: vec![],
                restore_copy: None,
                last_restore_backup: None,
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
    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    pub fn restore_backup(&self) -> Option<&Path> {
        self.last_restore_backup.as_deref()
    }

    /// A replacement restore must first preserve a verified, independent copy
    /// of the last acknowledged primary. This copy is never transaction residue.
    pub fn restore(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.validate_candidate(candidate)?;
        self.check_current()?;
        let backup = self.prepare_restore_copy()?;
        // Keep the verified copy protected from outside writes/deletion until
        // the ordinary commit has either succeeded or retained its candidate.
        let result = self.commit_validated(candidate);
        if result.is_ok() {
            // IO remains bound to the verified volume path. Report the same
            // filename through the original, user-readable directory spelling.
            self.last_restore_backup = backup.as_ref().map(|(_, path)| {
                self.original_parent.join(
                    path.file_name()
                        .expect("generated safety copy has a filename"),
                )
            });
        }
        drop(backup);
        result
    }

    fn prepare_restore_copy(&mut self) -> Result<Option<(File, PathBuf)>, String> {
        let Some(expected) = self.fingerprint else {
            return Ok(None);
        };
        if let Some((source, owned)) = &self.restore_copy
            && *source == expected
        {
            let mut file = platform::open_guarded(&owned.path, false)?
                .ok_or("還原前副本已不存在，未取代目前資料；請保留檔案並重新開啟程式。")?;
            // This stamp identifies the same bytes already decoded and validated
            // as the acknowledged primary; the held handle protects the copy.
            let (_, stamp) = inspect_open(&mut file)?;
            if stamp != owned.stamp {
                return Err(
                    "還原前副本已被外部修改，未取代目前資料；請保留檔案並重新開啟程式。".into(),
                );
            }
            return Ok(Some((file, owned.path.clone())));
        }
        let mut original =
            platform::open_guarded(&self.path, false)?.ok_or("目前資料檔已不存在，未開始還原。")?;
        let (bytes, stamp) = inspect_open(&mut original)?;
        if stamp != expected {
            return Err("目前資料檔與已載入資料不同，未開始還原。".into());
        }
        self.inject(FaultPoint::BeforeRestoreCopy)?;
        let path = self.canonical_parent.join(format!(
            "BNAuth-before-restore-{}-{}.bnauth",
            now_ms(),
            Uuid::new_v4()
        ));
        let mut copy = platform::create_temp(&path)
            .map_err(|error| format!("無法建立還原前副本，未取代目前資料（{error}）。"))?;
        let verified: Result<FileStamp, String> = (|| {
            copy.write_all(&bytes).map_err(io_error)?;
            self.inject(FaultPoint::RestoreCopyFlush)?;
            copy.sync_all().map_err(io_error)?;
            let (actual, stamp) = inspect_open(&mut copy)?;
            // Exact readback equality preserves the validated source without
            // allocating and validating another full snapshot.
            if actual.as_slice() != bytes.as_slice() {
                return Err("還原前副本讀回核對失敗。".into());
            }
            Ok(stamp)
        })();
        let stamp = match verified {
            Ok(stamp) => stamp,
            Err(error) => {
                let cleanup = platform::delete_open(&copy);
                return Err(format!(
                    "無法完成還原前副本，未取代目前資料：{error}{}",
                    cleanup
                        .err()
                        .map(|error| format!("\n副本尚未移除：{}（{error}）", path.display()))
                        .unwrap_or_default()
                ));
            }
        };
        self.restore_copy = Some((
            expected,
            OwnedFile {
                path: path.clone(),
                stamp,
            },
        ));
        Ok(Some((copy, path)))
    }

    /// Candidate uses the last committed revision. This method owns revision advancement.
    pub fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.validate_candidate(candidate)?;
        self.commit_validated(candidate)
    }

    fn validate_candidate(&self, candidate: &VaultSnapshot) -> Result<(), String> {
        if self.blocked {
            return Err("資料檔狀態需要檢查，已停止保存。請保留檔案並重新開啟 BNAuth。".into());
        }
        if !self.cleanup_pending.is_empty() {
            return Err(
                "上次保存留下的暫存檔尚未移除，請先按「重試移除暫存檔」；目前未再次保存資料。"
                    .into(),
            );
        }
        candidate.validate()?;
        match &self.current {
            Some(old) => {
                if candidate.vault_id != old.vault_id
                    || candidate.revision != old.revision
                    || candidate.created_at_ms != old.created_at_ms
                {
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
            .ok_or("資料檔的保存次數已達上限，無法繼續保存。")?;
        Ok(())
    }

    fn commit_validated(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.check_current()?;
        let mut next = candidate.clone();
        next.revision = candidate
            .revision
            .checked_add(1)
            .ok_or("資料檔的保存次數已達上限，無法繼續保存。")?;
        next.updated_at_ms = now_ms().max(next.updated_at_ms).max(next.created_at_ms);
        let encoded = native::encode_validated(&next)?;
        let expected: [u8; 32] = Sha256::digest(&*encoded).into();
        let transaction = Uuid::new_v4();
        let temp = self
            .canonical_parent
            .join(format!(".BNAuth.bnauth.{transaction}.tmp"));
        let rollback = self
            .canonical_parent
            .join(format!(".BNAuth.bnauth.{transaction}.rollback"));
        let previous = self.fingerprint;
        let mut staged: Option<OwnedFile> = None;

        let stage_result = (|| -> Result<(), String> {
            self.inject(FaultPoint::BeforeWrite)?;
            #[cfg(test)]
            if self.fault == Some(Fault::TempAlreadyExists) {
                fs::write(&temp, b"external pre-existing temp").map_err(io_error)?;
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
                    format!(
                        "{}\n下列暫存檔不是本次保存建立的，已保留，不會刪除：{}",
                        io_error(error),
                        temp.display()
                    )
                } else {
                    format!("無法建立保存所需的暫存檔。\n詳細原因：{error}")
                }
            })?;
            let write_result = (|| -> Result<(), String> {
                #[cfg(test)]
                if matches!(
                    self.fault,
                    Some(Fault::PartialWrite | Fault::PartialWriteAndCleanup)
                ) {
                    file.write_all(&encoded[..encoded.len() / 2])
                        .map_err(io_error)?;
                    return Err("模擬部分寫入失敗。".into());
                }
                file.write_all(&encoded).map_err(io_error)?;
                self.inject(FaultPoint::BeforeFlush)?;
                file.sync_all().map_err(io_error)
            })();
            // Capture even partial bytes while our original creation handle still
            // excludes outside writers. Never infer ownership from a filename.
            let (_, stamp) = inspect_open(&mut file).map_err(|error| {
                self.blocked = true;
                format!(
                    "{error}\n無法確認本次保存的暫存檔狀態，請保留檔案以供檢查：{}",
                    temp.display()
                )
            })?;
            staged = Some(OwnedFile {
                path: temp.clone(),
                stamp,
            });
            drop(file);
            write_result?;
            // The expected hash identifies the encoded, validated candidate.
            if stamp.hash != expected {
                return Err("尚未保存：暫存檔內容與預期不符。".into());
            }
            self.inject(FaultPoint::AfterStage)?;
            self.check_current()?;
            self.inject(FaultPoint::BeforeReplace)?;
            Ok(())
        })();
        if let Err(error) = stage_result {
            return Err(match &staged {
                Some(owned) => self.discard_uncommitted(owned, error),
                None => error,
            });
        }
        let staged = staged.expect("successful stage owns its created file");
        // ReplaceFileW needs DELETE access even in this process. Keep the main
        // guard throughout staging, then release it only for the atomic install.
        drop(self.primary_guard.take());
        let replace_result = self.install(&temp, &rollback);
        // Verification and the new lifetime guard are the same open handle;
        // there is no verified-but-unlocked success state to acknowledge.
        let opened = if self.inject(FaultPoint::ReadBack).is_ok() {
            self.inject(FaultPoint::ReacquirePrimary)
                .and_then(|()| open_primary(&self.path))
        } else {
            Ok(None)
        };
        let verified = match opened {
            Ok(Some(opened)) if opened.stamp == staged.stamp && opened.snapshot == next => {
                self.primary_guard = Some(opened.file);
                true
            }
            Ok(_) => false,
            Err(error) => {
                return self.block_for_recovery(
                    &temp,
                    &rollback,
                    &format!("更新資料檔後，無法確認內容或恢復檔案保護：{error}"),
                );
            }
        };
        let rollback_owned = previous.map(|stamp| OwnedFile {
            path: rollback.clone(),
            stamp,
        });
        let mut rollback_verified = previous.is_none();
        if let Some(old_stamp) = previous {
            match read_stamp_locked(&rollback) {
                Ok(Some(stamp)) if stamp == old_stamp => rollback_verified = true,
                Ok(None) => (),
                _ => return self.block_for_recovery(&temp, &rollback,
                    "保存期間原資料檔被外部修改，或無法確認原資料備份是否完整；已保留所有檔案，沒有刪除原資料備份。"),
            }
        }
        // API status alone is insufficient. The held, verified candidate and
        // verified previous-file rollback are the commit point.
        if verified {
            if !rollback_verified {
                return self.block_for_recovery(
                    &temp,
                    &rollback,
                    "新的資料檔已寫入，但找不到原資料備份，無法確認保存期間是否有外部修改。",
                );
            }
            self.current = Some(next.clone());
            self.fingerprint = Some(staged.stamp);
            let mut warnings = Vec::new();
            if let Err(error) = replace_result {
                warnings.push(format!(
                    "資料已保存，但保存過程曾發生異常。\n詳細原因：{error}"
                ));
            }
            for owned in std::iter::once(&staged).chain(rollback_owned.iter()) {
                if let Err(error) = self.cleanup(owned) {
                    warnings.push(format!(
                        "資料已保存，部分暫存檔尚未移除。\n詳細原因：{error}"
                    ));
                }
                if self.blocked {
                    break;
                }
            }
            return Ok(CommitOutcome {
                snapshot: next,
                warning: (!warnings.is_empty()).then(|| warnings.join("\n")),
            });
        }
        let reason = replace_result
            .err()
            .unwrap_or_else(|| "更新資料檔後，無法確認內容是否正確。".into());
        self.recover_failed_commit(&staged, rollback_owned.as_ref(), &rollback, reason)
    }

    pub fn retry_cleanup(&mut self) -> Result<(), String> {
        if self.blocked {
            return Err(
                "檔案已被外部修改或狀態無法確認，已停止移除暫存檔；請保留檔案以供檢查。".into(),
            );
        }
        let pending = std::mem::take(&mut self.cleanup_pending);
        let mut errors = Vec::new();
        for owned in pending {
            if self.blocked {
                self.cleanup_pending.push(owned);
                continue;
            }
            if let Err(error) = self.cleanup(&owned) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "部分暫存檔仍無法移除。\n詳細原因：\n{}",
                errors.join("\n")
            ))
        }
    }

    fn reacquire_current(&mut self) -> Result<(), String> {
        self.inject(FaultPoint::ReacquirePrimary)?;
        match (self.fingerprint, open_primary(&self.path)?) {
            (Some(expected), Some(opened))
                if opened.stamp == expected && Some(&opened.snapshot) == self.current.as_ref() =>
            {
                self.primary_guard = Some(opened.file);
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err("恢復後的資料檔與上次確認的內容不符，無法恢復檔案保護。".into()),
        }
    }

    fn check_current(&self) -> Result<(), String> {
        if platform::canonical_ntfs_directory(&self.original_parent)? != self.canonical_parent {
            return Err("資料檔所在的資料夾已變更，已停止保存。".into());
        }
        // The expected fingerprint belongs to already decoded, validated bytes.
        // Matching identity and hash proves those same bytes are still present.
        match (self.fingerprint, read_stamp_locked(&self.path)?) {
            (Some(expected), Some(stamp)) if stamp == expected => Ok(()),
            (None, None) => Ok(()),
            (None, Some(_)) => {
                Err("資料檔已由其他程式建立，已停止保存；請重新開啟 BNAuth。".into())
            }
            _ => Err("資料檔已被其他程式修改，已停止保存；請重新開啟 BNAuth。".into()),
        }
    }
    fn install(&self, temp: &Path, rollback: &Path) -> Result<(), String> {
        #[cfg(test)]
        match self.fault {
            Some(Fault::Replace1176) => return Err("模擬Win32錯誤1176。".into()),
            Some(Fault::Replace1177) => {
                platform::move_no_replace(&self.path, rollback)?;
                return Err("模擬Win32錯誤1177。".into());
            }
            Some(Fault::ExternalChangeBeforeReplace) => {
                let mut external = self.current.as_ref().unwrap().clone();
                external.revision += 1;
                external.groups.push(bnauth_core::Group {
                    id: Uuid::new_v4(),
                    name: "外部修改".into(),
                    sort_order: external.groups.len() as u32,
                    is_ungrouped: false,
                });
                fs::write(&self.path, &*native::encode(&external)?).map_err(io_error)?;
            }
            Some(Fault::ExternalReformatBeforeReplace) => {
                let compact = serde_json::to_vec(self.current.as_ref().unwrap()).unwrap();
                fs::write(&self.path, compact).map_err(io_error)?;
            }
            Some(Fault::ExternalFirstCreate) => {
                fs::write(&self.path, b"external first-create content").map_err(io_error)?;
            }
            _ => (),
        }
        if self.current.is_some() {
            platform::replace(&self.path, temp, Some(rollback))
        } else {
            platform::move_no_replace(temp, &self.path)
        }
    }

    fn cleanup(&mut self, owned: &OwnedFile) -> Result<(), String> {
        #[cfg(test)]
        if self.fault == Some(Fault::ExternalChangeBeforeCleanup)
            && owned.path.extension().is_some_and(|s| s == "rollback")
        {
            fs::write(&owned.path, b"external cleanup content").map_err(io_error)?;
        }
        let result = delete_owned(owned, self.inject(FaultPoint::Cleanup).is_err());
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.blocked |= error.changed;
                self.cleanup_pending.push(owned.clone());
                Err(format!("{}\n{}", error.message, owned.path.display()))
            }
        }
    }
    fn discard_uncommitted(&mut self, owned: &OwnedFile, reason: String) -> String {
        match self.cleanup(owned) {
            Ok(()) => reason,
            Err(error) => {
                format!("{reason}\n這次變更尚未保存，部分暫存檔尚未移除。\n詳細原因：{error}")
            }
        }
    }
    fn block_for_recovery(
        &mut self,
        temp: &Path,
        rollback: &Path,
        reason: &str,
    ) -> Result<CommitOutcome, String> {
        self.blocked = true;
        Err(format!(
            "{reason}\n無法確認這次保存的結果，已停止保存。請保留以下位置的檔案，不要刪除或覆蓋：\n{}\n{}\n{}\n{}",
            self.path.display(),
            temp.display(),
            rollback.display(),
            rollback.with_extension("recovery").display()
        ))
    }
    fn recover_failed_commit(
        &mut self,
        staged: &OwnedFile,
        rollback: Option<&OwnedFile>,
        rollback_path: &Path,
        reason: String,
    ) -> Result<CommitOutcome, String> {
        // Recovery may rename/delete the attempted primary. Never let our own
        // deny-delete guard obstruct restoring the acknowledged old snapshot.
        drop(self.primary_guard.take());
        // Expected stamps identify already validated bytes. Reacquiring the
        // acknowledged primary below still validates its full contents and guard.
        let actual = read_stamp_locked(&self.path);
        let actual_stamp = actual.as_ref().ok().copied().flatten();
        let main_missing = matches!(actual, Ok(None));
        let rollback_stamp = read_stamp_locked(rollback_path).ok().flatten();
        let mut displaced: Option<OwnedFile> = None;
        let restored = match self.fingerprint {
            Some(old_stamp) if actual_stamp == Some(old_stamp) => true,
            Some(old_stamp) if rollback_stamp == Some(old_stamp) => {
                let can_restore = if main_missing {
                    true
                } else if actual_stamp == Some(staged.stamp) {
                    #[cfg(test)]
                    if self.fault == Some(Fault::ExternalChangeBeforeRecovery) {
                        fs::write(&self.path, b"external recovery content").map_err(io_error)?;
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
                can_restore
                    && platform::move_no_replace(rollback_path, &self.path).is_ok()
                    && read_stamp_locked(&self.path)
                        .ok()
                        .flatten()
                        .is_some_and(|stamp| stamp == old_stamp)
            }
            None if main_missing => true,
            None if actual_stamp == Some(staged.stamp) => {
                #[cfg(test)]
                if self.fault == Some(Fault::ExternalChangeBeforeRecovery) {
                    fs::write(&self.path, b"external recovery content").map_err(io_error)?;
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
        if !restored {
            return self.block_for_recovery(
                &staged.path,
                rollback_path,
                &format!("{reason}\n無法確認原資料是否已恢復，已停止保存。"),
            );
        }
        if let Err(error) = self.reacquire_current() {
            return self.block_for_recovery(
                &staged.path,
                rollback_path,
                &format!("{reason}\n原資料已恢復，但無法確認內容或恢復檔案保護：{error}"),
            );
        }
        let mut error = format!("{reason}\n這次變更尚未保存，原有資料已確認保留，可重試保存。");
        for owned in std::iter::once(staged)
            .chain(rollback)
            .chain(displaced.as_ref())
        {
            error = self.discard_uncommitted(owned, error);
            if self.blocked {
                break;
            }
        }
        Err(error)
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
            return Err(format!("模擬保存故障：{point:?}"));
        }
        let _ = point;
        Ok(())
    }
}
fn io_error(error: std::io::Error) -> String {
    format!("檔案操作失敗（{error}）。")
}
fn find_remnants(parent: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = vec![];
    for entry in fs::read_dir(parent).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry
            .file_name()
            .to_string_lossy()
            .to_lowercase()
            .starts_with(".bnauth.bnauth.")
        {
            result.push(entry.path());
        }
    }
    Ok(result)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultPoint {
    BeforeRestoreCopy,
    RestoreCopyFlush,
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
            CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
            WAIT_ABANDONED, WAIT_OBJECT_0,
        },
        Storage::FileSystem::*,
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    };

    fn wide(value: &OsStr) -> Result<Vec<u16>, String> {
        let mut value: Vec<u16> = value.encode_wide().collect();
        if value.contains(&0) {
            return Err("路徑含無效字元。".into());
        }
        value.push(0);
        Ok(value)
    }
    fn last_error(action: &str) -> String {
        format!("{action}失敗（Windows 錯誤碼 {}）。", unsafe {
            GetLastError()
        })
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
            Err(last_error("讀取檔案資訊"))
        } else {
            Ok(Handle(handle))
        }
    }
    pub fn canonical_ntfs_directory(path: &Path) -> Result<PathBuf, String> {
        let handle = open_attributes(path, FILE_FLAG_BACKUP_SEMANTICS)?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err("資料檔所在的資料夾無法使用。".into());
        }
        let mut fsname = [0u16; 32];
        if unsafe {
            GetVolumeInformationByHandleW(
                handle.0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                fsname.as_mut_ptr(),
                fsname.len() as u32,
            )
        } == 0
        {
            return Err(last_error("確認磁碟格式"));
        }
        let end = fsname.iter().position(|&v| v == 0).unwrap_or(fsname.len());
        if String::from_utf16_lossy(&fsname[..end]) != "NTFS" {
            return Err(
                "目前只支援在 NTFS 格式的磁碟保存，請將整個資料夾移到本機的 NTFS 磁碟。".into(),
            );
        }
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
                return Err(last_error("確認資料檔所在的資料夾"));
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
        let path = wide(path.as_os_str())?;
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
                Err(io_error(error))
            }
        } else {
            // File owns this handle; no Handle wrapper may close it a second time.
            Ok(Some(unsafe { File::from_raw_handle(handle) }))
        }
    }
    pub fn file_identity(file: &File) -> Result<FileIdentity, String> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(last_error("檢查資料檔"));
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
    pub fn delete_open(file: &File) -> Result<(), String> {
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
            Err(last_error("移除暫存檔"))
        } else {
            Ok(())
        }
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
                return Err("此資料檔已在使用中，請關閉另一個 BNAuth 視窗。".into());
            }
            let wide_name = wide(OsStr::new(&name))?;
            let handle = unsafe { CreateMutexW(ptr::null(), 0, wide_name.as_ptr()) };
            if handle.is_null() {
                return Err(last_error("防止多個 BNAuth 同時修改資料檔"));
            }
            let handle = Handle(handle);
            let result = unsafe { WaitForSingleObject(handle.0, 0) };
            if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
                return Err("此資料檔已在使用中，或無法防止其他程式修改，已停止保存。".into());
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
            Err(last_error("建立或恢復資料檔"))
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
            Err(last_error("更新資料檔"))
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub struct Lock;
    pub fn canonical_ntfs_directory(_: &Path) -> Result<PathBuf, String> {
        Err("目前只支援在 Windows 的 NTFS 格式磁碟保存資料。".into())
    }
    pub fn create_temp(_: &Path) -> std::io::Result<File> {
        Err(std::io::Error::other("不支援的平台。"))
    }
    pub fn open_guarded(_: &Path, _: bool) -> Result<Option<File>, String> {
        Err("不支援的平台。".into())
    }
    pub fn file_identity(_: &File) -> Result<FileIdentity, String> {
        Err("不支援的平台。".into())
    }
    pub fn delete_open(_: &File) -> Result<(), String> {
        Err("不支援的平台。".into())
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
