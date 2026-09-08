#include "ortools_worker/solver_worker.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <exception>
#include <istream>
#include <limits>
#include <map>
#include <optional>
#include <ostream>
#include <set>
#include <string>
#include <string_view>
#include <tuple>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include "ortools/sat/cp_model.h"
#include "ortools/sat/cp_model_checker.h"
#include "ortools/sat/cp_model_solver.h"
#include "ortools/sat/sat_parameters.pb.h"
#include "ortools_worker/frame_io.h"
#include "ortools_worker/sha256.h"

namespace class_schedule::solver {
namespace {

namespace protocol = scheduler::v1;
namespace sat = operations_research::sat;

using Clock = std::chrono::steady_clock;
using Id = std::uint32_t;

struct Failure {
  protocol::SolverStatus status = protocol::SOLVER_STATUS_INVALID_INPUT;
  std::string code;
};

struct Atom {
  Id week = 0;
  Id day = 0;
  Id period = 0;

  auto operator<=>(const Atom&) const = default;
};

struct StartChoice {
  Id timeslot_id = 0;
  Id day = 0;
  std::vector<Id> occupied_timeslot_ids;
  std::vector<Atom> atoms;
  sat::BoolVar selected;
};

struct ActivityVariables {
  const protocol::Activity* activity = nullptr;
  std::vector<StartChoice> starts;
  std::vector<std::int64_t> teacher_candidates;
  std::vector<std::int64_t> room_candidates;
  sat::IntVar teacher;
  sat::IntVar room;
};

struct BindingVariables {
  const protocol::SectionRoomBinding* binding = nullptr;
  std::vector<std::int64_t> candidates;
  sat::IntVar room;
  bool fixed_across_activities = false;
};

struct TeacherBindingVariables {
  std::vector<std::int64_t> candidates;
  sat::IntVar teacher;
};

struct MetricExpression {
  protocol::ObjectiveMetricKind kind =
      protocol::OBJECTIVE_METRIC_KIND_UNSPECIFIED;
  sat::LinearExpr expression;
};

struct TierExpression {
  std::string tier_id;
  Id priority = 0;
  sat::LinearExpr expression;
  std::vector<MetricExpression> metrics;
};

struct BuiltModel {
  sat::CpModelBuilder model;
  std::vector<ActivityVariables> activities;
  std::map<Id, BindingVariables> bindings;
  std::map<Id, TeacherBindingVariables> teacher_bindings;
  std::vector<TierExpression> tiers;
  std::map<int, const protocol::ConstraintGroup*> assumption_groups;
};

struct SolveAggregate {
  sat::CpSolverResponse final_response;
  bool has_solution = false;
  bool all_tiers_optimal = true;
  bool timed_out = false;
  double deterministic_time = 0.0;
  std::uint64_t conflicts = 0;
  std::uint64_t branches = 0;
  std::uint64_t propagations = 0;
};

bool IsBlank(std::string_view value) {
  return value.empty() ||
         std::all_of(value.begin(), value.end(), [](unsigned char character) {
           return character == ' ' || character == '\t' || character == '\r' ||
                  character == '\n';
         });
}

template <typename Range>
bool HasDuplicates(const Range& values) {
  using Value = typename Range::value_type;
  std::set<Value> seen;
  for (const auto& value : values) {
    if (!seen.insert(value).second) {
      return true;
    }
  }
  return false;
}

template <typename Range>
std::vector<std::int64_t> ToSortedDomain(const Range& values) {
  std::vector<std::int64_t> result;
  result.reserve(static_cast<std::size_t>(values.size()));
  for (const auto value : values) {
    result.push_back(static_cast<std::int64_t>(value));
  }
  std::sort(result.begin(), result.end());
  result.erase(std::unique(result.begin(), result.end()), result.end());
  return result;
}

template <typename T>
bool Contains(const std::vector<T>& values, const T value) {
  return std::find(values.begin(), values.end(), value) != values.end();
}

std::uint64_t NonNegative(std::int64_t value) {
  return value < 0 ? 0U : static_cast<std::uint64_t>(value);
}

std::int64_t RoundedInteger(double value) {
  if (!std::isfinite(value)) {
    return 0;
  }
  if (value >= static_cast<double>(std::numeric_limits<std::int64_t>::max())) {
    return std::numeric_limits<std::int64_t>::max();
  }
  if (value <= static_cast<double>(std::numeric_limits<std::int64_t>::min())) {
    return std::numeric_limits<std::int64_t>::min();
  }
  return static_cast<std::int64_t>(std::llround(value));
}

void PopulateEngineVersion(protocol::EngineVersion* version) {
  version->set_engine_name(kEngineName);
  version->set_engine_version(kEngineVersion);
  version->set_adapter_version(kAdapterVersion);
#ifdef ORTOOLS_WORKER_BUILD_REVISION
  version->set_build_revision(ORTOOLS_WORKER_BUILD_REVISION);
#else
  version->set_build_revision("development");
#endif
}

protocol::SolverParameters SanitizedParameters(
    const protocol::SolverParameters* supplied) {
  protocol::SolverParameters effective;
  if (supplied != nullptr) {
    effective.CopyFrom(*supplied);
  }
  if (effective.mode() == protocol::SOLVE_MODE_UNSPECIFIED ||
      !protocol::SolveMode_IsValid(effective.mode())) {
    effective.set_mode(protocol::SOLVE_MODE_GENERATE);
  }
  if (effective.profile() == protocol::SOLVER_PROFILE_UNSPECIFIED ||
      !protocol::SolverProfile_IsValid(effective.profile())) {
    effective.set_profile(protocol::SOLVER_PROFILE_BALANCED);
  }
  if (effective.time_limit_millis() == 0) {
    effective.set_time_limit_millis(1);
  }
  if (effective.worker_count() == 0 ||
      effective.worker_count() >
          static_cast<std::uint32_t>(std::numeric_limits<std::int32_t>::max())) {
    effective.set_worker_count(1);
  }
  if (effective.reproducible()) {
    effective.set_worker_count(1);
  }
  if (effective.relative_gap_limit_ppm() > 1'000'000U) {
    effective.set_relative_gap_limit_ppm(1'000'000U);
  }
  return effective;
}

protocol::SolverEnvelope BaseResponse(
    const protocol::SolverEnvelope& request_envelope,
    const protocol::SolverParameters* supplied_parameters,
    const protocol::SchedulingProblemSnapshot* problem) {
  protocol::SolverEnvelope envelope;
  envelope.set_protocol_version(kProtocolVersion);
  envelope.set_request_id(request_envelope.request_id());
  auto* response = envelope.mutable_solve_response();
  PopulateEngineVersion(response->mutable_engine_version());
  response->mutable_effective_parameters()->CopyFrom(
      SanitizedParameters(supplied_parameters));
  if (problem != nullptr && problem->snapshot_hash().size() == 32) {
    response->set_input_snapshot_hash(problem->snapshot_hash());
  } else if (problem != nullptr) {
    const auto digest = Sha256(problem->SerializeAsString());
    response->set_input_snapshot_hash(digest.data(), digest.size());
  } else {
    const auto digest = Sha256(std::string_view{});
    response->set_input_snapshot_hash(digest.data(), digest.size());
  }
  return envelope;
}

void AddDiagnostic(protocol::SolveResponse* response, std::string_view group_id,
                   std::string_view problem_code,
                   protocol::DiagnosticSignal signal) {
  if (!response->effective_parameters().collect_diagnostics()) {
    return;
  }
  auto* diagnostic = response->add_diagnostic_groups();
  diagnostic->set_group_id(group_id);
  diagnostic->set_problem_code(problem_code);
  diagnostic->set_signal(signal);
}

protocol::SolverEnvelope FailureResponse(
    const protocol::SolverEnvelope& request_envelope,
    const protocol::SolverParameters* parameters,
    const protocol::SchedulingProblemSnapshot* problem, const Failure& failure) {
  auto envelope = BaseResponse(request_envelope, parameters, problem);
  auto* response = envelope.mutable_solve_response();
  response->set_status(failure.status);
  response->set_status_detail_code(failure.code);
  AddDiagnostic(response, "worker:model", failure.code,
                protocol::DIAGNOSTIC_SIGNAL_MODEL_BUILD);
  auto* statistics = response->mutable_statistics();
  statistics->set_worker_count(response->effective_parameters().worker_count());
  statistics->set_seed(response->effective_parameters().seed());
  return envelope;
}

std::optional<Failure> ValidateParameters(
    const protocol::SolverParameters& parameters) {
  if (!protocol::SolveMode_IsValid(parameters.mode()) ||
      parameters.mode() == protocol::SOLVE_MODE_UNSPECIFIED) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.INVALID_SOLVE_MODE"};
  }
  if (!protocol::SolverProfile_IsValid(parameters.profile()) ||
      parameters.profile() == protocol::SOLVER_PROFILE_UNSPECIFIED) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.INVALID_SOLVER_PROFILE"};
  }
  if (parameters.time_limit_millis() == 0) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.TIME_LIMIT_MUST_BE_POSITIVE"};
  }
  if (parameters.worker_count() == 0 ||
      parameters.worker_count() >
          static_cast<std::uint32_t>(std::numeric_limits<std::int32_t>::max())) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.WORKER_COUNT_OUT_OF_RANGE"};
  }
  if (parameters.reproducible() && parameters.worker_count() != 1) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.REPRODUCIBLE_REQUIRES_ONE_WORKER"};
  }
  if (parameters.relative_gap_limit_ppm() > 1'000'000U) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.RELATIVE_GAP_OUT_OF_RANGE"};
  }
  return std::nullopt;
}

