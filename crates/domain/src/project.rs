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
        let next_revision = self.revision.next()?;
        self.name = name;
        self.revision = next_revision;
        Ok(())
    }

    pub fn set_active_term(
        &mut self,
        expected_revision: Revision,
        active_term_id: Option<AcademicTermId>,
    ) -> Result<(), DomainError> {
        self.revision.ensure(expected_revision)?;
        let next_revision = self.revision.next()?;
        self.active_term_id = active_term_id;
        self.revision = next_revision;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_overflow_preserves_project_name_and_active_term() {
        let mut project =
            SchoolProject::new(SchoolProjectId::new_v4(), Name::new("Original").unwrap());
        project.revision = Revision::from_u64(u64::MAX);
        let before = project.clone();
        assert_eq!(
            project
                .rename(project.revision(), Name::new("Replacement").unwrap())
                .unwrap_err(),
            DomainError::RevisionOverflow
        );
        assert_eq!(project, before);
        assert_eq!(
            project
                .set_active_term(project.revision(), Some(AcademicTermId::new_v4()))
                .unwrap_err(),
            DomainError::RevisionOverflow
        );
        assert_eq!(project, before);
    }
}
