use super::ClockStatusNotices;

#[test]
fn a_jump_and_rejected_sample_in_the_same_frame_report_once() {
    let mut notices = ClockStatusNotices::default();
    notices.begin_sync();
    assert!(notices.should_report_discontinuity(true, true));
    assert!(!notices.should_report_discontinuity(false, false));
}

#[test]
fn a_jump_reported_while_sync_is_pending_does_not_repeat_when_its_old_sample_arrives() {
    let mut notices = ClockStatusNotices::default();
    assert!(!notices.should_report_discontinuity(false, false));
    notices.begin_sync();

    // The UI sees the clock jump before receiving the result from the same round.
    assert!(notices.should_report_discontinuity(true, false));
    assert!(!notices.should_report_discontinuity(false, true));
}

#[test]
fn new_failed_sync_rounds_report_their_own_errors_even_before_any_success() {
    let mut notices = ClockStatusNotices::default();
    assert!(!notices.should_report_discontinuity(false, false));
    for _ in 0..2 {
        notices.begin_sync();
        assert!(notices.should_report_discontinuity(false, true));
        assert!(!notices.should_report_discontinuity(false, false));
    }
}
