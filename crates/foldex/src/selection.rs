//! Selection is stored as disjoint row intervals, independent of the number of files.
#[derive(Default, Clone)]
pub struct Selection {
    ranges: Vec<std::ops::RangeInclusive<u32>>,
    count: usize,
    revision: u64,
}
impl Selection {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.count = 0;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn contains(&self, row: &u32) -> bool {
        let n = self.ranges.partition_point(|r| r.end() < row);
        self.ranges.get(n).is_some_and(|r| r.contains(row))
    }
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.ranges.iter().flat_map(Clone::clone)
    }
    /// Borrow the disjoint intervals without allocating or visiting each row.
    pub fn intervals_iter(
        &self,
    ) -> impl ExactSizeIterator<Item = (u32, u32)> + DoubleEndedIterator + '_ {
        self.ranges.iter().map(|r| (*r.start(), *r.end()))
    }
    pub fn intervals(&self) -> Vec<(u32, u32)> {
        self.intervals_iter().collect()
    }
    pub fn replace_intervals(&mut self, ranges: Vec<(u32, u32)>) {
        self.clear();
        for (first, last) in ranges {
            self.select(first, last);
        }
    }
    pub fn insert(&mut self, row: u32) {
        self.select(row, row);
    }
    /// Complement the intervals without enumerating individual selected files.
    pub fn invert(&mut self, len: usize) {
        let end = len.min(u32::MAX as usize + 1) as u64;
        let mut next = Vec::with_capacity(self.ranges.len() + 1);
        let mut at = 0_u64;
        for range in &self.ranges {
            let first = (*range.start() as u64).min(end);
            if at < first {
                next.push(at as u32..=(first - 1) as u32);
            }
            at = at.max(*range.end() as u64 + 1).min(end);
        }
        if at < end {
            next.push(at as u32..=(end - 1) as u32);
        }
        self.count = next
            .iter()
            .map(|r| *r.end() as u64 - *r.start() as u64 + 1)
            .sum::<u64>() as usize;
        self.ranges = next;
        self.revision = self.revision.wrapping_add(1);
    }
    /// Clip a result prefix without visiting its individual selected rows.
    pub fn clamp(&mut self, len: usize) {
        let end = len.min(u32::MAX as usize + 1) as u64;
        if self
            .ranges
            .last()
            .is_none_or(|range| (*range.end() as u64) < end)
        {
            return;
        }
        let mut removed = 0_u64;
        self.ranges.retain_mut(|range| {
            let first = *range.start() as u64;
            let last = *range.end() as u64;
            if first >= end {
                removed += last - first + 1;
                return false;
            }
            if last >= end {
                removed += last - end + 1;
                *range = first as u32..=(end - 1) as u32;
            }
            true
        });
        if removed > 0 {
            self.count -= removed as usize;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn select(&mut self, first: u32, last: u32) {
        let (mut first, mut last) = (first.min(last), first.max(last));
        let start = self
            .ranges
            .partition_point(|r| r.end().saturating_add(1) < first);
        let mut end = start;
        while let Some(r) = self.ranges.get(end) {
            if *r.start() > last.saturating_add(1) {
                break;
            }
            first = first.min(*r.start());
            last = last.max(*r.end());
            self.count -= (*r.end() as u64 - *r.start() as u64 + 1) as usize;
            end += 1;
        }
        self.count += (last as u64 - first as u64 + 1) as usize;
        self.ranges.splice(start..end, [first..=last]);
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn remove(&mut self, row: &u32) -> bool {
        let n = self.ranges.partition_point(|r| r.end() < row);
        let Some(r) = self.ranges.get(n).filter(|r| r.contains(row)).cloned() else {
            return false;
        };
        self.count -= 1;
        self.revision = self.revision.wrapping_add(1);
        let (a, b) = (*r.start(), *r.end());
        match (a == *row, b == *row) {
            (true, true) => {
                self.ranges.remove(n);
            }
            (true, false) => self.ranges[n] = row + 1..=b,
            (false, true) => self.ranges[n] = a..=row - 1,
            (false, false) => {
                self.ranges[n] = a..=row - 1;
                self.ranges.insert(n + 1, row + 1..=b);
            }
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_intervals_keep_huge_and_fragmented_selections_bounded() {
        let mut selection = Selection::default();
        selection.select(0, u32::MAX);
        assert_eq!(selection.intervals_iter().len(), 1);
        assert_eq!(selection.intervals_iter().next(), Some((0, u32::MAX)));
        assert!(selection.remove(&500_000));
        assert!(selection.remove(&u32::MAX));
        selection.insert(u32::MAX);
        let revision = selection.revision();
        {
            let mut intervals = selection.intervals_iter();
            assert_eq!(intervals.len(), 2);
            assert_eq!(intervals.next_back(), Some((500_001, u32::MAX)));
            assert_eq!(intervals.next(), Some((0, 499_999)));
            assert_eq!(intervals.len(), 0);
            assert!(intervals.next().is_none());
        }
        assert_eq!(selection.revision(), revision);
        selection.clamp(750_000);
        assert_eq!(
            selection.intervals_iter().collect::<Vec<_>>(),
            [(0, 499_999), (500_001, 749_999)]
        );
        selection.clear();
        assert!(selection.intervals_iter().next().is_none());
    }

    #[test]
    fn a_million_rows_use_one_interval_and_can_be_toggled() {
        let mut s = Selection::default();
        s.select(0, 999_999);
        assert_eq!(s.len(), 1_000_000);
        assert_eq!(s.ranges.len(), 1);
        assert!(s.remove(&500_000));
        assert_eq!(s.len(), 999_999);
        assert!(!s.contains(&500_000));
        s.insert(500_000);
        assert_eq!(s.ranges.len(), 1);
        assert_eq!(s.iter().take(3).collect::<Vec<_>>(), vec![0, 1, 2]);
    }
    #[test]
    fn overlapping_ranges_merge_and_boundary_removals_keep_counts() {
        let mut s = Selection::default();
        s.select(9, 4);
        s.select(0, 3);
        s.select(8, 13);
        assert_eq!(s.iter().collect::<Vec<_>>(), (0..=13).collect::<Vec<_>>());
        assert!(s.remove(&0));
        assert!(s.remove(&13));
        assert!(!s.remove(&99));
        assert_eq!(s.len(), 12);
    }
    #[test]
    fn invert_is_the_exact_complement_for_large_and_clipped_views() {
        let mut s = Selection::default();
        s.select(10, 20);
        s.select(900_000, 999_999);
        s.invert(1_000_000);
        assert_eq!(s.len(), 899_989);
        assert_eq!(s.intervals(), vec![(0, 9), (21, 899_999)]);
        s.invert(100);
        assert_eq!(s.intervals(), vec![(10, 20)]);
        s.invert(0);
        assert!(s.is_empty());
    }
    #[test]
    fn clamping_a_huge_selection_clips_intervals_and_updates_the_count() {
        let mut s = Selection::default();
        s.select(0, 999_999);
        s.select(2_000_000, 3_000_000);
        let revision = s.revision();
        s.clamp(850_000);
        assert_eq!(s.intervals(), vec![(0, 849_999)]);
        assert_eq!(s.len(), 850_000);
        assert_ne!(revision, s.revision());
        let revision = s.revision();
        s.clamp(900_000);
        assert_eq!(revision, s.revision());
        s.clamp(0);
        assert!(s.is_empty());
    }
    #[test]
    fn clamping_keeps_gaps_and_handles_the_last_representable_row() {
        let mut s = Selection::default();
        s.select(5, 9);
        s.select(20, 29);
        s.insert(u32::MAX);
        s.clamp(u32::MAX as usize + 1);
        assert_eq!(s.len(), 16);
        assert!(s.contains(&u32::MAX));
        s.clamp(25);
        assert_eq!(s.intervals(), vec![(5, 9), (20, 24)]);
        assert_eq!(s.len(), 10);
    }
}
