//! Shared metadata predicates. Filename matching remains a caller policy.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Conditions {
    pub path_contains: String,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
    /// Inclusive UTC Unix seconds. Missing timestamps do not satisfy a bound.
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    /// Symbolic saved condition, resolved once when opening a search.
    pub modified_within_days: Option<u16>,
    /// Raw scope-relative paths, including the excluded row and its descendants.
    pub excluded_subtrees: Vec<PathBuf>,
}
impl Conditions {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.path_contains.len() > 4096
            || self.excluded_subtrees.len() > 32
            || self
                .excluded_subtrees
                .iter()
                .map(|p| p.as_os_str().len())
                .sum::<usize>()
                > 16 * 1024
        {
            return Err("Search conditions exceed their path budget".into());
        }
        if self
            .min_bytes
            .zip(self.max_bytes)
            .is_some_and(|(a, b)| a > b)
            || self
                .modified_after
                .zip(self.modified_before)
                .is_some_and(|(a, b)| a > b)
        {
            return Err("The minimum must not exceed the maximum".into());
        }
        if self.excluded_subtrees.iter().any(|p| {
            p.as_os_str().is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_)))
        }) {
            return Err("Excluded folders must be relative paths without . or ..".into());
        }
        Ok(())
    }
    pub fn resolved(&self, now: i64) -> Self {
        let mut value = self.clone();
        if let Some(days) = value.modified_within_days.take() {
            let after = now.saturating_sub(i64::from(days) * 86_400);
            value.modified_after = Some(value.modified_after.map_or(after, |old| old.max(after)));
        }
        value
    }
    pub fn excludes(&self, relative: &Path) -> bool {
        self.excluded_subtrees
            .iter()
            .any(|p| relative.starts_with(p))
    }
    pub fn matches_metadata(&self, is_dir: bool, bytes: u64, modified: i64) -> bool {
        if ((self.min_bytes.is_some() || self.max_bytes.is_some()) && is_dir)
            || self.min_bytes.is_some_and(|min| bytes < min)
            || self.max_bytes.is_some_and(|max| bytes > max)
            || self
                .modified_after
                .is_some_and(|min| modified == 0 || modified < min)
            || self
                .modified_before
                .is_some_and(|max| modified == 0 || modified > max)
        {
            return false;
        }
        true
    }
    pub fn needs_path(&self) -> bool {
        !self.path_contains.is_empty() || !self.excluded_subtrees.is_empty()
    }
    pub fn matches(&self, relative: &Path, is_dir: bool, bytes: u64, modified: i64) -> bool {
        self.matches_metadata(is_dir, bytes, modified) && self.matches_path(relative)
    }
    pub fn matches_path(&self, relative: &Path) -> bool {
        if self.excludes(relative) {
            return false;
        }
        self.path_contains.is_empty()
            || relative
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase()
                .contains(&self.path_contains.replace('\\', "/").to_lowercase())
    }
}

/// Decimal/SI and binary size suffixes; no floats or rounding at u64 boundaries.
pub fn parse_bytes(text: &str) -> Result<Option<u64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let number: u64 = text[..split].parse().map_err(
        |_| "Enter a whole byte count, optionally followed by KB, MB, GB, KiB, MiB or GiB",
    )?;
    let factor = match text[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        _ => return Err("Unknown size unit".into()),
    };
    number
        .checked_mul(factor)
        .map(Some)
        .ok_or_else(|| "Size exceeds the supported range".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_relative_dates_and_raw_component_exclusions() {
        let c = Conditions {
            min_bytes: Some(10),
            max_bytes: Some(20),
            modified_within_days: Some(2),
            excluded_subtrees: vec!["build".into()],
            ..Default::default()
        }
        .resolved(200_000);
        assert!(c.matches(Path::new("build-tools/report"), false, 10, 30_000));
        assert!(!c.matches(Path::new("build/report"), false, 10, 30_000));
        assert!(!c.matches(Path::new("report"), true, 10, 30_000));
        assert!(!c.matches(Path::new("report"), false, 10, 0));
        assert!(!c.matches(Path::new("report"), false, 21, 30_000));
        assert!(
            Conditions {
                excluded_subtrees: vec!["../escape".into()],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn byte_units_overflow_and_invalid_ranges() {
        assert_eq!(parse_bytes("2 MiB").unwrap(), Some(2_097_152));
        assert_eq!(parse_bytes("18446744073709551615").unwrap(), Some(u64::MAX));
        assert!(parse_bytes("18446744073709551615 GB").is_err());
        assert!(parse_bytes("-1").is_err());
        assert!(
            Conditions {
                min_bytes: Some(2),
                max_bytes: Some(1),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}

/// Calendar boundaries in UTC; the upper bound includes the entire chosen day.
pub fn parse_utc_date(text: &str, end_of_day: bool) -> Result<Option<i64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let invalid = || "Enter a valid UTC date as YYYY-MM-DD".to_string();
    if text.len() != 10 || text.as_bytes()[4] != b'-' || text.as_bytes()[7] != b'-' {
        return Err(invalid());
    }
    let year: i64 = text[..4].parse().map_err(|_| invalid())?;
    let month: usize = text[5..7].parse().map_err(|_| invalid())?;
    let day: i64 = text[8..].parse().map_err(|_| invalid())?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        28 + i64::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > months[month - 1]
    {
        return Err(invalid());
    }
    let before = |year: i64| {
        let y = year - 1;
        365 * y + y / 4 - y / 100 + y / 400
    };
    let days = before(year) - before(1970) + months[..month - 1].iter().sum::<i64>() + day - 1;
    Ok(Some(days * 86_400 + if end_of_day { 86_399 } else { 0 }))
}
pub fn format_utc_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86400) + 719468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}")
}
#[cfg(test)]
mod calendar_tests {
    use super::*;
    #[test]
    fn calendar_bounds_round_trip_and_reject_invalid_days() {
        assert_eq!(parse_utc_date("1970-01-01", false).unwrap(), Some(0));
        assert_eq!(parse_utc_date("1970-01-01", true).unwrap(), Some(86399));
        for date in [
            "0001-01-01",
            "1969-12-31",
            "2000-02-29",
            "2024-02-29",
            "9999-12-31",
        ] {
            assert_eq!(
                format_utc_date(parse_utc_date(date, false).unwrap().unwrap()),
                date
            );
        }
        for date in [
            "1900-02-29",
            "2023-02-29",
            "2024-13-01",
            "2024-04-31",
            "2024-00-01",
            "a漢-01-01",
        ] {
            assert!(parse_utc_date(date, false).is_err());
        }
    }
}