template <typename MessageRange, typename IdAccessor>
std::optional<Failure> ValidateUniquePositiveIds(const MessageRange& messages,
                                                 IdAccessor id_accessor,
                                                 std::string_view code) {
  std::unordered_set<Id> ids;
  for (const auto& message : messages) {
    const Id id = id_accessor(message);
    if (id == 0 || !ids.insert(id).second) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT, std::string(code)};
    }
  }
  return std::nullopt;
}

std::optional<Failure> ValidateProblemStructure(
    const protocol::SchedulingProblemSnapshot& problem) {
  if (problem.schema_version() != kSnapshotSchemaVersion) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.UNSUPPORTED_SNAPSHOT_SCHEMA"};
  }
  if (problem.snapshot_hash().size() != 32) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.INVALID_SNAPSHOT_HASH"};
  }
  if (IsBlank(problem.project_id())) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.EMPTY_PROJECT_ID"};
  }

  if (const auto failure = ValidateUniquePositiveIds(
          problem.week_patterns(),
          [](const auto& item) { return item.week_pattern_id(); },
          "WORKER.INVALID_WEEK_PATTERN_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.timeslots(), [](const auto& item) { return item.timeslot_id(); },
          "WORKER.INVALID_TIMESLOT_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.rooms(), [](const auto& item) { return item.room_id(); },
          "WORKER.INVALID_ROOM_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.teachers(), [](const auto& item) { return item.teacher_id(); },
          "WORKER.INVALID_TEACHER_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.activities(), [](const auto& item) { return item.activity_id(); },
          "WORKER.INVALID_ACTIVITY_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.section_room_bindings(),
          [](const auto& item) { return item.binding_id(); },
          "WORKER.INVALID_ROOM_BINDING_IDS")) {
    return failure;
  }
  if (const auto failure = ValidateUniquePositiveIds(
          problem.meeting_patterns(),
          [](const auto& item) { return item.meeting_pattern_id(); },
          "WORKER.INVALID_MEETING_PATTERN_IDS")) {
    return failure;
  }

  std::unordered_map<Id, const protocol::WeekPattern*> week_patterns;
  for (const auto& pattern : problem.week_patterns()) {
    if (pattern.teaching_week_numbers().empty() ||
        HasDuplicates(pattern.teaching_week_numbers())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_TEACHING_WEEKS"};
    }
    for (const Id week : pattern.teaching_week_numbers()) {
      if (week == 0) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.INVALID_TEACHING_WEEKS"};
      }
    }
    week_patterns.emplace(pattern.week_pattern_id(), &pattern);
  }

  std::unordered_map<Id, const protocol::CalendarTimeslot*> timeslots;
  std::set<std::tuple<Id, Id, Id>> timeslot_coordinates;
  for (const auto& slot : problem.timeslots()) {
    if (!week_patterns.contains(slot.week_pattern_id()) || slot.day_index() == 0 ||
        slot.period_index() == 0 ||
        !timeslot_coordinates
             .emplace(slot.week_pattern_id(), slot.day_index(),
                      slot.period_index())
             .second) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_TIMESLOT"};
    }
    timeslots.emplace(slot.timeslot_id(), &slot);
  }
  for (const auto& slot : problem.timeslots()) {
    if (slot.has_next_consecutive_timeslot_id()) {
      const auto next = timeslots.find(slot.next_consecutive_timeslot_id());
      if (next == timeslots.end() ||
          next->second->week_pattern_id() != slot.week_pattern_id() ||
          next->second->day_index() != slot.day_index() ||
          next->second->period_index() != slot.period_index() + 1U) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.INVALID_CONSECUTIVE_LINK"};
      }
    }
  }

  std::unordered_map<Id, const protocol::Room*> rooms;
  for (const auto& room : problem.rooms()) {
    if (room.capacity() == 0 || room.building_id() == 0 ||
        HasDuplicates(room.feature_ids()) ||
        HasDuplicates(room.available_timeslot_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_ROOM"};
    }
    for (const Id slot : room.available_timeslot_ids()) {
      if (!timeslots.contains(slot)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.ROOM_AVAILABILITY_UNKNOWN_TIMESLOT"};
      }
    }
    rooms.emplace(room.room_id(), &room);
  }

  std::unordered_map<Id, const protocol::Teacher*> teachers;
  for (const auto& teacher : problem.teachers()) {
    if (HasDuplicates(teacher.available_timeslot_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_TEACHER_AVAILABILITY"};
    }
    for (const Id slot : teacher.available_timeslot_ids()) {
      if (!timeslots.contains(slot)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.TEACHER_AVAILABILITY_UNKNOWN_TIMESLOT"};
      }
    }
    teachers.emplace(teacher.teacher_id(), &teacher);
  }

  std::unordered_map<Id, const protocol::SectionRoomBinding*> bindings;
  for (const auto& binding : problem.section_room_bindings()) {
    if (!binding.has_policy() || binding.required_capacity() == 0 ||
        HasDuplicates(binding.required_feature_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_ROOM_BINDING"};
    }
    std::vector<Id> candidates;
    switch (binding.policy().policy_case()) {
      case protocol::RoomPolicy::kAdminHomeRoom:
        candidates.push_back(binding.policy().admin_home_room().room_id());
        break;
      case protocol::RoomPolicy::kFixed:
        candidates.push_back(binding.policy().fixed().room_id());
        break;
      case protocol::RoomPolicy::kSectionFixed:
        candidates.assign(binding.policy().section_fixed().candidate_room_ids().begin(),
                          binding.policy().section_fixed().candidate_room_ids().end());
        break;
      case protocol::RoomPolicy::kPreferredFixed:
        candidates.assign(binding.policy().preferred_fixed().preferred_room_ids().begin(),
                          binding.policy().preferred_fixed().preferred_room_ids().end());
        candidates.insert(candidates.end(),
                          binding.policy().preferred_fixed().fallback_room_ids().begin(),
                          binding.policy().preferred_fixed().fallback_room_ids().end());
        break;
      case protocol::RoomPolicy::kFlexible:
        candidates.assign(binding.policy().flexible().candidate_room_ids().begin(),
                          binding.policy().flexible().candidate_room_ids().end());
        break;
      case protocol::RoomPolicy::POLICY_NOT_SET:
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.ROOM_POLICY_MISSING"};
    }
    if (candidates.empty() || HasDuplicates(candidates)) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_ROOM_CANDIDATES"};
    }
    for (const Id candidate : candidates) {
      if (!rooms.contains(candidate)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.ROOM_CANDIDATE_NOT_FOUND"};
      }
    }
    bindings.emplace(binding.binding_id(), &binding);
  }

  std::unordered_map<Id, const protocol::MeetingPattern*> meeting_patterns;
  for (const auto& pattern : problem.meeting_patterns()) {
    if (pattern.activity_ids().empty() ||
        pattern.activity_ids_size() != pattern.duration_periods_size() ||
        pattern.maximum_periods_per_day() == 0 ||
        HasDuplicates(pattern.activity_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_MEETING_PATTERN"};
    }
    meeting_patterns.emplace(pattern.meeting_pattern_id(), &pattern);
  }

  std::unordered_map<Id, const protocol::Activity*> activities;
  std::unordered_map<Id, std::vector<Id>> teacher_bindings;
  std::unordered_set<Id> demand_ids;
  for (const auto& activity : problem.activities()) {
    if (activity.meeting_demand_id() == 0 || activity.subject_id() == 0 ||
        activity.duration_periods() == 0 ||
        activity.allowed_start_timeslot_ids().empty() ||
        HasDuplicates(activity.allowed_start_timeslot_ids()) ||
        !demand_ids.insert(activity.meeting_demand_id()).second ||
        !bindings.contains(activity.section_room_binding_id()) ||
        !meeting_patterns.contains(activity.meeting_pattern_id()) ||
        !activity.has_teacher_policy() || activity.teacher_binding_id() == 0) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_ACTIVITY"};
    }
    for (const Id start : activity.allowed_start_timeslot_ids()) {
      if (!timeslots.contains(start)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.ACTIVITY_START_NOT_FOUND"};
      }
    }
    std::vector<Id> teacher_candidates;
    switch (activity.teacher_policy().policy_case()) {
      case protocol::TeacherAssignmentPolicy::kFixedTeacher:
        teacher_candidates.push_back(
            activity.teacher_policy().fixed_teacher().teacher_id());
        break;
      case protocol::TeacherAssignmentPolicy::kCandidateTeachers:
        teacher_candidates.assign(
            activity.teacher_policy().candidate_teachers().teacher_ids().begin(),
            activity.teacher_policy().candidate_teachers().teacher_ids().end());
        break;
      case protocol::TeacherAssignmentPolicy::POLICY_NOT_SET:
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.TEACHER_POLICY_MISSING"};
    }
    if (teacher_candidates.empty() || HasDuplicates(teacher_candidates)) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_TEACHER_CANDIDATES"};
    }
    for (const Id teacher : teacher_candidates) {
      if (!teachers.contains(teacher)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.TEACHER_CANDIDATE_NOT_FOUND"};
      }
    }
    std::sort(teacher_candidates.begin(), teacher_candidates.end());
    const auto [binding, inserted] = teacher_bindings.emplace(
        activity.teacher_binding_id(), teacher_candidates);
    if (!inserted && binding->second != teacher_candidates) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INCONSISTENT_TEACHER_BINDING"};
    }
    activities.emplace(activity.activity_id(), &activity);
  }

  std::unordered_set<Id> pattern_membership;
  for (const auto& pattern : problem.meeting_patterns()) {
    for (int index = 0; index < pattern.activity_ids_size(); ++index) {
      const Id activity_id = pattern.activity_ids(index);
      const auto activity = activities.find(activity_id);
      if (activity == activities.end() ||
          !pattern_membership.insert(activity_id).second ||
          activity->second->meeting_pattern_id() != pattern.meeting_pattern_id() ||
          pattern.duration_periods(index) !=
              activity->second->duration_periods()) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.MEETING_PATTERN_ACTIVITY_MISMATCH"};
      }
    }
  }
  if (pattern_membership.size() != activities.size()) {
    return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                   "WORKER.ACTIVITY_WITHOUT_MEETING_PATTERN"};
  }

  std::unordered_map<std::string, const protocol::ConstraintGroup*> groups;
  for (const auto& group : problem.constraint_groups()) {
    if (IsBlank(group.group_id()) || IsBlank(group.problem_code()) ||
        !protocol::ConstraintSeverity_IsValid(group.severity()) ||
        group.severity() == protocol::CONSTRAINT_SEVERITY_UNSPECIFIED ||
        !groups.emplace(group.group_id(), &group).second) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_CONSTRAINT_GROUP"};
    }
  }
  for (const auto& activity : problem.activities()) {
    if (HasDuplicates(activity.constraint_group_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.DUPLICATE_ACTIVITY_CONSTRAINT_GROUP"};
    }
    for (const auto& group : activity.constraint_group_ids()) {
      if (!groups.contains(group)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.ACTIVITY_CONSTRAINT_GROUP_NOT_FOUND"};
      }
    }
  }

  std::unordered_set<std::string> lock_ids;
  std::unordered_set<Id> locked_activities;
  for (const auto& lock : problem.locks()) {
    if (IsBlank(lock.lock_id()) || !lock_ids.insert(lock.lock_id()).second ||
        !lock.has_assignment() || IsBlank(lock.constraint_group_id()) ||
        !groups.contains(lock.constraint_group_id()) ||
        groups.at(lock.constraint_group_id())->severity() !=
            protocol::CONSTRAINT_SEVERITY_HARD ||
        !activities.contains(lock.assignment().activity_id()) ||
        !locked_activities.insert(lock.assignment().activity_id()).second ||
        !timeslots.contains(lock.assignment().start_timeslot_id()) ||
        !rooms.contains(lock.assignment().room_id()) ||
        !teachers.contains(lock.assignment().teacher_id()) ||
        lock.assignment().duration_periods() !=
            activities.at(lock.assignment().activity_id())->duration_periods()) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_LOCK"};
    }
  }

  std::unordered_set<Id> incumbent_activities;
  for (const auto& assignment : problem.incumbent_assignments()) {
    if (!activities.contains(assignment.activity_id()) ||
        !incumbent_activities.insert(assignment.activity_id()).second ||
        !timeslots.contains(assignment.start_timeslot_id()) ||
        !rooms.contains(assignment.room_id()) ||
        !teachers.contains(assignment.teacher_id()) ||
        assignment.duration_periods() !=
            activities.at(assignment.activity_id())->duration_periods()) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_INCUMBENT_ASSIGNMENT"};
    }
  }

  for (const auto& edge : problem.student_conflicts().edges()) {
    if (edge.left_activity_id() == edge.right_activity_id() ||
        !activities.contains(edge.left_activity_id()) ||
        !activities.contains(edge.right_activity_id())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_STUDENT_CONFLICT_EDGE"};
    }
  }
  for (const auto& clique : problem.student_conflicts().cliques()) {
    if (clique.activity_ids_size() < 2 || HasDuplicates(clique.activity_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_STUDENT_CONFLICT_CLIQUE"};
    }
    for (const Id activity_id : clique.activity_ids()) {
      if (!activities.contains(activity_id)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.STUDENT_CONFLICT_ACTIVITY_NOT_FOUND"};
      }
    }
  }

  for (const auto& group : problem.resource_conflicts()) {
    if (!protocol::ResourceKind_IsValid(group.resource_kind()) ||
        group.resource_kind() == protocol::RESOURCE_KIND_UNSPECIFIED ||
        group.resource_id() == 0 || group.activity_ids().empty() ||
        HasDuplicates(group.activity_ids())) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_RESOURCE_CONFLICT_GROUP"};
    }
    if ((group.resource_kind() == protocol::RESOURCE_KIND_TEACHER &&
         !teachers.contains(group.resource_id())) ||
        (group.resource_kind() == protocol::RESOURCE_KIND_ROOM &&
         !rooms.contains(group.resource_id()))) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.RESOURCE_CONFLICT_RESOURCE_NOT_FOUND"};
    }
    for (const Id activity_id : group.activity_ids()) {
      if (!activities.contains(activity_id)) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.RESOURCE_CONFLICT_ACTIVITY_NOT_FOUND"};
      }
    }
  }

  std::unordered_set<std::string> tier_ids;
  std::unordered_set<Id> tier_priorities;
  for (const auto& tier : problem.objective_tiers()) {
    if (IsBlank(tier.tier_id()) || tier.priority() == 0 ||
        !tier_ids.insert(tier.tier_id()).second ||
        !tier_priorities.insert(tier.priority()).second) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.INVALID_OBJECTIVE_TIER"};
    }
    for (const auto& metric : tier.metrics()) {
      if (!protocol::ObjectiveMetricKind_IsValid(metric.metric_kind()) ||
          metric.metric_kind() == protocol::OBJECTIVE_METRIC_KIND_UNSPECIFIED ||
          metric.weight_within_tier() == 0) {
        return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                       "WORKER.INVALID_OBJECTIVE_METRIC"};
      }
      for (const auto& reference : metric.scope()) {
        if (!protocol::EntityKind_IsValid(reference.entity_kind()) ||
            reference.entity_kind() == protocol::ENTITY_KIND_UNSPECIFIED ||
            reference.compact_id() == 0 ||
            (reference.entity_kind() == protocol::ENTITY_KIND_ACTIVITY &&
             !activities.contains(reference.compact_id()))) {
          return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                         "WORKER.INVALID_OBJECTIVE_SCOPE"};
        }
      }
    }
  }
  return std::nullopt;
}

