use crate::{
    AdministrativeClassId, BuildingId, Capacity, ExternalCode, GradeId, Name, RoomId,
    SchoolProjectId, StudentId, SubjectId, TeacherId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RoomFeature(ExternalCode);

impl RoomFeature {
    pub fn new(value: impl Into<String>) -> Result<Self, crate::DomainError> {
        ExternalCode::new(value).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Student {
    id: StudentId,
    project_id: SchoolProjectId,
    external_code: ExternalCode,
    name: Name,
    administrative_class_id: AdministrativeClassId,
}

impl Student {
    #[must_use]
    pub const fn new(
        id: StudentId,
        project_id: SchoolProjectId,
        external_code: ExternalCode,
        name: Name,
        administrative_class_id: AdministrativeClassId,
    ) -> Self {
        Self {
            id,
            project_id,
            external_code,
            name,
            administrative_class_id,
        }
    }

    #[must_use]
    pub const fn id(&self) -> StudentId {
        self.id
    }
    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }
    #[must_use]
    pub const fn external_code(&self) -> &ExternalCode {
        &self.external_code
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn administrative_class_id(&self) -> AdministrativeClassId {
        self.administrative_class_id
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Teacher {
    id: TeacherId,
    project_id: SchoolProjectId,
    external_code: ExternalCode,
    name: Name,
}

impl Teacher {
    #[must_use]
    pub const fn new(
        id: TeacherId,
        project_id: SchoolProjectId,
        external_code: ExternalCode,
        name: Name,
    ) -> Self {
        Self {
            id,
            project_id,
            external_code,
            name,
        }
    }

    #[must_use]
    pub const fn id(&self) -> TeacherId {
        self.id
    }
    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }
    #[must_use]
    pub const fn external_code(&self) -> &ExternalCode {
        &self.external_code
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Building {
    id: BuildingId,
    project_id: SchoolProjectId,
    code: ExternalCode,
    name: Name,
}

impl Building {
    #[must_use]
    pub const fn new(
        id: BuildingId,
        project_id: SchoolProjectId,
        code: ExternalCode,
        name: Name,
    ) -> Self {
        Self {
            id,
            project_id,
            code,
            name,
        }
    }

    #[must_use]
    pub const fn id(&self) -> BuildingId {
        self.id
    }
    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }
    #[must_use]
    pub const fn code(&self) -> &ExternalCode {
        &self.code
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Room {
    id: RoomId,
    building_id: BuildingId,
    code: ExternalCode,
    name: Name,
    capacity: Capacity,
    features: BTreeSet<RoomFeature>,
}

impl Room {
    #[must_use]
    pub fn new(
        id: RoomId,
        building_id: BuildingId,
        code: ExternalCode,
        name: Name,
        capacity: Capacity,
        features: impl IntoIterator<Item = RoomFeature>,
    ) -> Self {
        Self {
            id,
            building_id,
            code,
            name,
            capacity,
            features: features.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> RoomId {
        self.id
    }
    #[must_use]
    pub const fn building_id(&self) -> BuildingId {
        self.building_id
    }
    #[must_use]
    pub const fn code(&self) -> &ExternalCode {
        &self.code
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn capacity(&self) -> Capacity {
        self.capacity
    }
    #[must_use]
    pub const fn features(&self) -> &BTreeSet<RoomFeature> {
        &self.features
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdministrativeClass {
    id: AdministrativeClassId,
    grade_id: GradeId,
    name: Name,
    home_room_id: RoomId,
}

impl AdministrativeClass {
    #[must_use]
    pub const fn new(
        id: AdministrativeClassId,
        grade_id: GradeId,
        name: Name,
        home_room_id: RoomId,
    ) -> Self {
        Self {
            id,
            grade_id,
            name,
            home_room_id,
        }
    }

    #[must_use]
    pub const fn id(&self) -> AdministrativeClassId {
        self.id
    }
    #[must_use]
    pub const fn grade_id(&self) -> GradeId {
        self.grade_id
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
    #[must_use]
    pub const fn home_room_id(&self) -> RoomId {
        self.home_room_id
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Subject {
    id: SubjectId,
    project_id: SchoolProjectId,
    code: ExternalCode,
    name: Name,
}

impl Subject {
    #[must_use]
    pub const fn new(
        id: SubjectId,
        project_id: SchoolProjectId,
        code: ExternalCode,
        name: Name,
    ) -> Self {
        Self {
            id,
            project_id,
            code,
            name,
        }
    }

    #[must_use]
    pub const fn id(&self) -> SubjectId {
        self.id
    }
    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }
    #[must_use]
    pub const fn code(&self) -> &ExternalCode {
        &self.code
    }
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
}

/// A student's selected subject, represented as data rather than subject-specific fields.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct StudentSubjectChoice {
    student_id: StudentId,
    subject_id: SubjectId,
}

impl StudentSubjectChoice {
    #[must_use]
    pub const fn new(student_id: StudentId, subject_id: SubjectId) -> Self {
        Self {
            student_id,
            subject_id,
        }
    }

    #[must_use]
    pub const fn student_id(self) -> StudentId {
        self.student_id
    }
    #[must_use]
    pub const fn subject_id(self) -> SubjectId {
        self.subject_id
    }
}
