use super::{edit, fixture, friday, single};
use class_schedule_application::{
    ScenarioEditOperation, ScenarioTimetableExportCommand, ScenarioTimetableExportFormat,
    TimetableQuery, TimetableView, load_scenario, prepare_scenario_timetable_export,
    publish_scenario_timetable_export, query_scenario_timetable, query_scenario_timetable_entities,
};
use class_schedule_domain::Revision;
use class_schedule_persistence::SqliteStore;

#[test]
fn seven_views_and_both_exports_use_actual_edited_a_and_b_state() {
    let directory = tempfile::tempdir().unwrap();
    for unsectioned in [false, true] {
        let mut store =
            SqliteStore::open(directory.path().join(format!("view-{unsectioned}.sqlite3")))
                .unwrap();
        let receipt = fixture::saved_scenario(&mut store, unsectioned);
        let loaded = load_scenario(&store, receipt.scenario_id).unwrap();
        let activity_id = single(&loaded);
        let target = friday(&loaded, 1);
        let moved = edit(
            &mut store,
            &receipt,
            ScenarioEditOperation::Move {
                activity_id,
                start: target,
                lock_after: false,
            },
        );
        let mut found = 0;
        for view in [
            TimetableView::AdministrativeClass,
            TimetableView::TeachingSection,
            TimetableView::Teacher,
            TimetableView::Room,
            TimetableView::Student,
            TimetableView::Subject,
            TimetableView::Grade,
        ] {
            let entities = query_scenario_timetable_entities(
                &store,
                moved.scenario_id,
                Revision::from_u64(1),
                Revision::from_u64(1),
                view,
                0,
                100,
            )
            .unwrap();
            for entity in entities.entities {
                let page = query_scenario_timetable(
                    &store,
                    moved.scenario_id,
                    Revision::from_u64(1),
                    Revision::from_u64(1),
                    &TimetableQuery {
                        filter: entity.filter,
                        offset: 0,
                        limit: 100,
                    },
                )
                .unwrap();
                if let Some(row) = page.rows.iter().find(|row| row.activity_id == activity_id) {
                    found += 1;
                    assert_eq!(row.day, class_schedule_domain::Day::Friday);
                    assert_eq!(row.period_index, 1);
                    assert_eq!(
                        page.calendar
                            .iter()
                            .find(|cell| cell.timeslot_id == target)
                            .unwrap()
                            .timeslot_index,
                        row.start_timeslot_index
                    );
                }
                if view == TimetableView::Grade {
                    for format in [
                        ScenarioTimetableExportFormat::Csv,
                        ScenarioTimetableExportFormat::Xlsx,
                    ] {
                        let prepared = prepare_scenario_timetable_export(
                            &store,
                            &ScenarioTimetableExportCommand {
                                scenario_id: moved.scenario_id,
                                expected_scenario_revision: Revision::from_u64(1),
                                expected_timetable_revision: Revision::from_u64(1),
                                filter: entity.filter,
                                format,
                            },
                        )
                        .unwrap();
                        let path = directory
                            .path()
                            .join(format!("edited-{unsectioned}.{}", format.extension()));
                        let result = publish_scenario_timetable_export(&prepared, &path).unwrap();
                        assert_eq!(result.metadata.receipt, moved);
                        assert_eq!(result.metadata.meeting_count as usize, page.rows.len());
                        if format == ScenarioTimetableExportFormat::Csv {
                            check_csv(&path, activity_id);
                        } else {
                            check_xlsx(&path, activity_id);
                        }
                    }
                }
            }
        }
        assert!(found >= 6);
    }
}

fn check_csv(path: &std::path::Path, activity_id: class_schedule_domain::MeetingDemandId) {
    let bytes = std::fs::read(path).unwrap();
    let mut csv = csv::Reader::from_reader(&bytes[3..]);
    let header = csv.headers().unwrap().clone();
    let id_column = header
        .iter()
        .position(|column| column == "activity_id")
        .unwrap();
    let day_column = header.iter().position(|column| column == "day").unwrap();
    let row = csv
        .records()
        .map(Result::unwrap)
        .find(|row| row[id_column] == activity_id.to_string())
        .unwrap();
    assert_eq!(&row[day_column], "星期五");
}

fn check_xlsx(path: &std::path::Path, activity_id: class_schedule_domain::MeetingDemandId) {
    use calamine::{DataType, Reader};
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path).unwrap();
    let details = workbook.worksheet_range("课程清单").unwrap();
    let row = details
        .rows()
        .find(|row| {
            row.get(13).and_then(DataType::get_string) == Some(activity_id.to_string().as_str())
        })
        .unwrap();
    assert_eq!(row[0].get_string(), Some("星期五"));
}