std::vector<Id> RoomCandidates(
    const protocol::SectionRoomBinding& binding) {
  std::vector<Id> candidates;
  switch (binding.policy().policy_case()) {
    case protocol::RoomPolicy::kAdminHomeRoom:
      candidates.push_back(binding.policy().admin_home_room().room_id());
      break;
    case protocol::RoomPolicy::kFixed:
      candidates.push_back(binding.policy().fixed().room_id());
      break;
    case protocol::RoomPolicy::kSectionFixed:
      candidates.assign(binding.policy().section_fixed().candidate_room_ids().begin(),
                        binding.policy().section_fixed().candidate_room_ids().end());
      break;
    case protocol::RoomPolicy::kPreferredFixed:
      candidates.assign(binding.policy().preferred_fixed().preferred_room_ids().begin(),
                        binding.policy().preferred_fixed().preferred_room_ids().end());
      candidates.insert(candidates.end(),
                        binding.policy().preferred_fixed().fallback_room_ids().begin(),
                        binding.policy().preferred_fixed().fallback_room_ids().end());
      break;
    case protocol::RoomPolicy::kFlexible:
      candidates.assign(binding.policy().flexible().candidate_room_ids().begin(),
                        binding.policy().flexible().candidate_room_ids().end());
      break;
    case protocol::RoomPolicy::POLICY_NOT_SET:
      break;
  }
  return candidates;
}

