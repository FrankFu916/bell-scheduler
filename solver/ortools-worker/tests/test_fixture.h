#pragma once

#include <cstdint>
#include <string>

#include "ortools_worker/solver_worker.h"
#include "scheduler/v1/solver.pb.h"

namespace class_schedule::solver::test {

namespace protocol = scheduler::v1;

inline protocol::SolverEnvelope ValidRequest() {
  protocol::SolverEnvelope envelope;
  envelope.set_protocol_version(kProtocolVersion);
  envelope.set_request_id("worker-test-request");
  auto* request = envelope.mutable_solve_request();
  auto* version = request->mutable_required_engine_version();
  version->set_engine_name(kEngineName);
  version->set_engine_version(kEngineVersion);
  version->set_adapter_version(kAdapterVersion);
  version->set_build_revision("test");

  auto* parameters = request->mutable_parameters();
  parameters->set_seed(42);
  parameters->set_mode(protocol::SOLVE_MODE_GENERATE);
  parameters->set_profile(protocol::SOLVER_PROFILE_BALANCED);
  parameters->set_reproducible(true);
  parameters->set_time_limit_millis(2'000);
  parameters->set_worker_count(1);
  parameters->set_collect_diagnostics(true);

  auto* problem = request->mutable_problem();
  problem->set_schema_version(kSnapshotSchemaVersion);
  problem->set_snapshot_hash(std::string(32, 'h'));
  problem->set_project_id("project-test");
  problem->set_project_revision(1);
  problem->set_scenario_id("scenario-test");
  problem->set_scenario_revision(1);

  auto* weeks = problem->add_week_patterns();
  weeks->set_week_pattern_id(1);
  weeks->set_stable_key("all-weeks");
  weeks->add_teaching_week_numbers(1);

  auto* first_slot = problem->add_timeslots();
  first_slot->set_timeslot_id(1);
  first_slot->set_week_pattern_id(1);
  first_slot->set_day_index(1);
  first_slot->set_period_index(1);
  first_slot->set_next_consecutive_timeslot_id(2);
  auto* second_slot = problem->add_timeslots();
  second_slot->set_timeslot_id(2);
  second_slot->set_week_pattern_id(1);
  second_slot->set_day_index(1);
  second_slot->set_period_index(2);
  auto* third_slot = problem->add_timeslots();
  third_slot->set_timeslot_id(3);
  third_slot->set_week_pattern_id(1);
  third_slot->set_day_index(2);
  third_slot->set_period_index(1);

  for (std::uint32_t room_id = 1; room_id <= 2; ++room_id) {
    auto* room = problem->add_rooms();
    room->set_room_id(room_id);
    room->set_capacity(40);
    room->set_building_id(1);
    room->add_available_timeslot_ids(1);
    room->add_available_timeslot_ids(2);
    room->add_available_timeslot_ids(3);
  }
  for (std::uint32_t teacher_id = 1; teacher_id <= 2; ++teacher_id) {
    auto* teacher = problem->add_teachers();
    teacher->set_teacher_id(teacher_id);
    teacher->add_available_timeslot_ids(1);
    teacher->add_available_timeslot_ids(2);
    teacher->add_available_timeslot_ids(3);
  }

  auto* binding = problem->add_section_room_bindings();
  binding->set_binding_id(1);
  binding->set_section_id(1);
  binding->set_required_capacity(1);
  binding->mutable_policy()->mutable_fixed()->set_room_id(1);

  auto* activity = problem->add_activities();
  activity->set_activity_id(1);
  activity->set_meeting_demand_id(1);
  activity->set_subject_id(1);
  activity->set_section_id(1);
  activity->set_duration_periods(1);
  activity->add_allowed_start_timeslot_ids(1);
  activity->add_allowed_start_timeslot_ids(2);
  activity->add_allowed_start_timeslot_ids(3);
  activity->mutable_teacher_policy()->mutable_fixed_teacher()->set_teacher_id(1);
  activity->set_section_room_binding_id(1);
  activity->set_meeting_pattern_id(1);
  activity->set_teacher_binding_id(1);

  auto* pattern = problem->add_meeting_patterns();
  pattern->set_meeting_pattern_id(1);
  pattern->add_activity_ids(1);
  pattern->add_duration_periods(1);
  pattern->set_minimum_gap_days(0);
  pattern->set_maximum_periods_per_day(1);
  pattern->set_forbid_cross_break(true);
  return envelope;
}

inline void AddSecondIndependentActivity(protocol::SchedulingProblemSnapshot* problem,
                                         std::uint32_t only_start) {
  auto* binding = problem->add_section_room_bindings();
  binding->set_binding_id(2);
  binding->set_section_id(2);
  binding->set_required_capacity(1);
  binding->mutable_policy()->mutable_fixed()->set_room_id(2);

  auto* activity = problem->add_activities();
  activity->set_activity_id(2);
  activity->set_meeting_demand_id(2);
  activity->set_subject_id(2);
  activity->set_section_id(2);
  activity->set_duration_periods(1);
  activity->add_allowed_start_timeslot_ids(only_start);
  activity->mutable_teacher_policy()->mutable_fixed_teacher()->set_teacher_id(2);
  activity->set_section_room_binding_id(2);
  activity->set_meeting_pattern_id(2);
  activity->set_teacher_binding_id(2);

  auto* pattern = problem->add_meeting_patterns();
  pattern->set_meeting_pattern_id(2);
  pattern->add_activity_ids(2);
  pattern->add_duration_periods(1);
  pattern->set_maximum_periods_per_day(1);
  pattern->set_forbid_cross_break(true);
}

}  // namespace class_schedule::solver::test
