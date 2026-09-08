# Small Input A fixture

This fixture is a deliberately small but non-trivial, fully sectioned Chinese senior-high-school
dataset. It uses only synthetic student and staff labels and contains no personal data.

## Scale

- Grade: `G12`
- 24 students in 2 administrative classes (12 students each)
- Exactly 3 selections per student from the 6 elective subjects
- 6 pre-existing teaching sections, each with 12 students
- 16 teachers and 10 rooms across teaching and science buildings
- 9 course plans and 12 audience-specific course offerings
- 34 meeting demands / 36 scheduled periods per week across all audiences
- 3 non-conflicting fixed activities

The eight selection combinations are balanced: every elective subject has exactly 12 students.
Together the combinations cover every pair of elective subjects, so the generated student
conflict graph must prevent all six teaching sections from overlapping one another. This makes
the fixture useful for catching implementations that check administrative classes but ignore
actual section enrollment.

## Business cases represented

- Chinese, mathematics, and English are administrative-class courses using `AdminHomeRoom`.
- Each administrative class gets a separate course offering, allowing its own teacher assignment.
- Physics, chemistry, and biology require feature-compatible specialist laboratories.
- All teaching sections use `SectionFixed`: the solver selects one room from the section's
  candidates and must retain it for every meeting of that section.
- Most offerings have a fixed teacher. English for `AC02`, chemistry, and politics use candidate
  teacher sets; one selected teacher must serve all meetings of that offering.
- Teacher unavailability spans both fixed and candidate teachers without making the instance
  trivially infeasible.
- Physics and chemistry include legal two-period meetings (`2;1`) and may not cross the break.
- The fixed activities exercise administrative home rooms and a two-period laboratory lock while
  avoiding student, teacher, and room conflicts.

## Calendar

The intended calendar is Monday through Friday, eight periods per day, with a break boundary
after period 4. All unavailability and fixed-activity references fall within that calendar.

## Import contract

Each filename is the canonical `DatasetKind::as_str()` value plus `.csv`. Headers match the strict
schema in `crates/import/src/parser.rs`; semicolon-separated values are used only for list fields.
The bundle is Input A because both `teaching_sections.csv` and `section_enrollments.csv` are
present. Every selected `(student, subject)` has exactly one section enrollment.
