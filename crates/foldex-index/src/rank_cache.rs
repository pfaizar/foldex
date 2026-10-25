//! Exact leading results for one query over an append-only generation.
//! Full paths stay on disk. Both groups have independent row and byte caps.
use crate::{IndexRow, model::MAX_PAGE_ROWS};
use std::cmp::Ordering;

pub(crate) const KEY_LIMIT: usize = MAX_PAGE_ROWS * 4 + 1;
pub(crate) const GROUP_BYTES: usize = 1024 * 1024;

pub(crate) struct Key {
    pub id: u64,
    pub score: Option<i32>,
    pub natural: Vec<u8>,
    pub raw: Vec<u8>,
}

impl Key {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.natural.capacity() + self.raw.capacity()
    }

    pub fn compare(&self, other: &Self, descending: bool) -> Ordering {
        other.score.cmp(&self.score).then_with(|| {
            let order = self
                .natural
                .cmp(&other.natural)
                .then_with(|| self.raw.cmp(&other.raw))
                .then_with(|| self.id.cmp(&other.id));
            if descending { order.reverse() } else { order }
        })
    }

    pub fn compare_row(&self, other: &IndexRow, descending: bool) -> Ordering {
        other.fuzzy_score.cmp(&self.score).then_with(|| {
            let order = self
                .natural
                .cmp(&other.natural_key)
                .then_with(|| self.raw.cmp(&other.raw_sort))
                .then_with(|| self.id.cmp(&other.id));
            if descending { order.reverse() } else { order }
        })
    }
}

pub(crate) struct Group {
    pub prefix: u64,
    pub keys: Vec<Key>,
    /// False means keys are only the exact leading prefix of the matching group.
    pub complete: bool,
}

impl Group {
    pub fn merge(self, newer: Self, descending: bool) -> Self {
        // A byte-limited input only proves its first K positions. Never fill a
        // missing tail with worse keys from the other input.
        let limit = KEY_LIMIT
            .min(if self.complete {
                KEY_LIMIT
            } else {
                self.keys.len()
            })
            .min(if newer.complete {
                KEY_LIMIT
            } else {
                newer.keys.len()
            });
        let prefix = newer.prefix;
        let mut complete = self.complete && newer.complete;
        let mut older = self.keys.into_iter().peekable();
        let mut newer = newer.keys.into_iter().peekable();
        let mut keys = Vec::with_capacity(limit);
        let mut bytes = limit * std::mem::size_of::<Key>();
        while older.peek().is_some() || newer.peek().is_some() {
            let take_old = match (older.peek(), newer.peek()) {
                (Some(a), Some(b)) => a.compare(b, descending) != Ordering::Greater,
                (Some(_), None) => true,
                _ => false,
            };
            let key = if take_old { older.next() } else { newer.next() }.unwrap();
            let payload = key.bytes() - std::mem::size_of::<Key>();
            if keys.len() >= limit || bytes.saturating_add(payload) > GROUP_BYTES {
                complete = false;
                break;
            }
            bytes += payload;
            keys.push(key);
        }
        Self {
            prefix,
            keys,
            complete,
        }
    }
}

#[derive(Default)]
pub(crate) struct RankCache {
    identity: Option<(String, u64)>,
    pub groups: [Option<Group>; 2],
}

impl RankCache {
    pub fn bytes(&self) -> usize {
        self.groups
            .iter()
            .flatten()
            .map(|group| {
                group.keys.capacity() * std::mem::size_of::<Key>()
                    + group
                        .keys
                        .iter()
                        .map(|key| key.natural.capacity() + key.raw.capacity())
                        .sum::<usize>()
            })
            .sum()
    }

    pub fn select(&mut self, generation: &str, query: u64) {
        if !self
            .identity
            .as_ref()
            .is_some_and(|(g, q)| g == generation && *q == query)
        {
            self.groups = [None, None];
            self.identity = Some((generation.to_owned(), query));
        }
    }
}
