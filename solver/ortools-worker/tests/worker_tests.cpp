#include <iostream>
#include <string>

#include "ortools_worker/solver_worker.h"
#include "test_fixture.h"

namespace {

namespace protocol = scheduler::v1;
using class_schedule::solver::SolveEnvelope;
using class_schedule::solver::test::AddSecondIndependentActivity;
using class_schedule::solver::test::ValidRequest;

int failures = 0;

void Expect(bool condition, const std::string& message) {
  if (!condition) {
    std::cerr << "FAILED: " << message << '\n';
    ++failures;
  }
}

void ValidModelProducesAnOptimalAssignment() {
  const auto response_envelope = SolveEnvelope(ValidRequest());
  Expect(response_envelope.has_solve_response(), "valid response payload");
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_OPTIMAL,
         "one-activity feasibility model is optimal");
  Expect(response.assignments_size() == 1, "one assignment returned");
  Expect(response.assignments(0).teacher_id() == 1,
         "fixed teacher is preserved");
  Expect(response.assignments(0).room_id() == 1, "fixed room is preserved");
  Expect(response.output_hash().size() == 32, "output hash is populated");
}

void ReproducibleModeReturnsTheSameCanonicalAssignment() {
  const auto first = SolveEnvelope(ValidRequest()).solve_response();
  const auto second = SolveEnvelope(ValidRequest()).solve_response();
  Expect(first.status() == second.status(), "reproducible status is stable");
  Expect(first.assignments_size() == second.assignments_size(),
         "reproducible assignment count is stable");
  for (int index = 0;
       index < std::min(first.assignments_size(), second.assignments_size());
       ++index) {
    Expect(first.assignments(index).SerializeAsString() ==
               second.assignments(index).SerializeAsString(),
           "reproducible assignments are stable for the regression fixture");
  }
  Expect(first.output_hash() == second.output_hash(),
         "reproducible canonical output hash is stable");
}

void StudentConflictIsHard() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  auto* first = problem->mutable_activities(0);
  first->clear_allowed_start_timeslot_ids();
  first->add_allowed_start_timeslot_ids(1);
  AddSecondIndependentActivity(problem, 1);
  auto* edge = problem->mutable_student_conflicts()->add_edges();
  edge->set_left_activity_id(1);
  edge->set_right_activity_id(2);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
         "student overlap is proven infeasible");
  Expect(response.assignments().empty(),
         "infeasible response carries no assignments");
}

void StudentConflictCliqueIsHard() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  auto* first = problem->mutable_activities(0);
  first->clear_allowed_start_timeslot_ids();
  first->add_allowed_start_timeslot_ids(1);
  AddSecondIndependentActivity(problem, 1);
  auto* clique = problem->mutable_student_conflicts()->add_cliques();
  clique->add_activity_ids(1);
  clique->add_activity_ids(2);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
         "student clique overlap is proven infeasible");
}

void EmptyAvailabilityMeansUnavailable() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  problem->mutable_teachers(0)->clear_available_timeslot_ids();

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
         "empty teacher availability is not interpreted as all slots");
}

void FixedTeacherAndRoomConflictsAreHard() {
  {
    auto request = ValidRequest();
    auto* problem = request.mutable_solve_request()->mutable_problem();
    problem->mutable_activities(0)->clear_allowed_start_timeslot_ids();
    problem->mutable_activities(0)->add_allowed_start_timeslot_ids(1);
    AddSecondIndependentActivity(problem, 1);
    problem->mutable_activities(1)
        ->mutable_teacher_policy()
        ->mutable_fixed_teacher()
        ->set_teacher_id(1);
    const auto response = SolveEnvelope(request).solve_response();
    Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
           "a fixed teacher cannot occupy one atom twice");
  }
  {
    auto request = ValidRequest();
    auto* problem = request.mutable_solve_request()->mutable_problem();
    problem->mutable_activities(0)->clear_allowed_start_timeslot_ids();
    problem->mutable_activities(0)->add_allowed_start_timeslot_ids(1);
    AddSecondIndependentActivity(problem, 1);
    problem->mutable_section_room_bindings(1)
        ->mutable_policy()
        ->mutable_fixed()
        ->set_room_id(1);
    const auto response = SolveEnvelope(request).solve_response();
    Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
           "a fixed room cannot occupy one atom twice");
  }
}

