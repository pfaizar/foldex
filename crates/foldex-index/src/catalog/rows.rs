//! Bounded decoded rows for live refreshes. Stable IDs identify immutable
//! generation records, so unchanged leading results need no repeated file opens.
use super::{Catalog, Hit};
use crate::{IndexRow, model::*};
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};
pub(super) const BUDGET: usize = 1024 * 1024;
const ROWS: usize = 1025;
#[derive(Clone, Copy, Default)]
pub(super) struct Timing {
    pub decode: Duration,
    pub elapsed: Duration,
    pub hits: usize,
    pub misses: usize,
}
#[derive(Default)]
pub(super) struct Rows {
    items: HashMap<u64, (u64, IndexRow)>,
    payload: usize,
    age: u64,
    pub timing: Timing,
    pub ordered_baseline: bool,
}
impl Rows {
    pub fn bytes(&self) -> usize {
        // Row.bytes includes occupied row structs; account for vacant buckets,
        // IDs, ages, and hash table control bytes as well.
        self.payload + self.items.capacity() * (std::mem::size_of::<(u64, (u64, IndexRow))>() + 1)
            - self.items.len() * std::mem::size_of::<IndexRow>()
    }
    fn retain(&mut self, row: &IndexRow) {
        self.age = self.age.wrapping_add(1);
        if let Some((age, _)) = self.items.get_mut(&row.id) {
            *age = self.age;
            return;
        }
        if row.bytes() > BUDGET / 2 {
            return;
        }
        let cached = row.clone();
        self.payload += cached.bytes();
        self.items.insert(row.id, (self.age, cached));
        while self.bytes() > BUDGET || self.items.len() > ROWS {
            let Some(id) = self
                .items
                .iter()
                .min_by_key(|(_, (age, _))| *age)
                .map(|(id, _)| *id)
            else {
                break;
            };
            let (_, row) = self.items.remove(&id).unwrap();
            self.payload -= row.bytes();
        }
    }
    pub fn page(
        &mut self,
        data: &Catalog,
        hits: &[Hit],
        scope: &Path,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRow>> {
        let started = Instant::now();
        self.timing = Timing::default();
        let hits = &hits[..hits.len().min(MAX_PAGE_ROWS)];
        let mut output: Vec<Option<IndexRow>> = (0..hits.len()).map(|_| None).collect();
        let mut slots = Vec::new();
        for (index, hit) in hits.iter().enumerate() {
            cancel.check()?;
            if let Some((age, row)) = self.items.get_mut(&hit.id) {
                self.age = self.age.wrapping_add(1);
                *age = self.age;
                let mut row = row.clone();
                // Rank belongs to the query; the immutable metadata cache does not.
                row.fuzzy_score = hit.score;
                output[index] = Some(row);
            } else {
                slots.push(index);
            }
        }
        self.timing.misses = slots.len();
        self.timing.hits = hits.len() - slots.len();
        let decode_started = Instant::now();
        let decoded = data.rows_at(
            hits,
            &slots,
            scope,
            MAX_PAGE_BYTES,
            cancel,
            self.ordered_baseline,
        )?;
        self.timing.decode = decode_started.elapsed();
        for (row, slot) in decoded.into_iter().zip(slots) {
            output[slot] = Some(row);
        }
        let mut page = Vec::with_capacity(hits.len());
        let mut bytes = 0usize;
        for row in output {
            let Some(row) = row else {
                break;
            };
            if !page.is_empty() && bytes.saturating_add(row.bytes()) > MAX_PAGE_BYTES {
                break;
            }
            bytes += row.bytes();
            self.retain(&row);
            page.push(row);
        }
        cancel.check()?;
        self.timing.elapsed = started.elapsed();
        Ok(page)
    }
}
