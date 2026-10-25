use crate::{Error, IndexRecord, Result};
use std::{fs, path::PathBuf};

pub(super) const RECORD_BYTES: usize = 96;
const HEADER_BYTES: usize = 64;
const MAGIC: &[u8; 8] = b"FLDCAT05";

/// Conservative fuzzy prefilter. ASCII membership is exact; non-ASCII hash
/// collisions admit extra candidates. Digit counts saturate at seven.
#[derive(Clone, Copy, Default)]
pub(super) struct Signature {
    pub(super) ascii: [u64; 2],
    pub(super) unicode: u64,
    digits: u32,
}
impl Signature {
    pub fn supports_ascii_substring(&self, original: &str, lower: &str) -> bool {
        if original.len() > foldex_match::MAX_TEXT_BYTES
            || lower.len() > foldex_match::MAX_TEXT_BYTES
        {
            return false;
        }
        if original.is_ascii() {
            return true;
        }
        let mut required = Self::default();
        for byte in lower.bytes().filter(|byte| *byte < 128) {
            required.ascii[byte as usize / 64] |= 1u64 << (byte % 64);
            if byte.is_ascii_digit() {
                let shift = u32::from(byte - b'0') * 3;
                if (required.digits >> shift) & 7 != 7 {
                    required.digits += 1 << shift;
                }
            }
        }
        // Unicode lowercase can introduce ASCII (for example dotted I and
        // Kelvin sign). Use the source-fold filter only when it covers every
        // literal ASCII requirement of the complete stored lowercase name.
        self.admits(&required)
    }
    pub fn of(text: &str) -> Self {
        let mut signature = Self::default();
        for letter in foldex_match::bounded_prefix(text).chars() {
            let point = foldex_match::fold_codepoint(letter as u32);
            if point < 128 {
                signature.ascii[point as usize / 64] |= 1u64 << (point % 64);
            } else {
                signature.unicode |=
                    1u64 << ((u64::from(point).wrapping_mul(0x9e3779b97f4a7c15)) >> 58);
            }
            if (u32::from(b'0')..=u32::from(b'9')).contains(&point) {
                let shift = (point - u32::from(b'0')) * 3;
                if (signature.digits >> shift) & 7 != 7 {
                    signature.digits += 1 << shift;
                }
            }
        }
        signature
    }
    fn admits(&self, required: &Self) -> bool {
        if self.ascii[0] & required.ascii[0] != required.ascii[0]
            || self.ascii[1] & required.ascii[1] != required.ascii[1]
            || self.unicode & required.unicode != required.unicode
        {
            return false;
        }
        let (mut actual, mut required) = (self.digits, required.digits);
        while required != 0 {
            if (actual & 7) < (required & 7) {
                return false;
            }
            actual >>= 3;
            required >>= 3;
        }
        true
    }
}

pub(super) struct Metadata {
    pub size: u64,
    pub modified_secs: i64,
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
    pub is_dir: bool,
    pub hidden: bool,
    pub symlink: bool,
}
impl From<&IndexRecord> for Metadata {
    fn from(record: &IndexRecord) -> Self {
        Self {
            size: record.size,
            modified_secs: record.modified_secs,
            created_secs: record.created_secs,
            accessed_secs: record.accessed_secs,
            is_dir: record.is_dir,
            hidden: record.hidden,
            symlink: record.symlink,
        }
    }
}