std::vector<Id> TeacherCandidates(const protocol::Activity& activity) {
  std::vector<Id> candidates;
  switch (activity.teacher_policy().policy_case()) {
    case protocol::TeacherAssignmentPolicy::kFixedTeacher:
      candidates.push_back(activity.teacher_policy().fixed_teacher().teacher_id());
      break;
    case protocol::TeacherAssignmentPolicy::kCandidateTeachers:
      candidates.assign(
          activity.teacher_policy().candidate_teachers().teacher_ids().begin(),
          activity.teacher_policy().candidate_teachers().teacher_ids().end());
      break;
    case protocol::TeacherAssignmentPolicy::POLICY_NOT_SET:
      break;
  }
  return candidates;
}

bool RoomMeetsRequirements(const protocol::Room& room,
                           const protocol::SectionRoomBinding& binding) {
  if (room.capacity() < binding.required_capacity()) {
    return false;
  }
  std::unordered_set<Id> features(room.feature_ids().begin(),
                                  room.feature_ids().end());
  return std::all_of(binding.required_feature_ids().begin(),
                     binding.required_feature_ids().end(),
                     [&features](Id feature) { return features.contains(feature); });
}

std::optional<Failure> ExpandStart(
    const protocol::Activity& activity,
    const protocol::MeetingPattern& meeting_pattern,
    const std::unordered_map<Id, const protocol::CalendarTimeslot*>& timeslots,
    const std::unordered_map<Id, const protocol::WeekPattern*>& week_patterns,
    Id start_id, std::vector<Id>* occupied, std::vector<Atom>* atoms) {
  Id current_id = start_id;
  std::unordered_set<Id> seen;
  for (Id offset = 0; offset < activity.duration_periods(); ++offset) {
    const auto current_it = timeslots.find(current_id);
    if (current_it == timeslots.end() || !seen.insert(current_id).second) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.ACTIVITY_DURATION_INVALID"};
    }
    const auto& current = *current_it->second;
    occupied->push_back(current_id);
    const auto pattern = week_patterns.find(current.week_pattern_id());
    if (pattern == week_patterns.end()) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.ACTIVITY_WEEK_PATTERN_NOT_FOUND"};
    }
    for (const Id week : pattern->second->teaching_week_numbers()) {
      atoms->push_back(Atom{week, current.day_index(), current.period_index()});
    }
    if (offset + 1U == activity.duration_periods()) {
      continue;
    }
    if (current.has_next_consecutive_timeslot_id()) {
      current_id = current.next_consecutive_timeslot_id();
      continue;
    }
    if (meeting_pattern.forbid_cross_break()) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.ACTIVITY_DURATION_CROSSES_BREAK"};
    }
    const auto next = std::find_if(
        timeslots.begin(), timeslots.end(), [&current](const auto& entry) {
          const auto& candidate = *entry.second;
          return candidate.week_pattern_id() == current.week_pattern_id() &&
                 candidate.day_index() == current.day_index() &&
                 candidate.period_index() == current.period_index() + 1U;
        });
    if (next == timeslots.end()) {
      return Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                     "WORKER.ACTIVITY_DURATION_OUT_OF_CALENDAR"};
    }
    current_id = next->first;
  }
  std::sort(atoms->begin(), atoms->end());
  atoms->erase(std::unique(atoms->begin(), atoms->end()), atoms->end());
  return std::nullopt;
}

bool SharesTeachingWeek(const StartChoice& left, const StartChoice& right) {
  std::unordered_set<Id> left_weeks;
  for (const auto& atom : left.atoms) {
    left_weeks.insert(atom.week);
  }
  return std::any_of(right.atoms.begin(), right.atoms.end(),
                     [&left_weeks](const Atom& atom) {
                       return left_weeks.contains(atom.week);
                     });
}

void AddStudentConflictGroup(
    const std::vector<Id>& activity_ids,
    const std::unordered_map<Id, std::size_t>& activity_index,
    BuiltModel* built) {
  std::map<Atom, std::vector<sat::BoolVar>> occupancy;
  for (const Id activity_id : activity_ids) {
    const auto& activity = built->activities[activity_index.at(activity_id)];
    for (const auto& start : activity.starts) {
      for (const auto& atom : start.atoms) {
        occupancy[atom].push_back(start.selected);
      }
    }
  }
  for (const auto& [atom, literals] : occupancy) {
    static_cast<void>(atom);
    if (literals.size() >= 2) {
      built->model.AddAtMostOne(literals);
    }
  }
}

void AddResourceOccupancyConstraints(BuiltModel* built) {
  std::map<std::pair<int, std::int64_t>, sat::BoolVar> equality_literals;
  std::map<std::pair<int, int>, sat::BoolVar> conjunction_literals;
  std::map<std::pair<Id, Atom>, std::vector<sat::BoolVar>> teacher_occupancy;
  std::map<std::pair<Id, Atom>, std::vector<sat::BoolVar>> room_occupancy;
  std::size_t equality_index = 0;
  std::size_t conjunction_index = 0;

  const auto equality_literal =
      [&equality_literals, &equality_index,
       built](sat::IntVar variable, std::int64_t value) -> sat::BoolVar {
    const auto key = std::make_pair(variable.index(), value);
    if (const auto existing = equality_literals.find(key);
        existing != equality_literals.end()) {
      return existing->second;
    }
    const auto selected = built->model.NewBoolVar().WithName(
        "resource_value_" + std::to_string(equality_index++));
    built->model.AddEquality(variable, value).OnlyEnforceIf(selected);
    built->model.AddNotEqual(variable, value).OnlyEnforceIf(selected.Not());
    equality_literals.emplace(key, selected);
    return selected;
  };
  const auto conjunction_literal =
      [&conjunction_literals, &conjunction_index,
       built](sat::BoolVar left, sat::BoolVar right) -> sat::BoolVar {
    const int left_index = left.index();
    const int right_index = right.index();
    const auto key = std::make_pair(std::min(left_index, right_index),
                                    std::max(left_index, right_index));
    if (const auto existing = conjunction_literals.find(key);
        existing != conjunction_literals.end()) {
      return existing->second;
    }
    const auto selected = built->model.NewBoolVar().WithName(
        "resource_occupancy_" + std::to_string(conjunction_index++));
    built->model.AddBoolAnd({left, right}).OnlyEnforceIf(selected);
    built->model.AddBoolOr({left.Not(), right.Not(), selected});
    conjunction_literals.emplace(key, selected);
    return selected;
  };
  const auto add_resource = [&equality_literal, &conjunction_literal](
                                sat::IntVar variable,
                                const std::vector<std::int64_t>& candidates,
                                const StartChoice& start,
                                std::map<std::pair<Id, Atom>,
                                         std::vector<sat::BoolVar>>* occupancy) {
    const bool fixed = candidates.size() == 1;
    for (const std::int64_t candidate : candidates) {
      const sat::BoolVar selected =
          fixed ? start.selected
                : conjunction_literal(start.selected,
                                      equality_literal(variable, candidate));
      for (const auto& atom : start.atoms) {
        (*occupancy)[{static_cast<Id>(candidate), atom}].push_back(selected);
      }
    }
  };

  for (const auto& activity : built->activities) {
    for (const auto& start : activity.starts) {
      add_resource(activity.teacher, activity.teacher_candidates, start,
                   &teacher_occupancy);
      add_resource(activity.room, activity.room_candidates, start,
                   &room_occupancy);
    }
  }
  const auto add_at_most_one = [built](const auto& occupancy) {
    for (const auto& [resource_and_atom, literals] : occupancy) {
      static_cast<void>(resource_and_atom);
      if (literals.size() >= 2) {
        built->model.AddAtMostOne(literals);
      }
    }
  };
  add_at_most_one(teacher_occupancy);
  add_at_most_one(room_occupancy);
}

