//! Construct a complete restore candidate without changing either input.
use crate::{Provenance, Uuid, VaultSnapshot};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct FullRestorePreview {
    pub candidate: VaultSnapshot,
    /// The acknowledged target identity and revision used to build this preview.
    /// `None` means the fixed working file must still be absent at confirmation.
    pub base: Option<(Uuid, u64)>,
    pub new_secret: bool,
}

pub fn prepare_full_restore(
    target: Option<&VaultSnapshot>,
    source: &VaultSnapshot,
    timestamp_ms: i64,
) -> Result<FullRestorePreview, String> {
    source.validate()?;
    if let Some(target) = target {
        target.validate()?;
        target
            .revision
            .checked_add(1)
            .ok_or("資料庫修訂號已達上限，無法保存還原結果。")?;
    }
    if timestamp_ms < 0 {
        return Err("系統時間早於Unix epoch，請先校正電腦時間。".into());
    }
    if source.provenance.len() >= 1_000 {
        return Err("來源紀錄已達1000筆上限，無法完整保留並加入本次還原紀錄。".into());
    }

    let mut candidate = source.clone();
    let occupied: HashSet<_> = std::iter::once(source.vault_id)
        .chain(source.groups.iter().map(|group| group.id))
        .chain(source.authenticators.iter().map(|record| record.id))
        .collect();
    if let Some(target) = target {
        if source
            .groups
            .iter()
            .any(|group| group.id == target.vault_id)
            || source
                .authenticators
                .iter()
                .any(|record| record.id == target.vault_id)
        {
            return Err(
                "來源分組或驗證器的識別碼與目前資料庫識別碼衝突，無法保留原識別碼；請先核對來源。"
                    .into(),
            );
        }
        candidate.vault_id = target.vault_id;
        candidate.revision = target.revision;
        candidate.created_at_ms = target.created_at_ms;
    } else {
        candidate.vault_id = loop {
            let id = Uuid::new_v4();
            if !occupied.contains(&id) {
                break id;
            }
        };
        candidate.revision = 0;
        candidate.created_at_ms = timestamp_ms;
    }
    candidate.updated_at_ms = timestamp_ms;
    candidate.provenance.push(Provenance {
        source_vault_id: source.vault_id,
        source_revision: source.revision,
        scope: "full".into(),
        imported_at_ms: timestamp_ms,
        source_time_sync: source.service_time_sync.battlenet.clone(),
    });
    candidate.validate()?;
    let new_secret = source.authenticators.iter().any(|record| {
        record.secret.as_ref().is_some_and(|secret| {
            !target.is_some_and(|target| {
                target
                    .authenticators
                    .iter()
                    .any(|existing| existing.secret.as_ref() == Some(secret))
            })
        })
    });
    Ok(FullRestorePreview {
        candidate,
        base: target.map(|target| (target.vault_id, target.revision)),
        new_secret,
    })
}

#[cfg(test)]
#[path = "full_restore_tests.rs"]
mod tests;
