use bnauth_core::Authenticator;
use std::collections::HashMap;
use uuid::Uuid;

/// Codes are valid for one shared 30-second time step. A clock correction can
/// move the step in either direction; loading a snapshot clears record identity.
#[derive(Default)]
pub struct OtpCache {
    step: Option<i64>,
    codes: HashMap<Uuid, String>,
    #[cfg(test)]
    pub calculations: usize,
}

impl OtpCache {
    pub fn clear(&mut self) {
        self.step = None;
        self.codes.clear();
    }

    pub fn code(&mut self, record: &Authenticator, now_ms: i64) -> String {
        let step = now_ms.div_euclid(30_000);
        if self.step != Some(step) {
            self.codes.clear();
            self.step = Some(step);
        }
        self.codes
            .entry(record.id)
            .or_insert_with(|| {
                #[cfg(test)]
                {
                    self.calculations += 1;
                }
                record
                    .secret
                    .as_ref()
                    .and_then(|secret| bnauth_core::totp(secret, now_ms).ok())
                    .unwrap_or_default()
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Authenticator {
        let vault = bnauth_core::VaultSnapshot::new_empty(1_800_000_000_000);
        bnauth_core::prepare_import(
            &vault,
            "人工測試",
            "",
            vault.groups[0].id,
            bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
            1_800_000_000_000,
        )
        .unwrap()
        .candidate
        .authenticators
        .remove(0)
    }

    #[test]
    fn computes_once_per_record_and_refreshes_for_forward_and_backward_clock_steps() {
        let mut cache = OtpCache::default();
        let record = record();
        let start = 1_800_000_000_000;
        let expected = |time| bnauth_core::totp(record.secret.as_ref().unwrap(), time).unwrap();
        assert_eq!(cache.code(&record, start), expected(start));
        assert_eq!(cache.code(&record, start + 29_999), expected(start));
        assert_eq!(cache.calculations, 1);
        assert_eq!(
            cache.code(&record, start + 30_000),
            expected(start + 30_000)
        );
        assert_eq!(cache.calculations, 2);
        assert_eq!(cache.code(&record, start - 1), expected(start - 1));
        assert_eq!(cache.calculations, 3);
        assert_eq!(cache.codes.len(), 1);
    }

    #[test]
    fn loading_new_snapshot_invalidates_codes_even_with_reused_identity() {
        let mut cache = OtpCache::default();
        let mut record = record();
        let now = 1_800_000_000_000;
        assert_eq!(cache.code(&record, now).len(), 8);
        record.secret = None;
        cache.clear();
        assert!(cache.code(&record, now).is_empty());
        assert_eq!(cache.calculations, 2);
    }
}
