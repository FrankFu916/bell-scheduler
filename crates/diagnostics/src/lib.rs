#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Read-only, localizable diagnostics assembled from independent Rust checks and solver signals.
//!
//! This crate cannot mutate constraints. In particular, CP-SAT sufficient assumptions are useful
//! evidence but are never described as a minimum conflict set, and every relaxation suggestion
//! requires an explicit application command outside this crate.

use std::collections::{BTreeMap, BTreeSet};

use class_schedule_validation::ValidationReport;
use serde::{Deserialize, Serialize};
use solver_contract::{DiagnosticSignal, EntityKind, SolveResponse, SolverStatus};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOutcome {
    StaticContradiction,
    Optimal,
    Feasible,
    ProvenInfeasible,
    Timeout,
    Unknown,
    Cancelled,
    InvalidInput,
    InvalidModel,
    InternalError,
    MissingSolverResponse,
    UnrecognizedSolverStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    IndependentStaticCheck,
    SufficientAssumptionsNotMinimal,
    SolverModelBuild,
    IndependentOutputValidation,
    UnrecognizedSolverSignal,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RelatedEntity {
    pub kind: String,
    pub compact_id: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DiagnosticItem {
    pub problem_code: String,
    pub group_id: Option<String>,
    pub evidence: EvidenceKind,
    pub activity_indices: Vec<u32>,
    pub related_entities: Vec<RelatedEntity>,
    pub parameters: BTreeMap<String, String>,
    /// Always false for sufficient assumptions; CP-SAT does not promise a minimum core here.
    pub is_minimum_conflict_set: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RelaxationSuggestion {
    pub suggestion_code: String,
    pub message_key: String,
    pub related_problem_codes: Vec<String>,
    pub requires_explicit_user_action: bool,
    pub applies_automatically: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticsReport {
    pub outcome: DiagnosticOutcome,
    pub status_detail_code: Option<String>,
    pub items: Vec<DiagnosticItem>,
    pub suggestions: Vec<RelaxationSuggestion>,
}

/// Aggregates diagnostic evidence without changing or weakening any constraint.
#[must_use]
pub fn aggregate(
    static_report: Option<&ValidationReport>,
    solver_response: Option<&SolveResponse>,
) -> DiagnosticsReport {
    let static_has_problems = static_report.is_some_and(|report| !report.is_valid());
    let mut items = BTreeSet::new();
    if let Some(report) = static_report {
        for problem in &report.hard_problems {
            items.insert(DiagnosticItem {
                problem_code: problem.code.as_str().to_owned(),
                group_id: None,
                evidence: EvidenceKind::IndependentStaticCheck,
                activity_indices: problem
                    .activities
                    .iter()
                    .map(|activity| activity.0)
                    .collect(),
                related_entities: problem
                    .entity_indices
                    .iter()
                    .map(|(kind, compact_id)| RelatedEntity {
                        kind: kind.clone(),
                        compact_id: *compact_id,
                    })
                    .collect(),
                parameters: problem.parameters.clone(),
                is_minimum_conflict_set: false,
            });
        }
    }
    if let Some(response) = solver_response {
        for group in &response.diagnostic_groups {
            let evidence = match DiagnosticSignal::try_from(group.signal).ok() {
                Some(DiagnosticSignal::StaticPrecheck) => EvidenceKind::IndependentStaticCheck,
                Some(DiagnosticSignal::SufficientAssumptions) => {
                    EvidenceKind::SufficientAssumptionsNotMinimal
                }
                Some(DiagnosticSignal::ModelBuild) => EvidenceKind::SolverModelBuild,
                Some(DiagnosticSignal::OutputValidation) => {
                    EvidenceKind::IndependentOutputValidation
                }
                Some(DiagnosticSignal::Unspecified) | None => {
                    EvidenceKind::UnrecognizedSolverSignal
                }
            };
            let mut related_entities = group
                .related_entities
                .iter()
                .map(|entity| RelatedEntity {
                    kind: entity_kind_name(entity.entity_kind),
                    compact_id: entity.compact_id,
                })
                .collect::<Vec<_>>();
            related_entities.sort();
            items.insert(DiagnosticItem {
                problem_code: group.problem_code.clone(),
                group_id: Some(group.group_id.clone()),
                evidence,
                activity_indices: Vec::new(),
                related_entities,
                parameters: group.parameters.clone().into_iter().collect(),
                is_minimum_conflict_set: false,
            });
        }
    }
    let outcome = if static_has_problems {
        DiagnosticOutcome::StaticContradiction
    } else {
        solver_response.map_or(DiagnosticOutcome::MissingSolverResponse, solver_outcome)
    };
    let status_detail_code = solver_response.map(|response| response.status_detail_code.clone());
    let items = items.into_iter().collect::<Vec<_>>();
    let suggestions = read_only_suggestions(&items);
    DiagnosticsReport {
        outcome,
        status_detail_code,
        items,
        suggestions,
    }
}

fn entity_kind_name(value: i32) -> String {
    match EntityKind::try_from(value).ok() {
        Some(EntityKind::Activity) => "activity".to_owned(),
        Some(EntityKind::Student) => "student".to_owned(),
        Some(EntityKind::Teacher) => "teacher".to_owned(),
        Some(EntityKind::Room) => "room".to_owned(),
        Some(EntityKind::Section) => "section".to_owned(),
        Some(EntityKind::AdministrativeClass) => "administrative_class".to_owned(),
        Some(EntityKind::Subject) => "subject".to_owned(),
        Some(EntityKind::Timeslot) => "timeslot".to_owned(),
        Some(EntityKind::Unspecified) | None => format!("unrecognized:{value}"),
    }
}

fn solver_outcome(response: &SolveResponse) -> DiagnosticOutcome {
    match SolverStatus::try_from(response.status).ok() {
        Some(SolverStatus::Optimal) => DiagnosticOutcome::Optimal,
        Some(SolverStatus::Feasible) => DiagnosticOutcome::Feasible,
        Some(SolverStatus::ProvenInfeasible) => DiagnosticOutcome::ProvenInfeasible,
        Some(SolverStatus::Timeout) => DiagnosticOutcome::Timeout,
        Some(SolverStatus::Unknown) => DiagnosticOutcome::Unknown,
        Some(SolverStatus::Cancelled) => DiagnosticOutcome::Cancelled,
        Some(SolverStatus::InvalidInput) => DiagnosticOutcome::InvalidInput,
        Some(SolverStatus::InvalidModel) => DiagnosticOutcome::InvalidModel,
        Some(SolverStatus::InternalError) => DiagnosticOutcome::InternalError,
        Some(SolverStatus::Unspecified) | None => DiagnosticOutcome::UnrecognizedSolverStatus,
    }
}

fn read_only_suggestions(items: &[DiagnosticItem]) -> Vec<RelaxationSuggestion> {
    let mut codes = items
        .iter()
        .map(|item| item.problem_code.as_str())
        .collect::<BTreeSet<_>>();
    let mut suggestions = Vec::new();
    if codes.remove("HARD_LOCKED_ASSIGNMENT") {
        suggestions.push(RelaxationSuggestion {
            suggestion_code: "DIAGNOSTIC_REVIEW_LOCK".to_owned(),
            message_key: "diagnostics.suggestion.review_lock".to_owned(),
            related_problem_codes: vec!["HARD_LOCKED_ASSIGNMENT".to_owned()],
            requires_explicit_user_action: true,
            applies_automatically: false,
        });
    }
    suggestions
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use class_schedule_scheduling::ActivityIndex;
    use class_schedule_validation::{HardProblem, HardProblemCode};
    use solver_contract::{DiagnosticGroup, SolveResponse};

    use super::*;

    fn response(status: SolverStatus) -> SolveResponse {
        SolveResponse {
            status: status as i32,
            status_detail_code: format!("STATUS.{status:?}"),
            ..SolveResponse::default()
        }
    }

    #[test]
    fn static_problems_are_deduplicated_and_take_precedence() {
        let problem = HardProblem {
            code: HardProblemCode::MeetingDemandNoLegalStart,
            activities: vec![ActivityIndex(3)],
            entity_indices: BTreeMap::new(),
            parameters: BTreeMap::from([("duration".to_owned(), "2".to_owned())]),
        };
        let report = ValidationReport {
            hard_problems: vec![problem.clone(), problem],
        };

        let diagnostics = aggregate(Some(&report), Some(&response(SolverStatus::Unknown)));

        assert_eq!(diagnostics.outcome, DiagnosticOutcome::StaticContradiction);
        assert_eq!(diagnostics.items.len(), 1);
        assert_eq!(diagnostics.items[0].activity_indices, vec![3]);
    }

    #[test]
    fn sufficient_assumptions_are_never_claimed_to_be_minimal_or_auto_applied() {
        let mut solver = response(SolverStatus::ProvenInfeasible);
        solver.diagnostic_groups.push(DiagnosticGroup {
            group_id: "lock:7".to_owned(),
            problem_code: "HARD_LOCKED_ASSIGNMENT".to_owned(),
            signal: DiagnosticSignal::SufficientAssumptions as i32,
            related_entities: Vec::new(),
            parameters: HashMap::new(),
        });

        let diagnostics = aggregate(None, Some(&solver));

        assert_eq!(diagnostics.outcome, DiagnosticOutcome::ProvenInfeasible);
        assert_eq!(
            diagnostics.items[0].evidence,
            EvidenceKind::SufficientAssumptionsNotMinimal
        );
        assert!(!diagnostics.items[0].is_minimum_conflict_set);
        assert_eq!(diagnostics.suggestions.len(), 1);
        assert!(diagnostics.suggestions[0].requires_explicit_user_action);
        assert!(!diagnostics.suggestions[0].applies_automatically);
    }

    #[test]
    fn timeout_unknown_and_proven_infeasible_remain_distinct() {
        let timeout = aggregate(None, Some(&response(SolverStatus::Timeout)));
        let unknown = aggregate(None, Some(&response(SolverStatus::Unknown)));
        let infeasible = aggregate(None, Some(&response(SolverStatus::ProvenInfeasible)));

        assert_eq!(timeout.outcome, DiagnosticOutcome::Timeout);
        assert_eq!(unknown.outcome, DiagnosticOutcome::Unknown);
        assert_eq!(infeasible.outcome, DiagnosticOutcome::ProvenInfeasible);
    }

    #[test]
    fn a_feasible_response_is_not_relabelled_as_infeasible() {
        let diagnostics = aggregate(None, Some(&response(SolverStatus::Feasible)));

        assert_eq!(diagnostics.outcome, DiagnosticOutcome::Feasible);
        assert!(diagnostics.items.is_empty());
        assert!(diagnostics.suggestions.is_empty());
    }
}