std::optional<Failure> BuildBaseModel(
    const protocol::SchedulingProblemSnapshot& problem, BuiltModel* built) {
  std::unordered_map<Id, const protocol::WeekPattern*> week_patterns;
  for (const auto& pattern : problem.week_patterns()) {
    week_patterns.emplace(pattern.week_pattern_id(), &pattern);
  }
  std::unordered_map<Id, const protocol::CalendarTimeslot*> timeslots;
  for (const auto& slot : problem.timeslots()) {
    timeslots.emplace(slot.timeslot_id(), &slot);
  }
  std::unordered_map<Id, const protocol::Room*> rooms;
  for (const auto& room : problem.rooms()) {
    rooms.emplace(room.room_id(), &room);
  }
  std::unordered_map<Id, const protocol::Teacher*> teachers;
  for (const auto& teacher : problem.teachers()) {
    teachers.emplace(teacher.teacher_id(), &teacher);
  }
  std::unordered_map<Id, const protocol::MeetingPattern*> patterns;
  for (const auto& pattern : problem.meeting_patterns()) {
    patterns.emplace(pattern.meeting_pattern_id(), &pattern);
  }

  for (const auto& binding : problem.section_room_bindings()) {
    std::vector<Id> filtered;
    for (const Id room_id : RoomCandidates(binding)) {
      if (RoomMeetsRequirements(*rooms.at(room_id), binding)) {
        filtered.push_back(room_id);
      }
    }
    auto candidates = ToSortedDomain(filtered);
    const bool fixed =
        binding.policy().policy_case() != protocol::RoomPolicy::kFlexible;
    if (candidates.empty()) {
      candidates.push_back(0);
      built->model.AddEquality(built->model.NewConstant(0), 1);
    }
    auto variable = built->model
                        .NewIntVar(operations_research::Domain::FromValues(candidates))
                        .WithName("binding_room_" +
                                  std::to_string(binding.binding_id()));
    built->bindings.emplace(
        binding.binding_id(),
        BindingVariables{&binding, std::move(candidates), variable, fixed});
  }

  built->activities.reserve(static_cast<std::size_t>(problem.activities_size()));
  for (const auto& activity : problem.activities()) {
    auto teacher_candidates = ToSortedDomain(TeacherCandidates(activity));
    const auto& binding = built->bindings.at(activity.section_room_binding_id());
    auto room_candidates = binding.candidates;
    auto teacher_binding = built->teacher_bindings.find(
        activity.teacher_binding_id());
    if (teacher_binding == built->teacher_bindings.end()) {
      auto teacher = built->model
                         .NewIntVar(operations_research::Domain::FromValues(
                             teacher_candidates))
                         .WithName("teacher_binding_" +
                                   std::to_string(activity.teacher_binding_id()));
      teacher_binding = built->teacher_bindings
                            .emplace(activity.teacher_binding_id(),
                                     TeacherBindingVariables{teacher_candidates,
                                                             teacher})
                            .first;
    }
    auto teacher = teacher_binding->second.teacher;
    sat::IntVar room = binding.fixed_across_activities
                           ? binding.room
                           : built->model
                                 .NewIntVar(operations_research::Domain::FromValues(
                                     room_candidates))
                                 .WithName("room_" +
                                           std::to_string(activity.activity_id()));
    ActivityVariables variables{&activity, {}, std::move(teacher_candidates),
                                std::move(room_candidates), teacher, room};
    std::vector<sat::BoolVar> start_literals;
    for (const Id start_id : activity.allowed_start_timeslot_ids()) {
      StartChoice choice;
      choice.timeslot_id = start_id;
      choice.day = timeslots.at(start_id)->day_index();
      choice.selected =
          built->model.NewBoolVar().WithName("activity_" +
                                             std::to_string(activity.activity_id()) +
                                             "_start_" + std::to_string(start_id));
      if (const auto failure = ExpandStart(
              activity, *patterns.at(activity.meeting_pattern_id()), timeslots,
              week_patterns, start_id, &choice.occupied_timeslot_ids,
              &choice.atoms)) {
        return failure;
      }
      start_literals.push_back(choice.selected);
      variables.starts.push_back(std::move(choice));
    }
    built->model.AddExactlyOne(start_literals);

    for (const auto& start : variables.starts) {
      for (const std::int64_t teacher_id : variables.teacher_candidates) {
        const auto& availability =
            teachers.at(static_cast<Id>(teacher_id))->available_timeslot_ids();
        const bool available = std::all_of(
            start.occupied_timeslot_ids.begin(),
            start.occupied_timeslot_ids.end(), [&availability](Id slot) {
              return std::find(availability.begin(), availability.end(), slot) !=
                     availability.end();
            });
        if (!available) {
          built->model.AddNotEqual(variables.teacher, teacher_id)
              .OnlyEnforceIf(start.selected);
        }
      }
      for (const std::int64_t room_id : variables.room_candidates) {
        if (room_id == 0) {
          continue;
        }
        const auto& availability =
            rooms.at(static_cast<Id>(room_id))->available_timeslot_ids();
        const bool available = std::all_of(
            start.occupied_timeslot_ids.begin(),
            start.occupied_timeslot_ids.end(), [&availability](Id slot) {
              return std::find(availability.begin(), availability.end(), slot) !=
                     availability.end();
            });
        if (!available) {
          built->model.AddNotEqual(variables.room, room_id)
              .OnlyEnforceIf(start.selected);
        }
      }
    }
    built->activities.push_back(std::move(variables));
  }

  std::unordered_map<Id, std::size_t> activity_index;
  for (std::size_t index = 0; index < built->activities.size(); ++index) {
    activity_index.emplace(built->activities[index].activity->activity_id(), index);
  }
  for (const auto& edge : problem.student_conflicts().edges()) {
    AddStudentConflictGroup(
        {edge.left_activity_id(), edge.right_activity_id()}, activity_index,
        built);
  }
  for (const auto& clique : problem.student_conflicts().cliques()) {
    AddStudentConflictGroup(
        std::vector<Id>(clique.activity_ids().begin(),
                        clique.activity_ids().end()),
        activity_index, built);
  }
  AddResourceOccupancyConstraints(built);

  for (const auto& pattern : problem.meeting_patterns()) {
    std::vector<std::size_t> indices;
    for (const Id activity_id : pattern.activity_ids()) {
      indices.push_back(activity_index.at(activity_id));
    }
    if (pattern.minimum_gap_days() > 0) {
      for (std::size_t left = 0; left < indices.size(); ++left) {
        for (std::size_t right = left + 1; right < indices.size(); ++right) {
          for (const auto& left_start : built->activities[indices[left]].starts) {
            for (const auto& right_start : built->activities[indices[right]].starts) {
              const Id gap = left_start.day > right_start.day
                                 ? left_start.day - right_start.day
                                 : right_start.day - left_start.day;
              if (SharesTeachingWeek(left_start, right_start) &&
                  gap < pattern.minimum_gap_days()) {
                built->model.AddAtMostOne(
                    {left_start.selected, right_start.selected});
              }
            }
          }
        }
      }
    }
    std::map<std::pair<Id, Id>, std::vector<std::pair<sat::BoolVar, Id>>>
        per_day;
    for (const std::size_t index : indices) {
      const auto& activity = built->activities[index];
      for (const auto& start : activity.starts) {
        std::set<std::pair<Id, Id>> weeks_and_days;
        for (const auto& atom : start.atoms) {
          weeks_and_days.emplace(atom.week, atom.day);
        }
        for (const auto& key : weeks_and_days) {
          per_day[key].emplace_back(start.selected,
                                    activity.activity->duration_periods());
        }
      }
    }
    for (const auto& [key, terms] : per_day) {
      static_cast<void>(key);
      sat::LinearExpr expression;
      for (const auto& [literal, duration] : terms) {
        expression += sat::LinearExpr::Term(literal, duration);
      }
      built->model.AddLessOrEqual(expression,
                                  pattern.maximum_periods_per_day());
    }
  }

  std::unordered_map<std::string, const protocol::ConstraintGroup*> groups;
  for (const auto& group : problem.constraint_groups()) {
    groups.emplace(group.group_id(), &group);
  }
  std::unordered_map<std::string, sat::BoolVar> assumption_variables;
  for (const auto& lock : problem.locks()) {
    const auto* group = groups.at(lock.constraint_group_id());
    std::optional<sat::BoolVar> assumption;
    if (group->assumption_enabled()) {
      auto existing = assumption_variables.find(group->group_id());
      if (existing == assumption_variables.end()) {
        const auto literal = built->model.NewBoolVar().WithName(
            "assumption_" + group->group_id());
        built->model.AddAssumption(literal);
        existing =
            assumption_variables.emplace(group->group_id(), literal).first;
        built->assumption_groups.emplace(literal.index(), group);
      }
      assumption = existing->second;
    }
    auto& variables =
        built->activities[activity_index.at(lock.assignment().activity_id())];
    auto start = std::find_if(
        variables.starts.begin(), variables.starts.end(),
        [&lock](const StartChoice& choice) {
          return choice.timeslot_id == lock.assignment().start_timeslot_id();
        });
    if (start == variables.starts.end()) {
      if (assumption.has_value()) {
        built->model.AddBoolOr({assumption->Not()});
      } else {
        built->model.AddEquality(built->model.NewConstant(0), 1);
      }
      continue;
    }
    auto start_constraint = built->model.AddEquality(start->selected, 1);
    auto teacher_constraint = built->model.AddEquality(
        variables.teacher, lock.assignment().teacher_id());
    auto room_constraint = built->model.AddEquality(
        variables.room, lock.assignment().room_id());
    if (assumption.has_value()) {
      start_constraint.OnlyEnforceIf(*assumption);
      teacher_constraint.OnlyEnforceIf(*assumption);
      room_constraint.OnlyEnforceIf(*assumption);
    }
  }

  for (const auto& incumbent : problem.incumbent_assignments()) {
    auto& variables =
        built->activities[activity_index.at(incumbent.activity_id())];
    for (const auto& start : variables.starts) {
      built->model.AddHint(start.selected,
                           start.timeslot_id == incumbent.start_timeslot_id());
    }
    if (Contains(variables.teacher_candidates,
                 static_cast<std::int64_t>(incumbent.teacher_id()))) {
      built->model.AddHint(variables.teacher, incumbent.teacher_id());
    }
    if (Contains(variables.room_candidates,
                 static_cast<std::int64_t>(incumbent.room_id()))) {
      built->model.AddHint(variables.room, incumbent.room_id());
    }
  }
  return std::nullopt;
}

