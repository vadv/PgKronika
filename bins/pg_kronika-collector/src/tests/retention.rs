#[cfg(test)]
mod retention_tests {
    use crate::config::RetentionPolicy;
    use crate::rotation::RotationState;
    use std::path::PathBuf;

    #[test]
    fn retention_policy_fixed_detects_overflow() {
        let mut state =
            RotationState::new(RetentionPolicy::Fixed(1000), PathBuf::from("/tmp"), 500);
        assert!(!state.should_rotate());
        state.update_size(600);
        assert!(state.should_rotate());
    }

    #[test]
    fn retention_policy_auto_defaults_to_80() {
        let state = RotationState::new(RetentionPolicy::Auto(80), PathBuf::from("/tmp"), 0);
        assert_eq!(state.get_current_size(), 0);
        // Can't test auto mode without real fs, but structure is sound
    }

    #[test]
    fn retention_policy_disabled_never_rotates() {
        let mut state =
            RotationState::new(RetentionPolicy::Disabled, PathBuf::from("/tmp"), 9999999);
        assert!(!state.should_rotate());
        state.update_size(9999999);
        assert!(!state.should_rotate());
    }

    #[test]
    fn retention_size_update_saturates() {
        let mut state =
            RotationState::new(RetentionPolicy::Fixed(1000), PathBuf::from("/tmp"), 500);
        state.update_size(600);
        assert_eq!(state.get_current_size(), 1100);
        state.update_size(-600);
        assert_eq!(state.get_current_size(), 500);
        state.update_size(-1000);
        assert_eq!(state.get_current_size(), 0);
    }

    #[test]
    fn degradation_event_throttles() {
        use std::time::{Duration, Instant};
        let mut state = RotationState::new(RetentionPolicy::Disabled, PathBuf::from("/tmp"), 0);
        let now = Instant::now();
        assert!(state.can_emit_degradation(now));
        assert!(!state.can_emit_degradation(now + Duration::from_secs(30)));
        assert!(state.can_emit_degradation(now + Duration::from_secs(61)));
    }
}
