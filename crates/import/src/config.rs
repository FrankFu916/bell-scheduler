use crate::DatasetKind;
use std::collections::BTreeMap;

/// Maps a source CSV header to a canonical schema field. Headers not mentioned here retain their
/// original name and are still subject to strict schema checking.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ColumnMapping {
    renames: BTreeMap<String, String>,
}

impl ColumnMapping {
    #[must_use]
    pub fn new(renames: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Self {
        Self {
            renames: renames
                .into_iter()
                .map(|(source, target)| (source.into(), target.into()))
                .collect(),
        }
    }

    pub(crate) fn resolve<'a>(&'a self, source: &'a str) -> &'a str {
        self.renames.get(source).map_or(source, String::as_str)
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.renames
            .iter()
            .map(|(source, target)| (source.as_str(), target.as_str()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportConfig {
    exact_subject_choices: Option<usize>,
    require_core_datasets: bool,
    mappings: BTreeMap<DatasetKind, ColumnMapping>,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            exact_subject_choices: Some(3),
            require_core_datasets: true,
            mappings: BTreeMap::new(),
        }
    }
}

impl ImportConfig {
    #[must_use]
    pub fn with_exact_subject_choices(mut self, count: Option<usize>) -> Self {
        self.exact_subject_choices = count;
        self
    }

    #[must_use]
    pub fn with_required_core_datasets(mut self, required: bool) -> Self {
        self.require_core_datasets = required;
        self
    }

    #[must_use]
    pub fn with_mapping(mut self, kind: DatasetKind, mapping: ColumnMapping) -> Self {
        self.mappings.insert(kind, mapping);
        self
    }

    #[must_use]
    pub const fn exact_subject_choices(&self) -> Option<usize> {
        self.exact_subject_choices
    }

    pub(crate) const fn require_core_datasets(&self) -> bool {
        self.require_core_datasets
    }

    pub(crate) fn mapping(&self, kind: DatasetKind) -> Option<&ColumnMapping> {
        self.mappings.get(&kind)
    }
}