std::optional<Failure> BuildObjectives(
    const protocol::SchedulingProblemSnapshot& problem,
    protocol::SolveMode solve_mode, BuiltModel* built) {
  std::unordered_map<Id, const protocol::MeetingAssignment*> incumbents;
  for (const auto& incumbent : problem.incumbent_assignments()) {
    incumbents.emplace(incumbent.activity_id(), &incumbent);
  }
  std::unordered_map<Id, sat::BoolVar> changed_variables;
  std::unordered_map<Id, ActivityVariables*> activities;
  for (auto& activity : built->activities) {
    activities.emplace(activity.activity->activity_id(), &activity);
  }
  std::size_t course_distribution_literal_index = 0;
  std::map<std::pair<Id, Id>, sat::BoolVar> activity_day_literals;
  const auto changed_for_activity =
      [&incumbents, &changed_variables,
       built](ActivityVariables& variables) -> sat::BoolVar {
    const Id activity_id = variables.activity->activity_id();
    if (const auto existing = changed_variables.find(activity_id);
        existing != changed_variables.end()) {
      return existing->second;
    }
    const auto incumbent = incumbents.find(activity_id);
    if (incumbent == incumbents.end()) {
      const auto changed = built->model.TrueVar();
      changed_variables.emplace(activity_id, changed);
      return changed;
    }
    auto matching_start = std::find_if(
        variables.starts.begin(), variables.starts.end(),
        [&incumbent](const StartChoice& start) {
          return start.timeslot_id == incumbent->second->start_timeslot_id();
        });
    const sat::BoolVar same_start =
        matching_start == variables.starts.end() ? built->model.FalseVar()
                                                  : matching_start->selected;
    const auto same_teacher = built->model.NewBoolVar().WithName(
        "same_teacher_" + std::to_string(activity_id));
    built->model
        .AddEquality(variables.teacher, incumbent->second->teacher_id())
        .OnlyEnforceIf(same_teacher);
    built->model
        .AddNotEqual(variables.teacher, incumbent->second->teacher_id())
        .OnlyEnforceIf(same_teacher.Not());
    const auto same_room = built->model.NewBoolVar().WithName(
        "same_room_" + std::to_string(activity_id));
    built->model.AddEquality(variables.room, incumbent->second->room_id())
        .OnlyEnforceIf(same_room);
    built->model.AddNotEqual(variables.room, incumbent->second->room_id())
        .OnlyEnforceIf(same_room.Not());
    const auto unchanged = built->model.NewBoolVar().WithName(
        "unchanged_" + std::to_string(activity_id));
    built->model.AddImplication(unchanged, same_start);
    built->model.AddImplication(unchanged, same_teacher);
    built->model.AddImplication(unchanged, same_room);
    built->model.AddBoolOr(
        {same_start.Not(), same_teacher.Not(), same_room.Not(), unchanged});
    const auto changed = unchanged.Not();
    changed_variables.emplace(activity_id, changed);
    return changed;
  };
  const auto course_distribution =
      [&activities, &activity_day_literals,
       &course_distribution_literal_index, built](
          const protocol::SchedulingProblemSnapshot& snapshot,
          const std::unordered_set<Id>& scope) -> sat::LinearExpr {
    const auto day_literals = [&activity_day_literals, built](
                                  ActivityVariables* activity) {
      std::map<Id, sat::BoolVar> result;
      std::map<Id, std::vector<sat::BoolVar>> starts_by_day;
      for (const auto& start : activity->starts) {
        starts_by_day[start.day].push_back(start.selected);
      }
      for (const auto& [day, starts] : starts_by_day) {
        const auto key =
            std::make_pair(activity->activity->activity_id(), day);
        auto existing = activity_day_literals.find(key);
        if (existing == activity_day_literals.end()) {
          const auto selected = built->model.NewBoolVar().WithName(
              "activity_" +
              std::to_string(activity->activity->activity_id()) + "_day_" +
              std::to_string(day));
          sat::LinearExpr expression;
          for (const auto& start : starts) {
            expression += start;
          }
          built->model.AddEquality(selected, expression);
          existing = activity_day_literals.emplace(key, selected).first;
        }
        result.emplace(day, existing->second);
      }
      return result;
    };
    sat::LinearExpr expression;
    for (const auto& pattern : snapshot.meeting_patterns()) {
      std::vector<ActivityVariables*> members;
      for (const Id activity_id : pattern.activity_ids()) {
        if (scope.empty() || scope.contains(activity_id)) {
          members.push_back(activities.at(activity_id));
        }
      }
      for (std::size_t left = 0; left < members.size(); ++left) {
        const auto left_days = day_literals(members[left]);
        for (std::size_t right = left + 1; right < members.size(); ++right) {
          const auto right_days = day_literals(members[right]);
          for (const auto& [left_day, left_selected] : left_days) {
            for (const auto& [right_day, right_selected] : right_days) {
              const Id gap = left_day > right_day ? left_day - right_day
                                                  : right_day - left_day;
              const std::int64_t penalty = gap == 0 ? 4 : (gap == 1 ? 1 : 0);
              if (penalty == 0) {
                continue;
              }
              const auto selected_together = built->model.NewBoolVar().WithName(
                  "course_distribution_pair_" +
                  std::to_string(course_distribution_literal_index++));
              built->model
                  .AddBoolAnd({left_selected, right_selected})
                  .OnlyEnforceIf(selected_together);
              built->model.AddBoolOr({left_selected.Not(),
                                      right_selected.Not(),
                                      selected_together});
              expression += sat::LinearExpr::Term(selected_together, penalty);
            }
          }
        }
      }
    }
    return expression;
  };

  std::vector<const protocol::ObjectiveTierDefinition*> definitions;
  for (const auto& tier : problem.objective_tiers()) {
    definitions.push_back(&tier);
  }
  std::sort(definitions.begin(), definitions.end(), [](const auto* left,
                                                        const auto* right) {
    return std::make_tuple(left->priority(), left->tier_id()) <
           std::make_tuple(right->priority(), right->tier_id());
  });
  if (solve_mode == protocol::SOLVE_MODE_REPAIR) {
    TierExpression repair_tier;
    repair_tier.tier_id = "repair:changed-assignments";
    repair_tier.priority = 0;
    MetricExpression repair_metric;
    repair_metric.kind = protocol::OBJECTIVE_METRIC_KIND_REPAIR_CHANGES;
    for (auto& activity : built->activities) {
      repair_metric.expression += changed_for_activity(activity);
    }
    repair_tier.expression = repair_metric.expression;
    repair_tier.metrics.push_back(std::move(repair_metric));
    built->tiers.push_back(std::move(repair_tier));
  }
  for (const auto* definition : definitions) {
    TierExpression tier;
    tier.tier_id = definition->tier_id();
    tier.priority = definition->priority();
    for (const auto& metric : definition->metrics()) {
      if (metric.metric_kind() !=
              protocol::OBJECTIVE_METRIC_KIND_REPAIR_CHANGES &&
          metric.metric_kind() !=
              protocol::OBJECTIVE_METRIC_KIND_LAYOUT_STABILITY &&
          metric.metric_kind() !=
              protocol::OBJECTIVE_METRIC_KIND_COURSE_DISTRIBUTION) {
        return Failure{protocol::SOLVER_STATUS_INVALID_MODEL,
                       "WORKER.UNSUPPORTED_OBJECTIVE_METRIC"};
      }
      if (!metric.parameters().empty()) {
        return Failure{protocol::SOLVER_STATUS_INVALID_MODEL,
                       "WORKER.UNSUPPORTED_OBJECTIVE_PARAMETERS"};
      }
      std::unordered_set<Id> scope;
      for (const auto& reference : metric.scope()) {
        if (reference.entity_kind() != protocol::ENTITY_KIND_ACTIVITY) {
          return Failure{protocol::SOLVER_STATUS_INVALID_MODEL,
                         "WORKER.UNSUPPORTED_OBJECTIVE_SCOPE"};
        }
        scope.insert(reference.compact_id());
      }
      MetricExpression metric_expression;
      metric_expression.kind = metric.metric_kind();
      if (metric.metric_kind() ==
          protocol::OBJECTIVE_METRIC_KIND_COURSE_DISTRIBUTION) {
        metric_expression.expression = course_distribution(problem, scope);
      } else {
        for (auto& activity : built->activities) {
          if (scope.empty() || scope.contains(activity.activity->activity_id())) {
            metric_expression.expression += changed_for_activity(activity);
          }
        }
      }
      sat::LinearExpr weighted = metric_expression.expression;
      weighted *= static_cast<std::int64_t>(metric.weight_within_tier());
      tier.expression += weighted;
      tier.metrics.push_back(std::move(metric_expression));
    }
    built->tiers.push_back(std::move(tier));
  }
  return std::nullopt;
}