void CandidateResourcesCanSelectDifferentValuesAtTheSameTime() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  auto* first = problem->mutable_activities(0);
  first->clear_allowed_start_timeslot_ids();
  first->add_allowed_start_timeslot_ids(1);
  first->mutable_teacher_policy()->clear_policy();
  first->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(1);
  first->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(2);
  auto* first_binding = problem->mutable_section_room_bindings(0);
  first_binding->mutable_policy()->clear_policy();
  first_binding->mutable_policy()->mutable_section_fixed()->add_candidate_room_ids(1);
  first_binding->mutable_policy()->mutable_section_fixed()->add_candidate_room_ids(2);

  AddSecondIndependentActivity(problem, 1);
  auto* second = problem->mutable_activities(1);
  second->mutable_teacher_policy()->clear_policy();
  second->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(1);
  second->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(2);
  auto* second_binding = problem->mutable_section_room_bindings(1);
  second_binding->mutable_policy()->clear_policy();
  second_binding->mutable_policy()->mutable_section_fixed()->add_candidate_room_ids(1);
  second_binding->mutable_policy()->mutable_section_fixed()->add_candidate_room_ids(2);

  const auto response = SolveEnvelope(request).solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_OPTIMAL,
         "candidate resources permit a feasible simultaneous choice");
  if (response.assignments_size() == 2) {
    Expect(response.assignments(0).teacher_id() !=
               response.assignments(1).teacher_id(),
           "simultaneous activities choose different candidate teachers");
    Expect(response.assignments(0).room_id() != response.assignments(1).room_id(),
           "simultaneous activities choose different candidate rooms");
  }
}

void CandidateTeacherBindingIsFixedAcrossMeetings() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  auto* first = problem->mutable_activities(0);
  first->clear_allowed_start_timeslot_ids();
  first->add_allowed_start_timeslot_ids(1);
  first->mutable_teacher_policy()->clear_policy();
  first->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(1);
  first->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(2);

  AddSecondIndependentActivity(problem, 2);
  auto* second = problem->mutable_activities(1);
  second->set_teacher_binding_id(1);
  second->mutable_teacher_policy()->clear_policy();
  second->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(1);
  second->mutable_teacher_policy()->mutable_candidate_teachers()->add_teacher_ids(2);

  auto* first_teacher = problem->mutable_teachers(0);
  first_teacher->clear_available_timeslot_ids();
  first_teacher->add_available_timeslot_ids(1);
  auto* second_teacher = problem->mutable_teachers(1);
  second_teacher->clear_available_timeslot_ids();
  second_teacher->add_available_timeslot_ids(2);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_PROVEN_INFEASIBLE,
         "one offering cannot switch candidate teachers between meetings");
}

void MismatchedTeacherBindingPolicyIsInvalidInput() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  AddSecondIndependentActivity(problem, 2);
  problem->mutable_activities(1)->set_teacher_binding_id(1);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_INVALID_INPUT,
         "inconsistent teacher binding is rejected before model build");
  Expect(response.status_detail_code() ==
             "WORKER.INCONSISTENT_TEACHER_BINDING",
         "inconsistent binding has a stable problem code");
}

void CourseDistributionUsesARealLexicographicObjective() {
  auto request = ValidRequest();
  auto* problem = request.mutable_solve_request()->mutable_problem();
  AddSecondIndependentActivity(problem, 1);
  auto* second = problem->mutable_activities(1);
  second->clear_allowed_start_timeslot_ids();
  second->add_allowed_start_timeslot_ids(1);
  second->add_allowed_start_timeslot_ids(2);
  second->add_allowed_start_timeslot_ids(3);
  second->set_meeting_pattern_id(1);
  problem->mutable_meeting_patterns()->RemoveLast();
  auto* pattern = problem->mutable_meeting_patterns(0);
  pattern->add_activity_ids(2);
  pattern->add_duration_periods(1);
  pattern->set_maximum_periods_per_day(2);
  auto* conflict = problem->mutable_student_conflicts()->add_edges();
  conflict->set_left_activity_id(1);
  conflict->set_right_activity_id(2);

  auto* tier = problem->add_objective_tiers();
  tier->set_tier_id("distribution");
  tier->set_priority(1);
  auto* metric = tier->add_metrics();
  metric->set_metric_kind(
      protocol::OBJECTIVE_METRIC_KIND_COURSE_DISTRIBUTION);
  metric->set_weight_within_tier(3);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_OPTIMAL,
         "course distribution model is proven optimal");
  Expect(response.objective().tiers_size() == 1,
         "course distribution objective breakdown is returned");
  if (response.objective().tiers_size() == 1) {
    const auto& result = response.objective().tiers(0);
    Expect(result.value() == 3, "tier applies the within-tier weight");
    Expect(result.metrics_size() == 1 && result.metrics(0).value() == 1,
           "metric reports the unweighted adjacent-day penalty");
  }
  if (response.assignments_size() == 2) {
    const auto first_start = response.assignments(0).start_timeslot_id();
    const auto second_start = response.assignments(1).start_timeslot_id();
    Expect((first_start == 3) != (second_start == 3),
           "the two meetings are distributed across distinct days");
  }
}

