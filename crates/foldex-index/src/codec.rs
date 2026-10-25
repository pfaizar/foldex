use crate::model::{Error, Result};
use std::{
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
pub(crate) const CODEC: &str = "windows-wtf8-v1";
#[cfg(unix)]
pub(crate) const CODEC: &str = "unix-bytes-v1";

pub(crate) fn encode(value: &OsStr) -> Vec<u8> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if let Some(text) = value.to_str() {
            return text.as_bytes().to_vec();
        }
        let mut units = value.encode_wide().peekable();
        let mut output = Vec::with_capacity(value.len());
        while let Some(unit) = units.next() {
            let point = if (0xd800..=0xdbff).contains(&unit)
                && units
                    .peek()
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next))
            {
                0x10000 + (((unit as u32) - 0xd800) << 10) + (units.next().unwrap() as u32 - 0xdc00)
            } else {
                unit as u32
            };
            if point < 0x80 {
                output.push(point as u8);
            } else if point < 0x800 {
                output.extend_from_slice(&[0xc0 | (point >> 6) as u8, 0x80 | (point & 63) as u8]);
            } else if point < 0x10000 {
                output.extend_from_slice(&[
                    0xe0 | (point >> 12) as u8,
                    0x80 | ((point >> 6) & 63) as u8,
                    0x80 | (point & 63) as u8,
                ]);
            } else {
                output.extend_from_slice(&[
                    0xf0 | (point >> 18) as u8,
                    0x80 | ((point >> 12) & 63) as u8,
                    0x80 | ((point >> 6) & 63) as u8,
                    0x80 | (point & 63) as u8,
                ]);
            }
        }
        output
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
}

pub(crate) fn decode(value: &[u8]) -> Result<OsString> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        if value.contains(&0) {
            return Err(Error::InvalidIndex("NUL in path".into()));
        }
        if let Ok(text) = std::str::from_utf8(value) {
            return Ok(OsString::from(text));
        }
        let mut units = Vec::with_capacity(value.len());
        let mut position = 0;
        while position < value.len() {
            let first = value[position];
            let (length, minimum, mut point) = match first {
                0x01..=0x7f => (1, 1, first as u32),
                0xc2..=0xdf => (2, 0x80, (first & 31) as u32),
                0xe0..=0xef => (3, 0x800, (first & 15) as u32),
                0xf0..=0xf4 => (4, 0x10000, (first & 7) as u32),
                _ => return Err(Error::InvalidIndex("invalid WTF-8 path blob".into())),
            };
            if position + length > value.len() {
                return Err(Error::InvalidIndex("truncated WTF-8 path blob".into()));
            }
            for &next in &value[position + 1..position + length] {
                if next & 0xc0 != 0x80 {
                    return Err(Error::InvalidIndex("invalid WTF-8 continuation".into()));
                }
                point = (point << 6) | (next & 63) as u32;
            }
            if point < minimum || point > 0x10ffff {
                return Err(Error::InvalidIndex("noncanonical WTF-8 path blob".into()));
            }
            if point <= 0xffff {
                units.push(point as u16);
            } else {
                let value = point - 0x10000;
                units.extend_from_slice(&[
                    (0xd800 | (value >> 10)) as u16,
                    (0xdc00 | (value & 1023)) as u16,
                ]);
            }
            position += length;
        }
        Ok(OsString::from_wide(&units))
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        if value.contains(&0) {
            return Err(Error::InvalidIndex("NUL in path".into()));
        }
        Ok(OsString::from_vec(value.to_vec()))
    }
}

pub(crate) fn relative(path: &Path, allow_empty: bool) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !name.as_encoded_bytes().contains(&0) => result.push(name),
            Component::CurDir => {}
            _ => {
                return Err(Error::InvalidPath(
                    "paths must stay relative to the indexed scope".into(),
                ));
            }
        }
    }
    if result.as_os_str().is_empty() && !allow_empty {
        return Err(Error::InvalidPath("empty relative path".into()));
    }
    Ok(result)
}

/// Lexicographic BLOB order matches allocation-free natural comparison of UTF-8 names.
pub(crate) fn natural_key(name: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(name.len() + 16);
    natural_key_into(name, &mut output);
    output
}

pub(crate) fn natural_key_into(name: &str, output: &mut Vec<u8>) {
    output.clear();
    let bytes = name.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position].is_ascii_digit() {
            let start = position;
            while position < bytes.len() && bytes[position].is_ascii_digit() {
                position += 1;
            }
            let mut significant = start;
            while significant < position && bytes[significant] == b'0' {
                significant += 1;
            }
            output.push(b'0');
            ordered_length(output, (position - significant) as u64);
            output.extend_from_slice(&bytes[significant..position]);
            ordered_length(output, (position - start) as u64);
        } else {
            output.push(bytes[position]);
            position += 1;
        }
    }
}

fn ordered_length(output: &mut Vec<u8>, value: u64) {
    let bytes = (u64::BITS - value.leading_zeros()).div_ceil(8) as usize;
    output.push(bytes as u8);
    output.extend_from_slice(&value.to_be_bytes()[8 - bytes..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_keys_order_numbers_and_prefixes() {
        let mut names = vec![
            "a10",
            "a2b",
            "a2",
            "a02",
            "a000",
            "a0",
            "a/",
            "a:",
            "a999999999999999999999999999999",
        ];
        names.sort_by_cached_key(|name| natural_key(name));
        assert_eq!(
            names,
            [
                "a/",
                "a0",
                "a000",
                "a2",
                "a2b",
                "a02",
                "a10",
                "a999999999999999999999999999999",
                "a:"
            ]
        );
    }
    #[test]
    fn rejects_paths_that_escape_scope() {
        assert!(relative(Path::new("../outside"), false).is_err());
        assert!(relative(Path::new("/outside"), false).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn compact_windows_paths_preserve_every_code_unit_and_reject_invalid_bytes() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = (1..=65535).collect();
        let raw = OsString::from_wide(&units);
        let encoded = encode(&raw);
        assert_eq!(
            decode(&encoded).unwrap().encode_wide().collect::<Vec<_>>(),
            units
        );
        assert_eq!(encode(OsStr::new("folder/file.txt")).len(), 15);
        for invalid in [
            &[0][..],
            &[0xc0, 0x80],
            &[0xe0, 0x80, 0x80],
            &[0xf4, 0x90, 0x80, 0x80],
            &[0xed, 0xa0],
            &[0xff],
        ] {
            assert!(decode(invalid).is_err());
        }
    }
}