pub(super) struct Buffer {
    records: Vec<u8>,
    names: Vec<u8>,
    lower: Vec<u8>,
    natural: Vec<u8>,
    paths: Vec<u8>,
    pub count: usize,
    last_parent: Vec<u8>,
    last_parent_range: (u32, u32),
    filter: super::bitmap::Builder,
    filter_start: Option<usize>,
}
impl Buffer {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            names: Vec::new(),
            lower: Vec::new(),
            natural: Vec::new(),
            paths: Vec::new(),
            count: 0,
            last_parent: Vec::new(),
            last_parent_range: (0, 0),
            filter: super::bitmap::Builder::default(),
            filter_start: None,
        }
    }
    pub fn bytes(&self) -> usize {
        self.parts().iter().map(|part| part.len()).sum::<usize>() + HEADER_BYTES
    }
    pub fn retained_bytes(&self) -> usize {
        self.records.capacity()
            + self.names.capacity()
            + self.lower.capacity()
            + self.natural.capacity()
            + self.paths.capacity()
            + self.last_parent.capacity()
            + self.filter.retained_bytes()
    }
    pub fn parts(&self) -> [&[u8]; 5] {
        [
            &self.records,
            &self.names,
            &self.lower,
            &self.natural,
            &self.paths,
        ]
    }
    pub fn clear(&mut self) {
        self.records.clear();
        self.names.clear();
        self.lower.clear();
        self.natural.clear();
        self.paths.clear();
        self.count = 0;
        self.last_parent.clear();
        self.last_parent_range = (0, 0);
        self.filter_start = None;
    }
    pub fn prepare_filter(&mut self) {
        if self.filter_start.is_some() {
            return;
        }
        let encoded = self.filter.encode(&self.records, self.count);
        if !encoded.is_empty() {
            self.filter_start = Some(self.paths.len());
            self.paths.extend_from_slice(encoded);
        }
    }
    pub fn push(
        &mut self,
        record: Metadata,
        name: &[u8],
        lower: &[u8],
        natural: &[u8],
        parent: &[u8],
        signature: Signature,
        substring_safe: bool,
    ) -> Result<()> {
        for (blob, value) in [
            (&mut self.names, name),
            (&mut self.lower, lower),
            (&mut self.natural, natural),
        ] {
            let start = u32::try_from(blob.len())
                .map_err(|_| Error::InvalidIndex("catalog blob exceeds u32".into()))?;
            let len = u32::try_from(value.len())
                .map_err(|_| Error::InvalidIndex("catalog field exceeds u32".into()))?;
            self.records.extend_from_slice(&start.to_le_bytes());
            self.records.extend_from_slice(&len.to_le_bytes());
            blob.extend_from_slice(value);
        }
        let (start, len) = if self.count > 0 && self.last_parent == parent {
            self.last_parent_range
        } else {
            let start = u32::try_from(self.paths.len())
                .map_err(|_| Error::InvalidIndex("catalog parent blob exceeds u32".into()))?;
            let len = u32::try_from(parent.len())
                .map_err(|_| Error::InvalidIndex("catalog parent exceeds u32".into()))?;
            self.paths.extend_from_slice(parent);
            self.last_parent.clear();
            self.last_parent.extend_from_slice(parent);
            self.last_parent_range = (start, len);
            (start, len)
        };
        self.records.extend_from_slice(&start.to_le_bytes());
        self.records.extend_from_slice(&len.to_le_bytes());
        self.records.extend_from_slice(&record.size.to_le_bytes());
        self.records
            .extend_from_slice(&record.modified_secs.to_le_bytes());
        let flags = u32::from(record.is_dir)
            | (u32::from(record.hidden) << 1)
            | (u32::from(record.symlink) << 2)
            | (u32::from(substring_safe) << 3);
        self.records.extend_from_slice(&flags.to_le_bytes());
        self.records
            .extend_from_slice(&signature.digits.to_le_bytes());
        self.records
            .extend_from_slice(&signature.ascii[0].to_le_bytes());
        self.records
            .extend_from_slice(&signature.ascii[1].to_le_bytes());
        self.records
            .extend_from_slice(&signature.unicode.to_le_bytes());
        self.records
            .extend_from_slice(&record.created_secs.unwrap_or(i64::MIN).to_le_bytes());
        self.records
            .extend_from_slice(&record.accessed_secs.unwrap_or(i64::MIN).to_le_bytes());
        self.count += 1;
        Ok(())
    }
    pub fn header(&self, first_id: u64) -> [u8; HEADER_BYTES] {
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&first_id.to_le_bytes());
        header[16..20].copy_from_slice(&(self.count as u32).to_le_bytes());
        let mut offset = HEADER_BYTES;
        for (i, part) in self.parts().iter().enumerate() {
            header[20 + i * 4..24 + i * 4].copy_from_slice(&(offset as u32).to_le_bytes());
            offset += part.len();
        }
        header[40..44].copy_from_slice(&(offset as u32).to_le_bytes());
        header[44..48].copy_from_slice(&(RECORD_BYTES as u32).to_le_bytes());
        if let Some(start) = self.filter_start {
            // Optional extension is trailing unused path-blob storage. Older v4
            // readers ignore it and retain identical row offsets and semantics.
            let absolute = offset - self.paths.len() + start;
            header[48..52].copy_from_slice(&(absolute as u32).to_le_bytes());
            header[52..56].copy_from_slice(&super::bitmap::MARKER.to_le_bytes());
        }
        header
    }
}

pub(super) struct Shard {
    pub path: PathBuf,
    pub first_id: u64,
    pub count: usize,
    pub bytes: u64,
    pub checksum: u32,
    pub verified: std::sync::atomic::AtomicBool,
}

