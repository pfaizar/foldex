//! Optional transposed signature columns inside an immutable catalog shard.
//!
//! Each column chooses zero, full, sorted u16 positions, or plain u64 words.
//! The 4,096-row bound keeps query scratch at 512 bytes and needs no dependency
//! or persistent heap allocation per shard. These are necessary conditions;
//! the original matcher still decides every admitted record.
use super::format::Signature;
use crate::{Error, KindFilter, Result};

pub(super) const MARKER: u32 = u32::from_le_bytes(*b"FBM1");
const ROWS: usize = 4096;
const WORDS: usize = ROWS / 64;
const DIRECTORY: usize = 192;
const HIDDEN: usize = 193;
const SUBSTRING_UNSAFE: usize = 194;
const COLUMNS: usize = 195;
const TABLE_BYTES: usize = COLUMNS * 4;

#[derive(Default)]
pub(super) struct Builder {
    words: Vec<u64>,
    encoded: Vec<u8>,
}
impl Builder {
    pub fn retained_bytes(&self) -> usize {
        self.words.capacity() * std::mem::size_of::<u64>() + self.encoded.capacity()
    }
    pub fn encode(&mut self, records: &[u8], count: usize) -> &[u8] {
        self.encoded.clear();
        if !(128..=ROWS).contains(&count) {
            return &self.encoded;
        }
        let width = count.div_ceil(64);
        self.words.resize(COLUMNS * width, 0);
        self.words.fill(0);
        for (row, record) in records
            .chunks_exact(super::format::RECORD_BYTES)
            .enumerate()
        {
            let word = row / 64;
            let bit = 1u64 << (row % 64);
            for (base, offset) in [(0, 56), (64, 64), (128, 72)] {
                let mut present = u64_at(record, offset);
                while present != 0 {
                    let column = base + present.trailing_zeros() as usize;
                    self.words[column * width + word] |= bit;
                    present &= present - 1;
                }
            }
            let flags = u32_at(record, 48);
            for (column, present) in [
                (DIRECTORY, flags & 1 != 0),
                (HIDDEN, flags & 2 != 0),
                (SUBSTRING_UNSAFE, flags & 8 == 0),
            ] {
                if present {
                    self.words[column * width + word] |= bit;
                }
            }
        }
        self.encoded.resize(TABLE_BYTES, 0);
        for column in 0..COLUMNS {
            let words = &self.words[column * width..(column + 1) * width];
            let population = words
                .iter()
                .map(|word| word.count_ones() as usize)
                .sum::<usize>();
            let tag = if population == 0 {
                0
            } else if population == count {
                1
            } else {
                while self.encoded.len() % 4 != 0 {
                    self.encoded.push(0);
                }
                let offset = self.encoded.len() as u32;
                if 2 + population * 2 < width * 8 {
                    self.encoded
                        .extend_from_slice(&(population as u16).to_le_bytes());
                    for (word_index, &word) in words.iter().enumerate() {
                        let mut remaining = word;
                        while remaining != 0 {
                            let row = word_index * 64 + remaining.trailing_zeros() as usize;
                            self.encoded.extend_from_slice(&(row as u16).to_le_bytes());
                            remaining &= remaining - 1;
                        }
                    }
                    offset | 2
                } else {
                    for word in words {
                        self.encoded.extend_from_slice(&word.to_le_bytes());
                    }
                    offset
                }
            };
            self.encoded[column * 4..column * 4 + 4].copy_from_slice(&tag.to_le_bytes());
        }
        &self.encoded
    }
}

pub(super) struct Selection {
    words: [u64; WORDS],
    word: usize,
    pub eligible: u64,
    pub selected: u64,
}
impl Iterator for Selection {
    type Item = usize;
    fn next(&mut self) -> Option<usize> {
        while self.word < WORDS {
            let bits = &mut self.words[self.word];
            if *bits != 0 {
                let row = self.word * 64 + bits.trailing_zeros() as usize;
                *bits &= *bits - 1;
                return Some(row);
            }
            self.word += 1;
        }
        None
    }
}

pub(super) enum Rows {
    Range(std::ops::Range<usize>),
    Selected(Selection),
}
impl Iterator for Rows {
    type Item = usize;
    fn next(&mut self) -> Option<usize> {
        match self {
            Self::Range(range) => range.next(),
            Self::Selected(selection) => selection.next(),
        }
    }
}

