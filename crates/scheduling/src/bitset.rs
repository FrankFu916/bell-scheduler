use serde::{Deserialize, Serialize};

use crate::SchedulingError;

/// A deterministic compact set used for student audiences and slot availability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DenseBitSet {
    len: usize,
    words: Vec<u64>,
}

impl DenseBitSet {
    pub fn empty(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }

    pub fn full(len: usize) -> Self {
        let mut set = Self {
            len,
            words: vec![u64::MAX; len.div_ceil(64)],
        };
        set.clear_unused_bits();
        set
    }

    /// Builds a set and rejects any index outside `len`.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulingError::BitIndexOutOfRange`] for an invalid index.
    pub fn from_indices(
        len: usize,
        indices: impl IntoIterator<Item = usize>,
    ) -> Result<Self, SchedulingError> {
        let mut set = Self::empty(len);
        for index in indices {
            set.insert(index)?;
        }
        Ok(set)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// Inserts an index and reports whether it was newly present.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulingError::BitIndexOutOfRange`] when `index >= self.len()`.
    pub fn insert(&mut self, index: usize) -> Result<bool, SchedulingError> {
        if index >= self.len {
            return Err(SchedulingError::BitIndexOutOfRange {
                index,
                len: self.len,
            });
        }
        let word = index / 64;
        let mask = 1_u64 << (index % 64);
        let existed = self.words[word] & mask != 0;
        self.words[word] |= mask;
        Ok(!existed)
    }

    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        index < self.len && self.words[index / 64] & (1_u64 << (index % 64)) != 0
    }

    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.len == other.len
            && self
                .words
                .iter()
                .zip(&other.words)
                .any(|(left, right)| left & right != 0)
    }

    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.words
            .iter()
            .enumerate()
            .flat_map(move |(word_index, word)| {
                let mut remaining = *word;
                std::iter::from_fn(move || {
                    if remaining == 0 {
                        return None;
                    }
                    let bit = remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    let index = word_index * 64 + bit;
                    (index < self.len).then_some(index)
                })
            })
    }

    fn clear_unused_bits(&mut self) {
        let used = self.len % 64;
        if used != 0
            && let Some(last) = self.words.last_mut()
        {
            *last &= (1_u64 << used) - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_boundaries_and_intersection() {
        let left = DenseBitSet::from_indices(130, [0, 63, 64, 129]).unwrap();
        let right = DenseBitSet::from_indices(130, [1, 64]).unwrap();
        assert_eq!(left.indices().collect::<Vec<_>>(), vec![0, 63, 64, 129]);
        assert!(left.intersects(&right));
        assert_eq!(left.count(), 4);
    }

    #[test]
    fn full_masks_unused_tail_bits() {
        let set = DenseBitSet::full(65);
        assert_eq!(set.count(), 65);
        assert!(!set.contains(65));
    }
}
