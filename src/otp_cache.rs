use bnauth_core::{Authenticator, ErrorCode, Secret};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct DisplayCode {
    pub digits: String,
    pub formatted: String,
}

impl DisplayCode {
    fn new(secret: &Secret, now_ms: i64) -> Self {
        let digits = bnauth_core::totp(secret, now_ms).unwrap_or_default();
        let formatted = if digits.len() == 8 {
            format!("{} {}", &digits[..4], &digits[4..])
        } else {
            ErrorCode::TimeError.notice().to_string()
        };
        Self { digits, formatted }
    }
}

/// A single draft's code, kept only for the current 30-second step.
#[derive(Default)]
pub(crate) struct PreviewCode(Option<(i64, DisplayCode)>);

impl PreviewCode {
    pub(crate) fn at(&mut self, secret: &Secret, now_ms: i64) -> &DisplayCode {
        let step = now_ms.div_euclid(30_000);
        if self
            .0
            .as_ref()
            .is_none_or(|(cached_step, _)| *cached_step != step)
        {
            self.0 = Some((step, DisplayCode::new(secret, now_ms)));
        }
        &self.0.as_ref().expect("preview code cached").1
    }
}

/// Codes are valid for one shared 30-second time step. A clock correction can
/// move the step in either direction; loading a snapshot clears record identity.
#[derive(Default)]
pub struct OtpCache {
    step: Option<i64>,
    codes: HashMap<Secret, Arc<DisplayCode>>,
    #[cfg(test)]
    pub calculations: usize,
    #[cfg(test)]
    pub clears: usize,
}

impl OtpCache {
    pub fn clear(&mut self) {
        #[cfg(test)]
        {
            self.clears += 1;
        }
        self.step = None;
        self.codes.clear();
    }

    #[cfg(test)]
    pub fn code(&mut self, record: &Authenticator, now_ms: i64) -> String {
        self.display(record, now_ms).digits.clone()
    }

    pub(crate) fn display(&mut self, record: &Authenticator, now_ms: i64) -> Arc<DisplayCode> {
        let step = now_ms.div_euclid(30_000);
        if self.step != Some(step) {
            self.codes.clear();
            self.step = Some(step);
        }
        if let Some(code) = self.codes.get(&record.secret) {
            return Arc::clone(code);
        }
        #[cfg(test)]
        {
            self.calculations += 1;
        }
        let code = Arc::new(DisplayCode::new(&record.secret, now_ms));
        self.codes.insert(record.secret.clone(), Arc::clone(&code));
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Authenticator {
        let vault = bnauth_core::VaultSnapshot::new_empty();
        bnauth_core::prepare_import(
            &vault,
            "人工測試",
            "",
            vault.groups[0].id,
            bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
        )
        .unwrap()
        .record()
    }

    #[test]
    fn computes_once_per_record_and_refreshes_for_forward_and_backward_clock_steps() {
        let mut cache = OtpCache::default();
        let record = record();
        let start = 1_800_000_000_000;
        let expected = |time| bnauth_core::totp(&record.secret, time).unwrap();
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

}
