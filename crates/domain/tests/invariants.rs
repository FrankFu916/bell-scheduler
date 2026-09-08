use class_schedule_domain::{
    Capacity, ClassSizeRange, MeetingDuration, MeetingPattern, ProblemCode, Revision, WeeklyPeriods,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn every_positive_duration_partition_constructs_a_consistent_pattern(
        raw_durations in prop::collection::vec(1_u8..=8, 1..=8),
        min_days_between in 0_u8..=6,
        may_cross_breaks in any::<bool>(),
    ) {
        let total: u16 = raw_durations.iter().map(|value| u16::from(*value)).sum();
        let max_duration = raw_durations.iter().copied().max().expect("generated non-empty");
        let durations = raw_durations
            .iter()
            .copied()
            .map(|value| MeetingDuration::new(value).unwrap())
            .collect::<Vec<_>>();

        let pattern = MeetingPattern::new(
            WeeklyPeriods::new(total).unwrap(),
            durations,
            min_days_between,
            WeeklyPeriods::new(u16::from(max_duration)).unwrap(),
            may_cross_breaks,
        )
        .unwrap();

        prop_assert_eq!(pattern.weekly_periods().get(), total);
        prop_assert_eq!(
            pattern
                .durations()
                .iter()
                .map(|duration| u16::from(duration.get()))
                .sum::<u16>(),
            total
        );
    }

    #[test]
    fn inconsistent_meeting_totals_always_have_a_stable_problem_code(
        weekly_periods in 1_u8..=100,
    ) {
        let error = MeetingPattern::new(
            WeeklyPeriods::new(u16::from(weekly_periods)).unwrap(),
            vec![MeetingDuration::new(weekly_periods + 1).unwrap()],
            0,
            WeeklyPeriods::new(u16::from(weekly_periods + 1)).unwrap(),
            false,
        )
        .unwrap_err();

        prop_assert_eq!(error.code(), ProblemCode::DomainInconsistentTotal);
    }

    #[test]
    fn class_size_range_acceptance_matches_its_ordering_invariant(
        min in 0_u16..=200,
        target in 0_u16..=200,
        max in 1_u16..=200,
    ) {
        let result = ClassSizeRange::new(min, target, Capacity::new(max).unwrap());
        prop_assert_eq!(result.is_ok(), min <= target && target <= max);
    }

    #[test]
    fn revision_increment_is_monotonic(value in 0_u64..u64::MAX) {
        let current = Revision::from_u64(value);
        let next = current.next().unwrap();
        prop_assert_eq!(next.get(), value + 1);
        prop_assert!(next > current);
    }
}
