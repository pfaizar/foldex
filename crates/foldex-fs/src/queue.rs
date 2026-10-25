//! Bounded work queue prototype: directory parallelism without a fanout-sized
//! resident path list. Overflow paths use one private sequential spool file.
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

const QUEUED_PATHS: usize = 1024;
const QUEUED_BYTES: usize = 1024 * 1024;
struct Spool {
    path: PathBuf,
    reader: BufReader<fs::File>,
    writer: BufWriter<fs::File>,
    pending: usize,
}
impl Spool {
    fn create(root: &Path) -> io::Result<Self> {
        let path = root.join(format!(
            "owned-{}-{}.queue",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        let reader = BufReader::with_capacity(64 * 1024, fs::File::open(&path)?);
        Ok(Self {
            path,
            reader,
            writer: BufWriter::with_capacity(64 * 1024, file),
            pending: 0,
        })
    }
    fn push(&mut self, path: &Path) -> io::Result<()> {
        #[cfg(windows)]
        let bytes = {
            use std::os::windows::ffi::OsStrExt;
            path.as_os_str()
                .encode_wide()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        };
        #[cfg(not(windows))]
        let bytes = path.as_os_str().as_encoded_bytes().to_vec();
        if bytes.len() > 128 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Queued path too long",
            ));
        }
        self.writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.writer.write_all(&bytes)?;
        self.pending += 1;
        Ok(())
    }
    pub fn pop(&mut self) -> io::Result<Option<PathBuf>> {
        if self.pending == 0 {
            return Ok(None);
        }
        self.writer.flush()?;
        let mut length = [0; 4];
        self.reader.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > 128 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid queued path size",
            ));
        }
        let mut bytes = vec![0; length];
        self.reader.read_exact(&mut bytes)?;
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            if length % 2 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Odd UTF-16 path",
                ));
            }
            PathBuf::from(OsString::from_wide(
                &bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>(),
            ))
        };
        #[cfg(not(windows))]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(OsString::from_vec(bytes))
        };
        if path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid relative queued path",
            ));
        }
        self.pending -= 1;
        if self.pending == 0 {
            self.writer.seek(SeekFrom::Start(0))?;
            self.writer.get_ref().set_len(0)?;
            self.reader.seek(SeekFrom::Start(0))?;
        }
        Ok(Some(path))
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = self.writer.flush();
        let _ = fs::remove_file(&self.path);
    }
}
pub(crate) struct Queue {
    ram: VecDeque<PathBuf>,
    bytes: usize,
    spool: Option<Spool>,
    pub active: usize,
    pub done: bool,
    spilled: u64,
    peak_paths: usize,
    peak_bytes: usize,
}
impl Queue {
    pub fn new() -> Self {
        Self {
            ram: VecDeque::from([PathBuf::new()]),
            bytes: std::mem::size_of::<PathBuf>(),
            spool: None,
            active: 0,
            done: false,
            spilled: 0,
            peak_paths: 1,
            peak_bytes: std::mem::size_of::<PathBuf>(),
        }
    }

    pub fn push(&mut self, path: PathBuf, root: &Path) -> io::Result<()> {
        let bytes = path
            .capacity()
            .saturating_mul(2)
            .saturating_add(std::mem::size_of::<PathBuf>());
        if self.ram.len() < QUEUED_PATHS && self.bytes.saturating_add(bytes) <= QUEUED_BYTES {
            self.bytes += bytes;
            self.ram.push_back(path);
            self.peak_paths = self.peak_paths.max(self.ram.len());
            self.peak_bytes = self.peak_bytes.max(self.bytes);
        } else {
            if self.spool.is_none() {
                self.spool = Some(Spool::create(root)?);
            }
            self.spool.as_mut().unwrap().push(&path)?;
            self.spilled += 1;
        }
        Ok(())
    }
    pub fn pop(&mut self) -> io::Result<Option<PathBuf>> {
        if let Some(path) = self.ram.pop_front() {
            self.bytes = self.bytes.saturating_sub(
                path.capacity()
                    .saturating_mul(2)
                    .saturating_add(std::mem::size_of::<PathBuf>()),
            );
            return Ok(Some(path));
        }
        self.spool.as_mut().map_or(Ok(None), Spool::pop)
    }
    pub fn empty(&self) -> bool {
        self.ram.is_empty() && self.spool.as_ref().is_none_or(|spool| spool.pending == 0)
    }
}