pub(super) struct View<'a> {
    bytes: &'a [u8],
    count: usize,
    width: usize,
}
impl<'a> View<'a> {
    pub fn new(bytes: &'a [u8], count: usize) -> Result<Self> {
        if bytes.len() < TABLE_BYTES || count > ROWS {
            return Err(invalid());
        }
        Ok(Self {
            bytes,
            count,
            width: count.div_ceil(64),
        })
    }
    fn column(&self, column: usize, into: &mut [u64; WORDS]) -> Result<()> {
        let tag = u32_at(self.bytes, column * 4);
        into.fill(0);
        if tag <= 1 {
            if tag == 1 {
                into[..self.width].fill(u64::MAX);
                if self.count % 64 != 0 {
                    into[self.width - 1] = (1u64 << (self.count % 64)) - 1;
                }
            }
            return Ok(());
        }
        let offset = (tag & !3) as usize;
        if offset < TABLE_BYTES || tag & 1 != 0 {
            return Err(invalid());
        }
        if tag & 2 != 0 {
            let header = self.bytes.get(offset..offset + 2).ok_or_else(invalid)?;
            let count = u16::from_le_bytes(header.try_into().unwrap()) as usize;
            let positions = self
                .bytes
                .get(offset + 2..offset + 2 + count * 2)
                .ok_or_else(invalid)?;
            let mut previous = None;
            for position in positions.chunks_exact(2) {
                let row = u16::from_le_bytes(position.try_into().unwrap()) as usize;
                if row >= self.count || previous.is_some_and(|previous| row <= previous) {
                    return Err(invalid());
                }
                into[row / 64] |= 1u64 << (row % 64);
                previous = Some(row);
            }
        } else {
            let words = self
                .bytes
                .get(offset..offset + self.width * 8)
                .ok_or_else(invalid)?;
            for (into, bytes) in into.iter_mut().zip(words.chunks_exact(8)) {
                *into = u64::from_le_bytes(bytes.try_into().unwrap());
            }
            if self.count % 64 != 0 && into[self.width - 1] >> (self.count % 64) != 0 {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn select(
        &self,
        required: Option<&Signature>,
        literal_ascii: bool,
        show_hidden: bool,
        kind: KindFilter,
        range: std::ops::Range<usize>,
    ) -> Result<Selection> {
        let mut selection = Selection {
            words: [0; WORDS],
            word: 0,
            eligible: 0,
            selected: 0,
        };
        if kind == KindFilter::None {
            return Ok(selection);
        }
        for word in range.start / 64..range.end.div_ceil(64) {
            let first = range.start.saturating_sub(word * 64).min(64);
            let end = range.end.saturating_sub(word * 64).min(64);
            selection.words[word] = (u64::MAX << first) & (u64::MAX >> (64 - end));
        }
        let mut scratch = [0u64; WORDS];
        if !show_hidden {
            self.column(HIDDEN, &mut scratch)?;
            for (word, mask) in selection.words.iter_mut().zip(scratch) {
                *word &= !mask;
            }
        }
        if kind != KindFilter::All {
            self.column(DIRECTORY, &mut scratch)?;
            for (word, mask) in selection.words.iter_mut().zip(scratch) {
                *word &= if kind == KindFilter::Folders {
                    mask
                } else {
                    !mask
                };
            }
        }
        selection.eligible = selection
            .words
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum();
        if let Some(required) = required {
            let eligible = selection.words;
            for (base, mut present) in [
                (0, required.ascii[0]),
                (64, required.ascii[1]),
                (128, required.unicode),
            ] {
                while present != 0 {
                    self.column(base + present.trailing_zeros() as usize, &mut scratch)?;
                    for (word, mask) in selection.words.iter_mut().zip(scratch) {
                        *word &= mask;
                    }
                    present &= present - 1;
                }
            }
            if literal_ascii {
                self.column(SUBSTRING_UNSAFE, &mut scratch)?;
                for ((word, mask), eligible) in
                    selection.words.iter_mut().zip(scratch).zip(eligible)
                {
                    *word |= mask & eligible;
                }
            }
        }
        selection.selected = selection
            .words
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum();
        Ok(selection)
    }
}

fn invalid() -> Error {
    Error::InvalidIndex("invalid catalog bitmap column".into())
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