pub(super) struct View {
    // Drop the mapped view first. The read guard denies write/delete sharing on Windows.
    #[cfg(windows)]
    bytes: memmap2::Mmap,
    #[cfg(not(windows))]
    bytes: Vec<u8>,
    _guard: fs::File,
    offsets: [usize; 6],
    count: usize,
    filter_start: Option<usize>,
}
impl View {
    pub fn select(
        &self,
        required: Option<&Signature>,
        literal_ascii: bool,
        show_hidden: bool,
        kind: crate::KindFilter,
        range: std::ops::Range<usize>,
    ) -> Result<Option<super::bitmap::Selection>> {
        self.filter_start
            .map(|start| {
                super::bitmap::View::new(&self.bytes[start..], self.count)?.select(
                    required,
                    literal_ascii,
                    show_hidden,
                    kind,
                    range,
                )
            })
            .transpose()
    }
    #[inline]
    pub fn filter(
        &self,
        row: usize,
        required: Option<&Signature>,
        literal_ascii: bool,
    ) -> Result<(u32, bool)> {
        if row >= self.count {
            return Err(Error::InvalidCursor);
        }
        let start = HEADER_BYTES + row * RECORD_BYTES;
        let header = &self.bytes[start..start + RECORD_BYTES];
        let flags = u32_at(header, 48);
        let admitted = literal_ascii && flags & 8 == 0
            || required.is_none_or(|required| {
                Signature {
                    ascii: [u64_at(header, 56), u64_at(header, 64)],
                    unicode: u64_at(header, 72),
                    digits: u32_at(header, 52),
                }
                .admits(required)
            });
        Ok((flags, admitted))
    }
    pub fn open(shard: &Shard) -> Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let guard = options.open(&shard.path)?;
        let length = guard.metadata()?.len();
        if length != shard.bytes || length < HEADER_BYTES as u64 || length > 2 * 1024 * 1024 {
            return Err(Error::InvalidIndex("catalog shard length mismatch".into()));
        }
        #[cfg(windows)]
        // SAFETY: the shard writer was closed before publication. The retained read
        // guard denies all write/delete access (including pre-existing writers),
        // prevents replacement/truncation, and outlives this read-only mapping.
        let bytes = unsafe { memmap2::MmapOptions::new().map(&guard)? };
        #[cfg(not(windows))]
        let bytes = {
            use std::io::Read;
            let mut data = Vec::with_capacity(length as usize);
            (&guard).read_to_end(&mut data)?;
            data
        };
        if !shard.verified.load(std::sync::atomic::Ordering::Acquire) {
            if crc32fast::hash(&bytes) != shard.checksum {
                return Err(Error::InvalidIndex(
                    "catalog shard checksum mismatch".into(),
                ));
            }
            shard
                .verified
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if &bytes[..8] != MAGIC
            || u64_at(&bytes, 8) != shard.first_id
            || u32_at(&bytes, 16) as usize != shard.count
            || u32_at(&bytes, 44) as usize != RECORD_BYTES
        {
            return Err(Error::InvalidIndex(
                "catalog shard identity mismatch".into(),
            ));
        }
        let offsets = std::array::from_fn(|i| u32_at(&bytes, 20 + i * 4) as usize);
        if offsets[0] != HEADER_BYTES
            || offsets[1] != HEADER_BYTES + shard.count * RECORD_BYTES
            || offsets[5] != bytes.len()
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(Error::InvalidIndex("invalid catalog shard offsets".into()));
        }
        let filter_start = if u32_at(&bytes, 52) == 0 && u32_at(&bytes, 48) == 0 {
            None
        } else {
            let start = u32_at(&bytes, 48) as usize;
            if u32_at(&bytes, 52) != super::bitmap::MARKER
                || start < offsets[4]
                || start >= offsets[5]
            {
                return Err(Error::InvalidIndex("invalid catalog bitmap offset".into()));
            }
            super::bitmap::View::new(&bytes[start..], shard.count)?;
            Some(start)
        };
        Ok(Self {
            bytes,
            _guard: guard,
            offsets,
            count: shard.count,
            filter_start,
        })
    }
    pub fn record(&self, row: usize) -> Result<Record<'_>> {
        if row >= self.count {
            return Err(Error::InvalidCursor);
        }
        let start = HEADER_BYTES + row * RECORD_BYTES;
        let header = &self.bytes[start..start + RECORD_BYTES];
        let mut fields = [&[][..]; 4];
        for (i, field) in fields.iter_mut().enumerate() {
            let begin = u32_at(header, i * 8) as usize;
            let length = u32_at(header, i * 8 + 4) as usize;
            let end = if i == 3 {
                self.filter_start.unwrap_or(self.offsets[i + 2])
            } else {
                self.offsets[i + 2]
            };
            let blob = &self.bytes[self.offsets[i + 1]..end];
            *field = blob
                .get(begin..begin.saturating_add(length))
                .ok_or_else(|| Error::InvalidIndex("invalid catalog record field".into()))?;
        }
        Ok(Record {
            fields,
            flags: u32_at(header, 48),
            size: u64_at(header, 32),
            modified: u64_at(header, 40) as i64,
            created: (u64_at(header, 80) as i64 != i64::MIN).then(|| u64_at(header, 80) as i64),
            accessed: (u64_at(header, 88) as i64 != i64::MIN).then(|| u64_at(header, 88) as i64),
        })
    }
}

pub(super) struct Record<'a> {
    fields: [&'a [u8]; 4],
    pub flags: u32,
    pub size: u64,
    pub modified: i64,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
}
impl<'a> Record<'a> {
    pub fn name(&self) -> &'a [u8] {
        self.fields[0]
    }
    pub fn lower(&self) -> &'a [u8] {
        self.fields[1]
    }
    pub fn natural(&self) -> &'a [u8] {
        self.fields[2]
    }
    pub fn path(&self) -> &'a [u8] {
        self.fields[3]
    }
}
#[inline]
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
#[inline]
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