void LaterObjectiveCannotSacrificeAnEarlierTier() {
  auto request = ValidRequest();
  auto* solve_request = request.mutable_solve_request();
  solve_request->mutable_parameters()->set_mode(protocol::SOLVE_MODE_IMPROVE);
  auto* problem = solve_request->mutable_problem();
  AddSecondIndependentActivity(problem, 1);
  auto* second = problem->mutable_activities(1);
  second->clear_allowed_start_timeslot_ids();
  second->add_allowed_start_timeslot_ids(1);
  second->add_allowed_start_timeslot_ids(2);
  second->add_allowed_start_timeslot_ids(3);
  second->set_meeting_pattern_id(1);
  problem->mutable_meeting_patterns()->RemoveLast();
  auto* pattern = problem->mutable_meeting_patterns(0);
  pattern->add_activity_ids(2);
  pattern->add_duration_periods(1);
  pattern->set_maximum_periods_per_day(2);
  auto* conflict = problem->mutable_student_conflicts()->add_edges();
  conflict->set_left_activity_id(1);
  conflict->set_right_activity_id(2);

  auto* first_incumbent = problem->add_incumbent_assignments();
  first_incumbent->set_activity_id(1);
  first_incumbent->set_start_timeslot_id(1);
  first_incumbent->set_room_id(1);
  first_incumbent->set_teacher_id(1);
  first_incumbent->set_duration_periods(1);
  auto* second_incumbent = problem->add_incumbent_assignments();
  second_incumbent->set_activity_id(2);
  second_incumbent->set_start_timeslot_id(2);
  second_incumbent->set_room_id(2);
  second_incumbent->set_teacher_id(2);
  second_incumbent->set_duration_periods(1);

  auto* distribution = problem->add_objective_tiers();
  distribution->set_tier_id("distribution");
  distribution->set_priority(1);
  auto* distribution_metric = distribution->add_metrics();
  distribution_metric->set_metric_kind(
      protocol::OBJECTIVE_METRIC_KIND_COURSE_DISTRIBUTION);
  distribution_metric->set_weight_within_tier(1);
  auto* stability = problem->add_objective_tiers();
  stability->set_tier_id("stability");
  stability->set_priority(2);
  auto* stability_metric = stability->add_metrics();
  stability_metric->set_metric_kind(
      protocol::OBJECTIVE_METRIC_KIND_LAYOUT_STABILITY);
  stability_metric->set_weight_within_tier(1);

  const auto response_envelope = SolveEnvelope(request);
  const auto& response = response_envelope.solve_response();
  Expect(response.status() == protocol::SOLVER_STATUS_OPTIMAL,
         "both objective passes are proven optimal");
  Expect(response.objective().tiers_size() == 2,
         "both lexicographic tiers are reported");
  if (response.assignments_size() == 2) {
    const auto first_start = response.assignments(0).start_timeslot_id();
    const auto second_start = response.assignments(1).start_timeslot_id();
    Expect((first_start == 3) != (second_start == 3),
           "layout stability cannot collapse the course back onto one day");
  }
}

}  // namespace

int main() {
  ValidModelProducesAnOptimalAssignment();
  ReproducibleModeReturnsTheSameCanonicalAssignment();
  StudentConflictIsHard();
  StudentConflictCliqueIsHard();
  EmptyAvailabilityMeansUnavailable();
  FixedTeacherAndRoomConflictsAreHard();
  CandidateResourcesCanSelectDifferentValuesAtTheSameTime();
  CandidateTeacherBindingIsFixedAcrossMeetings();
  MismatchedTeacherBindingPolicyIsInvalidInput();
  CourseDistributionUsesARealLexicographicObjective();
  LaterObjectiveCannotSacrificeAnEarlierTier();
  return failures == 0 ? 0 : 1;
}
