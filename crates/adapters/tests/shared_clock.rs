use bnauth_adapters::{
    export_file::{self, Format},
    native,
    storage::Repository,
    winauth2,
};
use bnauth_core::{
    Group, TIME_SOURCE, TimeSyncRecord, Uuid, VaultSnapshot,
    clock::{RuntimeClock, system_now_ms},
    parse_manual, prepare_import,
};
use std::{fs, path::PathBuf};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-shared-clock-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn fixture(offset_ms: i64) -> VaultSnapshot {
    const SAMPLE_TIME: i64 = 1_800_000_000_000;
    let mut snapshot = VaultSnapshot::new_empty(SAMPLE_TIME);
    for (index, name) in ["原始分組", "空分組"].into_iter().enumerate() {
        snapshot.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order: index as u32 + 1,
            is_ungrouped: false,
        });
    }
    snapshot = prepare_import(
        &snapshot,
        "公開RFC測試驗證器",
        " 原始備註\r\n原文 ",
        snapshot.groups[1].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
        SAMPLE_TIME,
    )
    .unwrap()
    .candidate;
    let mut seedless = snapshot.authenticators[0].clone();
    seedless.id = Uuid::new_v4();
    seedless.name = "組內尚無密鑰".into();
    seedless.secret = None;
    seedless.sort_order = 1;
    snapshot.authenticators.push(seedless.clone());
    seedless.id = Uuid::new_v4();
    seedless.group_id = snapshot.groups[0].id;
    seedless.sort_order = 0;
    seedless.name = "未分組尚無密鑰".into();
    snapshot.authenticators.push(seedless);
    snapshot.service_time_sync.battlenet = Some(TimeSyncRecord {
        source: TIME_SOURCE.into(),
        server_time_ms: SAMPLE_TIME + offset_ms,
        local_wall_ms_at_sample: SAMPLE_TIME,
        offset_ms,
        rtt_ms: 40,
    });
    snapshot.validate().unwrap();
    snapshot
}

#[test]
fn disk_reopen_uses_the_same_shared_calibration_for_rows_backups_and_xml_including_real_zero() {
    for offset_ms in [-1_234, 4_321, 0] {
        let directory = Directory::new();
        let primary = directory.0.join("BNAuth.bnauth");
        let source = fixture(offset_ms);
        let expected_calibration = source.service_time_sync.battlenet.clone();
        let (mut repository, missing) = Repository::open(primary.clone()).unwrap();
        assert!(missing.is_none());
        let saved = repository.commit(&source).unwrap().snapshot;
        drop(repository);

        let (repository, reopened) = Repository::open(primary.clone()).unwrap();
        let reopened = reopened.unwrap();
        assert_eq!(reopened, saved);
        assert_eq!(reopened.groups, source.groups);
        assert_eq!(reopened.authenticators, source.authenticators);
        assert_eq!(reopened.service_time_sync.battlenet, expected_calibration);
        let mut shared_clock =
            RuntimeClock::from_saved(reopened.service_time_sync.battlenet.as_ref()).unwrap();
        assert!(shared_clock.is_from_saved());
        assert_eq!(shared_clock.offset_ms(), Some(offset_ms));
        let local_before = system_now_ms();
        let corrected_now = shared_clock.now_ms();
        let local_after = system_now_ms();
        assert!(
            i128::from(corrected_now) >= i128::from(local_before) + i128::from(offset_ms) - 100
                && i128::from(corrected_now)
                    <= i128::from(local_after) + i128::from(offset_ms) + 100,
            "reopened clock must apply the saved offset to today's time"
        );
        assert_eq!(shared_clock.last_success(), expected_calibration.as_ref());
        for record in &reopened.authenticators {
            assert!(
                reopened
                    .groups
                    .iter()
                    .any(|group| group.id == record.group_id)
            );
            if record.secret.is_some() {
                let xml = winauth2::encode(record, shared_clock.offset_ms()).unwrap();
                let xml_offset = if offset_ms == 0 { 1 } else { offset_ms };
                assert!(xml.contains(&format!("<servertimediff>{xml_offset}</servertimediff>")));
                assert_eq!(shared_clock.offset_ms(), Some(offset_ms));
            }
        }

        let full_backup = directory.0.join("完整備份.bnauth");
        export_file::write(
            &full_backup,
            &native::encode(&reopened).unwrap(),
            Format::Full,
            Some(&primary),
        )
        .unwrap();
        let backed_up = native::read_full(&full_backup).unwrap();
        assert_eq!(backed_up, reopened);
        let group = reopened.export_group(reopened.groups[1].id).unwrap();
        let group_backup = directory.0.join("分組備份.bnauth");
        export_file::write(
            &group_backup,
            &native::encode_group(&group).unwrap(),
            Format::Group,
            Some(&primary),
        )
        .unwrap();
        let backed_up_group = native::read_group(&group_backup).unwrap();
        assert_eq!(
            backed_up_group.service_time_sync.battlenet,
            expected_calibration
        );
        assert_eq!(backed_up_group, group);
        assert_eq!(native::read_full(&primary).unwrap(), saved);
        assert_eq!(
            saved
                .service_time_sync
                .battlenet
                .as_ref()
                .unwrap()
                .offset_ms,
            offset_ms,
            "the +1 ms zero sentinel belongs only to WinAuth2 XML"
        );
        drop(repository);
    }
}
