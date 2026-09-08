use crate::{AcademicTermId, DomainError, GradeId, Name, Revision, SchoolProjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SchoolProject {
    id: SchoolProjectId,
    name: Name,
    active_term_id: Option<AcademicTermId>,
    revision: Revision,
}

impl SchoolProject {
    #[must_use]
    pub fn new(id: SchoolProjectId, name: Name) -> Self {
        Self {
            id,
            name,
            active_term_id: None,
            revision: Revision::INITIAL,
        }
    }

    #[must_use]
    pub const fn id(&self) -> SchoolProjectId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &Name {
        &self.name
    }

    #[must_use]
    pub const fn active_term_id(&self) -> Option<AcademicTermId> {
        self.active_term_id
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    pub fn rename(&mut self, expected_revision: Revision, name: Name) -> Result<(), DomainError> {
        self.revision.ensure(expected_revision)?;
        self.name = name;
        self.revision = self.revision.next()?;
        Ok(())
    }

    pub fn set_active_term(
        &mut self,
        expected_revision: Revision,
        active_term_id: Option<AcademicTermId>,
    ) -> Result<(), DomainError> {
        self.revision.ensure(expected_revision)?;
        self.active_term_id = active_term_id;
        self.revision = self.revision.next()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Grade {
    id: GradeId,
    project_id: SchoolProjectId,
    name: Name,
}

impl Grade {
    #[must_use]
    pub const fn new(id: GradeId, project_id: SchoolProjectId, name: Name) -> Self {
        Self {
            id,
            project_id,
            name,
        }
    }

    #[must_use]
    pub const fn id(&self) -> GradeId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> SchoolProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
}
