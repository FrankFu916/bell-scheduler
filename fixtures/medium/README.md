# Generated medium benchmark fixture

Synthetic data only: 480 students, 12 administrative classes, six elective subjects,
24 teaching sections, 81 teachers, 43 rooms and 40 weekly periods. Every student has
exactly three choices. Science and humanities tracks keep the fixture feasible while
actual section enrollments, fixed section rooms, specialist features, teacher
unavailability and fixed activities remain active. Regenerate with:

```text
cargo run -p class-schedule-fixture-generator -- fixtures/medium
```
