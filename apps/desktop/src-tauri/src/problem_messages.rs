//! Display-only translations of independent validator results; no constraint decisions live here.

use class_schedule_validation::HardProblemCode;

pub(super) fn hard_problem_message(code: HardProblemCode) -> &'static str {
    use HardProblemCode as Code;
    match code {
        Code::AssignmentMissing => "课表缺少应安排的活动。",
        Code::AssignmentDuplicate => "同一活动在课表中被安排多次。",
        Code::AssignmentUnknownActivity => "课表包含不属于此输入版本的活动。",
        Code::StartNotAllowed => "活动的开始时间不在允许范围内。",
        Code::DurationInvalid => "连续课时无法完整安排，或跨越了不允许跨越的休息时段。",
        Code::TeacherNotAllowed => "安排的教师不符合该课程的固定或候选教师要求。",
        Code::TeacherUnavailable => "该教师在活动占用的时段不可用。",
        Code::OfferingTeacherMismatch => "同一开课的多次活动没有使用一致的固定教师。",
        Code::RoomNotAllowed => "安排的教室不符合该活动的教室策略。",
        Code::RoomUnavailable => "该教室在活动占用的时段不可用。",
        Code::RoomCapacityInsufficient => "教室容量不足以容纳实际参加活动的学生。",
        Code::RoomFeatureMissing => "教室缺少该活动要求的设施或功能。",
        Code::StudentConflict => "实际选课学生需要同时参加多个活动，发生学生时间冲突。",
        Code::TeacherConflict => "同一教师在同一时段被安排到多个活动。",
        Code::RoomConflict => "同一教室在同一时段被多个活动占用。",
        Code::SectionFixedRoomMismatch => "教学班的活动没有使用要求的一致固定教室。",
        Code::LockedAssignmentChanged => "课表改变了已经锁定的活动安排。",
        Code::MeetingMinimumGapDays => "同一课程的活动间隔小于要求的天数。",
        Code::MeetingMaximumPeriodsPerDay => "同一课程在一天内安排的课时超过上限。",
        Code::MeetingDemandNoLegalStart => {
            "活动没有可容纳完整时长的允许开始时间，请检查日历、连堂和休息设置。"
        }
        Code::ActivityNoEligibleTeacher => {
            "活动没有符合要求的教师，请检查教师学科与固定或候选教师配置。"
        }
        Code::ActivityNoEligibleRoom => {
            "活动没有符合要求的教室，请检查容量、设施和固定或候选教室配置。"
        }
        Code::ActivityNoFeasibleResourceCombination => {
            "活动找不到同时可用的时间、教师和教室组合，请检查不可用时段与固定安排。"
        }
        Code::TeacherRequiredLoadExceedsAvailability => {
            "教师必须承担的课时超过其可用时段，请检查授课分配与不可用时间。"
        }
        Code::StudentRequiredLoadExceedsCalendar => {
            "学生实际需要参加的总课时超过日历可用课时，请检查课程计划与日历。"
        }
        Code::SectionHasNoCommonFixedRoom => {
            "教学班各次活动没有共同可用的固定教室，请检查教室策略。"
        }
        Code::FeatureRoomSupplyInsufficient => {
            "所需专用教室的课时供给不足，请检查设施要求与教室数量。"
        }
        Code::FixedAssignmentsConflict => {
            "固定活动之间发生时间或资源冲突，请核对相关活动的学生、教师和教室。"
        }
        _ => "数据违反必需约束，请按问题代码与相关活动检查；此问题不能通过降低质量权重忽略。",
    }
}