std::optional<Failure> BuildModel(
    const protocol::SchedulingProblemSnapshot& problem,
    protocol::SolveMode solve_mode, BuiltModel* built) {
  if (const auto failure = BuildBaseModel(problem, built)) {
    return failure;
  }
  return BuildObjectives(problem, solve_mode, built);
}

sat::SatParameters OrToolsParameters(const protocol::SolverParameters& input,
                                     double remaining_seconds) {
  sat::SatParameters parameters;
  parameters.set_max_time_in_seconds(std::max(0.001, remaining_seconds));
  parameters.set_num_search_workers(static_cast<std::int32_t>(input.worker_count()));
  parameters.set_random_seed(static_cast<std::int32_t>(input.seed() & 0x7fffffffULL));
  parameters.set_log_search_progress(false);
  parameters.set_log_to_stdout(false);
  parameters.set_relative_gap_limit(
      static_cast<double>(input.relative_gap_limit_ppm()) / 1'000'000.0);
  if (input.memory_limit_bytes() > 0) {
    const auto mebibytes = std::max<std::uint64_t>(
        1, (input.memory_limit_bytes() + (1U << 20U) - 1U) >> 20U);
    parameters.set_max_memory_in_mb(static_cast<std::int64_t>(std::min<
                                     std::uint64_t>(
        mebibytes,
        static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max()))));
  }
  if (input.reproducible()) {
    parameters.set_num_search_workers(1);
    parameters.set_randomize_search(false);
  }
  return parameters;
}

void AccumulateStatistics(const sat::CpSolverResponse& response,
                          SolveAggregate* aggregate) {
  aggregate->deterministic_time += response.deterministic_time();
  aggregate->conflicts += NonNegative(response.num_conflicts());
  aggregate->branches += NonNegative(response.num_branches());
  aggregate->propagations +=
      NonNegative(response.num_binary_propagations()) +
      NonNegative(response.num_integer_propagations());
}

SolveAggregate RunLexicographicSolve(
    BuiltModel* built, const protocol::SolverParameters& parameters,
    protocol::ObjectiveBreakdown* objective) {
  SolveAggregate aggregate;
  const auto started = Clock::now();
  const double total_seconds =
      static_cast<double>(parameters.time_limit_millis()) / 1000.0;
  const std::size_t passes = std::max<std::size_t>(1, built->tiers.size());
  for (std::size_t pass = 0; pass < passes; ++pass) {
    const auto elapsed = std::chrono::duration<double>(Clock::now() - started).count();
    const double remaining = total_seconds - elapsed;
    if (remaining <= 0.0) {
      aggregate.timed_out = true;
      aggregate.all_tiers_optimal = false;
      break;
    }
    built->model.ClearObjective();
    if (!built->tiers.empty()) {
      built->model.Minimize(built->tiers[pass].expression);
    }
    const auto response = sat::SolveWithParameters(
        built->model.Build(), OrToolsParameters(parameters, remaining));
    AccumulateStatistics(response, &aggregate);
    if (response.status() == sat::CpSolverStatus::MODEL_INVALID ||
        response.status() == sat::CpSolverStatus::INFEASIBLE ||
        response.status() == sat::CpSolverStatus::UNKNOWN) {
      if (!aggregate.has_solution) {
        aggregate.final_response = response;
      }
      if (response.status() == sat::CpSolverStatus::UNKNOWN) {
        const auto now_elapsed =
            std::chrono::duration<double>(Clock::now() - started).count();
        aggregate.timed_out = now_elapsed >= total_seconds * 0.95;
      }
      aggregate.all_tiers_optimal = false;
      break;
    }
    aggregate.has_solution = true;
    aggregate.final_response = response;
    if (!built->tiers.empty()) {
      auto* tier_result = objective->add_tiers();
      const auto& tier = built->tiers[pass];
      tier_result->set_tier_id(tier.tier_id);
      tier_result->set_priority(tier.priority);
      const auto value = sat::SolutionIntegerValue(response, tier.expression);
      tier_result->set_value(value);
      tier_result->set_best_bound(
          response.status() == sat::CpSolverStatus::OPTIMAL
              ? value
              : RoundedInteger(response.best_objective_bound()));
      for (const auto& metric : tier.metrics) {
        auto* metric_result = tier_result->add_metrics();
        metric_result->set_metric_kind(metric.kind);
        metric_result->set_value(
            sat::SolutionIntegerValue(response, metric.expression));
      }
      if (response.status() == sat::CpSolverStatus::OPTIMAL) {
        built->model.AddEquality(tier.expression, value);
      }
    }
    if (response.status() != sat::CpSolverStatus::OPTIMAL) {
      aggregate.all_tiers_optimal = false;
      break;
    }
  }
  return aggregate;
}

void AppendUint32(std::string* bytes, std::uint32_t value) {
  bytes->push_back(static_cast<char>((value >> 24U) & 0xffU));
  bytes->push_back(static_cast<char>((value >> 16U) & 0xffU));
  bytes->push_back(static_cast<char>((value >> 8U) & 0xffU));
  bytes->push_back(static_cast<char>(value & 0xffU));
}

void SetOutputHash(protocol::SolveResponse* response) {
  std::string canonical;
  canonical.reserve(static_cast<std::size_t>(response->assignments_size() * 20 +
                                             response->section_room_assignments_size() *
                                                 8));
  for (const auto& assignment : response->assignments()) {
    AppendUint32(&canonical, assignment.activity_id());
    AppendUint32(&canonical, assignment.start_timeslot_id());
    AppendUint32(&canonical, assignment.room_id());
    AppendUint32(&canonical, assignment.teacher_id());
    AppendUint32(&canonical, assignment.duration_periods());
  }
  for (const auto& binding : response->section_room_assignments()) {
    AppendUint32(&canonical, binding.section_room_binding_id());
    AppendUint32(&canonical, binding.room_id());
  }
  const auto digest = Sha256(canonical);
  response->set_output_hash(digest.data(), digest.size());
}

bool ExtractAssignments(const BuiltModel& built,
                        const sat::CpSolverResponse& solution,
                        protocol::SolveResponse* response) {
  for (const auto& activity : built.activities) {
    const auto selected = std::find_if(
        activity.starts.begin(), activity.starts.end(),
        [&solution](const StartChoice& start) {
          return sat::SolutionBooleanValue(solution, start.selected);
        });
    if (selected == activity.starts.end()) {
      response->clear_assignments();
      response->clear_section_room_assignments();
      return false;
    }
    auto* assignment = response->add_assignments();
    assignment->set_activity_id(activity.activity->activity_id());
    assignment->set_start_timeslot_id(selected->timeslot_id);
    assignment->set_room_id(static_cast<Id>(
        sat::SolutionIntegerValue(solution, activity.room)));
    assignment->set_teacher_id(static_cast<Id>(
        sat::SolutionIntegerValue(solution, activity.teacher)));
    assignment->set_duration_periods(activity.activity->duration_periods());
  }
  for (const auto& [binding_id, binding] : built.bindings) {
    if (!binding.fixed_across_activities) {
      continue;
    }
    auto* assignment = response->add_section_room_assignments();
    assignment->set_section_room_binding_id(binding_id);
    assignment->set_room_id(static_cast<Id>(
        sat::SolutionIntegerValue(solution, binding.room)));
  }
  SetOutputHash(response);
  return true;
}

