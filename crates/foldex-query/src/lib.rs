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
    pub fn matches(&self, relative: &Path, is_dir: bool, bytes: u64, modified: i64) -> bool {
        if self.excludes(relative)
            || ((self.min_bytes.is_some() || self.max_bytes.is_some()) && is_dir)
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
