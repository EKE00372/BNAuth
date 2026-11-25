//! Construct a complete restore candidate from a backup snapshot.
use crate::{Uuid, ValidatedBackup, VaultSnapshot};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct FullRestoreTarget {
    vault_id: Uuid,
    revision: u64,
}

impl From<&VaultSnapshot> for FullRestoreTarget {
    fn from(snapshot: &VaultSnapshot) -> Self {
        Self {
            vault_id: snapshot.vault_id,
            revision: snapshot.revision,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FullRestorePreview {
    pub candidate: VaultSnapshot,
    pub group_counts: HashMap<Uuid, usize>,
    /// The acknowledged target identity and revision used to build this preview.
    /// `None` means the fixed working file must still be absent at confirmation.
    pub base: Option<(Uuid, u64)>,
}

pub fn prepare_full_restore(
    target: Option<&VaultSnapshot>,
    source: &VaultSnapshot,
) -> Result<FullRestorePreview, String> {
    let source = ValidatedBackup::new(source.clone(), false)?;
    if let Some(target) = target {
        target.validate()?;
    }
    prepare_full_restore_from_validated(target.map(FullRestoreTarget::from), source)
}

/// Reuse a validated full backup as the candidate for an acknowledged vault.
pub fn prepare_full_restore_from_validated(
    target: Option<FullRestoreTarget>,
    source: ValidatedBackup,
) -> Result<FullRestorePreview, String> {
    let ValidatedBackup {
        snapshot: mut candidate,
        index,
    } = source;
    let (vault_id, revision) = if let Some(target) = target {
        target
            .revision
            .checked_add(1)
            .ok_or("資料庫修訂號已達上限，無法保存還原結果。")?;
        if target.vault_id != candidate.vault_id && index.ids.contains(&target.vault_id) {
            return Err(
                "來源分組或驗證器的識別碼與目前資料庫識別碼衝突，無法保留原識別碼；請先核對來源。"
                    .into(),
            );
        }
        (target.vault_id, target.revision)
    } else {
        loop {
            let id = Uuid::new_v4();
            if !index.ids.contains(&id) {
                break (id, 0);
            }
        }
    };
    candidate.vault_id = vault_id;
    candidate.revision = revision;
    Ok(FullRestorePreview {
        candidate,
        group_counts: index
            .group_orders
            .into_iter()
            .map(|(id, rows)| (id, rows.len()))
            .collect(),
        base: target.map(|target| (target.vault_id, target.revision)),
    })
}

#[cfg(test)]
#[path = "full_restore_tests.rs"]
mod tests;