void AddAssumptionDiagnostics(const BuiltModel& built,
                              const sat::CpSolverResponse& solution,
                              protocol::SolveResponse* response) {
  if (!response->effective_parameters().collect_diagnostics()) {
    return;
  }
  for (const int literal : solution.sufficient_assumptions_for_infeasibility()) {
    const int positive = literal >= 0 ? literal : -literal - 1;
    const auto group = built.assumption_groups.find(positive);
    if (group == built.assumption_groups.end()) {
      continue;
    }
    auto* diagnostic = response->add_diagnostic_groups();
    diagnostic->set_group_id(group->second->group_id());
    diagnostic->set_problem_code(group->second->problem_code());
    diagnostic->set_signal(
        protocol::DIAGNOSTIC_SIGNAL_SUFFICIENT_ASSUMPTIONS);
    for (const auto& reference : group->second->related_entities()) {
      diagnostic->add_related_entities()->CopyFrom(reference);
    }
    for (const auto& [key, value] : group->second->parameters()) {
      (*diagnostic->mutable_parameters())[key] = value;
    }
  }
}

protocol::SolverEnvelope SolveValidated(
    const protocol::SolverEnvelope& request_envelope,
    const protocol::SolveRequest& request) {
  const auto& parameters = request.parameters();
  const auto& problem = request.problem();
  auto envelope = BaseResponse(request_envelope, &parameters, &problem);
  auto* response = envelope.mutable_solve_response();

  BuiltModel built;
  if (const auto failure = BuildModel(
          problem, static_cast<protocol::SolveMode>(parameters.mode()), &built)) {
    return FailureResponse(request_envelope, &parameters, &problem, *failure);
  }
  const std::string model_error = sat::ValidateCpModel(built.model.Build());
  if (!model_error.empty()) {
    return FailureResponse(
        request_envelope, &parameters, &problem,
        Failure{protocol::SOLVER_STATUS_INVALID_MODEL,
                "WORKER.CP_SAT_MODEL_INVALID"});
  }

  const auto started = Clock::now();
  auto aggregate = RunLexicographicSolve(
      &built, parameters, response->mutable_objective());
  const auto elapsed_millis =
      std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started)
          .count();
  auto* statistics = response->mutable_statistics();
  statistics->set_wall_time_millis(NonNegative(elapsed_millis));
  statistics->set_deterministic_time(aggregate.deterministic_time);
  statistics->set_conflicts(aggregate.conflicts);
  statistics->set_branches(aggregate.branches);
  statistics->set_propagations(aggregate.propagations);
  statistics->set_worker_count(parameters.worker_count());
  statistics->set_seed(parameters.seed());

  const auto status = aggregate.final_response.status();
  if (aggregate.has_solution) {
    if (!ExtractAssignments(built, aggregate.final_response, response)) {
      response->clear_objective();
      response->set_status(protocol::SOLVER_STATUS_INTERNAL_ERROR);
      response->set_status_detail_code("WORKER.SOLUTION_EXTRACTION_FAILED");
      return envelope;
    }
    if (aggregate.all_tiers_optimal && status == sat::CpSolverStatus::OPTIMAL) {
      response->set_status(protocol::SOLVER_STATUS_OPTIMAL);
      response->set_status_detail_code("SOLVER.OPTIMAL");
    } else {
      response->set_status(protocol::SOLVER_STATUS_FEASIBLE);
      response->set_status_detail_code(
          aggregate.timed_out ? "SOLVER.FEASIBLE_TIME_LIMIT"
                              : "SOLVER.FEASIBLE_NOT_PROVEN_OPTIMAL");
    }
    return envelope;
  }
  response->clear_objective();
  switch (status) {
    case sat::CpSolverStatus::INFEASIBLE:
      response->set_status(protocol::SOLVER_STATUS_PROVEN_INFEASIBLE);
      response->set_status_detail_code("SOLVER.PROVEN_INFEASIBLE");
      AddAssumptionDiagnostics(built, aggregate.final_response, response);
      break;
    case sat::CpSolverStatus::MODEL_INVALID:
      response->set_status(protocol::SOLVER_STATUS_INVALID_MODEL);
      response->set_status_detail_code("SOLVER.CP_SAT_MODEL_INVALID");
      break;
    case sat::CpSolverStatus::UNKNOWN:
      response->set_status(aggregate.timed_out ? protocol::SOLVER_STATUS_TIMEOUT
                                               : protocol::SOLVER_STATUS_UNKNOWN);
      response->set_status_detail_code(aggregate.timed_out
                                           ? "SOLVER.TIME_LIMIT_REACHED"
                                           : "SOLVER.UNKNOWN");
      break;
    case sat::CpSolverStatus::FEASIBLE:
    case sat::CpSolverStatus::OPTIMAL:
      response->set_status(protocol::SOLVER_STATUS_INTERNAL_ERROR);
      response->set_status_detail_code("WORKER.SOLUTION_EXTRACTION_FAILED");
      break;
    default:
      response->set_status(protocol::SOLVER_STATUS_UNKNOWN);
      response->set_status_detail_code("SOLVER.UNKNOWN");
      break;
  }
  return envelope;
}

}  // namespace

protocol::SolverEnvelope SolveEnvelope(
    const protocol::SolverEnvelope& request_envelope) {
  const protocol::SolveRequest* request =
      request_envelope.has_solve_request() ? &request_envelope.solve_request()
                                           : nullptr;
  const protocol::SolverParameters* parameters =
      request != nullptr && request->has_parameters() ? &request->parameters()
                                                       : nullptr;
  const protocol::SchedulingProblemSnapshot* problem =
      request != nullptr && request->has_problem() ? &request->problem() : nullptr;
  try {
    if (request_envelope.protocol_version() != kProtocolVersion) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.UNSUPPORTED_PROTOCOL_VERSION"});
    }
    if (IsBlank(request_envelope.request_id()) ||
        request_envelope.request_id().size() > 128U) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.INVALID_REQUEST_ID"});
    }
    if (request == nullptr) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.EXPECTED_SOLVE_REQUEST"});
    }
    if (!request->has_required_engine_version() ||
        IsBlank(request->required_engine_version().engine_name()) ||
        IsBlank(request->required_engine_version().engine_version()) ||
        IsBlank(request->required_engine_version().adapter_version())) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.REQUIRED_ENGINE_VERSION_MISSING"});
    }
    const auto& required = request->required_engine_version();
    if (required.engine_name() != kEngineName ||
        required.engine_version() != kEngineVersion ||
        required.adapter_version() != kAdapterVersion) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.ENGINE_VERSION_MISMATCH"});
    }
    if (parameters == nullptr) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.SOLVER_PARAMETERS_MISSING"});
    }
    if (const auto failure = ValidateParameters(*parameters)) {
      return FailureResponse(request_envelope, parameters, problem, *failure);
    }
    if (problem == nullptr) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.PROBLEM_SNAPSHOT_MISSING"});
    }
    if ((parameters->mode() == protocol::SOLVE_MODE_IMPROVE ||
         parameters->mode() == protocol::SOLVE_MODE_REPAIR) &&
        problem->incumbent_assignments().empty()) {
      return FailureResponse(
          request_envelope, parameters, problem,
          Failure{protocol::SOLVER_STATUS_INVALID_INPUT,
                  "WORKER.INCUMBENT_REQUIRED_FOR_MODE"});
    }
    if (const auto failure = ValidateProblemStructure(*problem)) {
      return FailureResponse(request_envelope, parameters, problem, *failure);
    }
    return SolveValidated(request_envelope, *request);
  } catch (const std::exception&) {
    return FailureResponse(
        request_envelope, parameters, problem,
        Failure{protocol::SOLVER_STATUS_INTERNAL_ERROR,
                "WORKER.INTERNAL_EXCEPTION"});
  } catch (...) {
    return FailureResponse(
        request_envelope, parameters, problem,
        Failure{protocol::SOLVER_STATUS_INTERNAL_ERROR,
                "WORKER.INTERNAL_UNKNOWN_EXCEPTION"});
  }
}

int RunOneShot(std::istream& input, std::ostream& output,
               std::ostream& diagnostics) {
  protocol::SolverEnvelope request;
  std::string error;
  if (!ReadFrame(input, &request, &error)) {
    diagnostics << error << '\n';
    return 2;
  }
  if (input.peek() != std::char_traits<char>::eof()) {
    diagnostics << "FRAME_TRAILING_BYTES" << '\n';
    return 2;
  }
  const auto response = SolveEnvelope(request);
  if (!WriteFrame(output, response, &error)) {
    diagnostics << error << '\n';
    return 3;
  }
  return 0;
}

}  // namespace class_schedule::solver
