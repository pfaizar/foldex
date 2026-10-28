//! Shared preferences use field deltas under a cross-process lock. A window's
//! unchanged fields never replace a newer value written by another window.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::Path,
};

pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
const FIELDS: &[&str] = &[
    "bookmarks",
    "recent",
    "home_history",
    "sidebar_preferences",
    "saved_layouts",
    "folder_views",
    "command_bookmarks",
    "context_pins",
    "hotkeys",
    "launcher",
    "saved_searches",
    "developer_mode",
];

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    revision: u64,
    preferences: Value,
}

pub(crate) fn preferences(session: &Value) -> Value {
    Value::Object(
        FIELDS
            .iter()
            .filter_map(|key| {
                session
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

pub(crate) fn overlay(session: &mut Value, preferences: &Value) {
    if let (Some(session), Some(preferences)) = (session.as_object_mut(), preferences.as_object()) {
        for &key in FIELDS {
            if let Some(value) = preferences.get(key) {
                session.insert(key.to_owned(), value.clone());
            }
        }
    }
}

pub(super) fn strip(session: &mut Value) {
    if let Some(object) = session.as_object_mut() {
        for key in FIELDS {
            object.remove(*key);
        }
    }
}

fn read(path: &Path) -> io::Result<Option<Document>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Shared preferences exceed 4 MiB",
        ));
    }
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.version != 1 || !document.preferences.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported shared preferences format; the original file was preserved",
        ));
    }
    validate(&document.preferences)?;
    Ok(Some(document))
}

fn validate(preferences: &Value) -> io::Result<()> {
    for (field, limit) in [
        ("bookmarks", crate::bookmarks::MAX_BOOKMARKS),
        ("saved_layouts", 16),
        ("folder_views", 128),
        ("saved_searches", 32),
    ] {
        if preferences
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|values| values.len() > limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Shared {field} exceed the {limit}-item limit; remove an unused item and save again"
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn load(path: &Path, session: &mut Value) -> io::Result<()> {
    if let Some(document) = read(path)? {
        overlay(session, &document.preferences);
    }
    Ok(())
}

pub(super) fn current(path: &Path) -> io::Result<Option<Value>> {
    Ok(read(path)?.map(|document| document.preferences))
}

pub(super) fn save(path: &Path, baseline: &Value, desired: &Value) -> io::Result<Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let previous = read(path)?;
    let current = previous
        .as_ref()
        .map_or(baseline, |document| &document.preferences);
    let merged = merge(baseline, current, desired);
    validate(&merged)?;
    if previous.is_some() && &merged == current {
        return Ok(merged);
    }
    let document = Document {
        version: 1,
        revision: previous
            .as_ref()
            .map_or(1, |document| document.revision.saturating_add(1)),
        preferences: merged.clone(),
    };
    let bytes = serde_json::to_vec(&document)?;
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Shared preferences exceed 4 MiB; remove unused saved layouts or collections",
        ));
    }
    if let Some(previous) = previous {
        crate::platform::atomic_save(
            &path.with_extension("previous.json"),
            &serde_json::to_vec(&previous)?,
        )?;
    }
    crate::platform::atomic_save(path, &bytes)?;
    Ok(merged)
}

pub(crate) fn merge(baseline: &Value, current: &Value, desired: &Value) -> Value {
    merge_field("", baseline, current, desired)
}

fn merge_field(field: &str, baseline: &Value, current: &Value, desired: &Value) -> Value {
    if desired == baseline || current == desired {
        return current.clone();
    }
    if current == baseline {
        return desired.clone();
    }
    match (baseline, current, desired) {
        (Value::Object(base), Value::Object(remote), Value::Object(local)) => {
            let mut result = remote.clone();
            for key in base.keys().chain(local.keys()).collect::<BTreeSet<_>>() {
                let before = base.get(key).unwrap_or(&Value::Null);
                let next = local.get(key).unwrap_or(&Value::Null);
                if before == next {
                    continue;
                }
                if !local.contains_key(key) {
                    result.remove(key);
                } else {
                    result.insert(
                        key.clone(),
                        merge_field(key, before, remote.get(key).unwrap_or(&Value::Null), next),
                    );
                }
            }
            Value::Object(result)
        }
        (Value::Array(base), Value::Array(remote), Value::Array(local)) => {
            let key = match field {
                "hotkeys" => Some("command"),
                "saved_layouts" | "saved_searches" => Some("name"),
                "folder_views" | "entries" => Some("path"),
                _ => None,
            };
            if let Some(key) = key {
                let same = |left: &Value, right: &Value| match (left.get(key), right.get(key)) {
                    (Some(left), Some(right)) if key == "path" => path_value_eq(left, right),
                    (Some(left), Some(right)) => left == right,
                    _ => false,
                };
                let mut result = remote.clone();
                for old in base {
                    if old.get(key).is_some() && !local.iter().any(|value| same(old, value)) {
                        result.retain(|value| !same(old, value));
                    }
                }
                for next in local {
                    if next.get(key).is_none() {
                        return desired.clone();
                    }
                    let old = base.iter().find(|value| same(value, next));
                    if old == Some(next) {
                        continue;
                    }
                    if let Some(at) = result.iter().position(|value| same(value, next)) {
                        // Frequency is a bounded recommendation heuristic. Taking maxima
                        // keeps retries idempotent and preserves the newest activity.
                        let mut merged =
                            merge_field(field, old.unwrap_or(&Value::Null), &result[at], next);
                        if field == "entries"
                            && let Some(object) = merged.as_object_mut()
                        {
                            let uses = result[at]
                                .get("uses")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                .max(next.get("uses").and_then(Value::as_u64).unwrap_or(0))
                                .min(1_000_000);
                            object.insert("uses".into(), uses.into());
                            let time = result[at]
                                .get("last_used")
                                .and_then(Value::as_i64)
                                .unwrap_or(0)
                                .max(next.get("last_used").and_then(Value::as_i64).unwrap_or(0));
                            object.insert("last_used".into(), time.into());
                        }
                        result[at] = merged;
                    } else {
                        result.push(next.clone());
                    }
                }
                Value::Array(result)
            } else if matches!(
                field,
                "bookmarks" | "recent" | "command_bookmarks" | "context_pins"
            ) && base
                .iter()
                .chain(remote)
                .chain(local)
                .all(|value| !value.is_object() && !value.is_array())
            {
                let contains = |values: &Vec<Value>, value: &Value| {
                    values.iter().any(|old| {
                        if matches!(field, "bookmarks" | "recent") {
                            path_value_eq(old, value)
                        } else {
                            old == value
                        }
                    })
                };
                // Membership changes merge independently; unchanged membership and
                // order from the other writer survive an unrelated local edit.
                let mut result = remote
                    .iter()
                    .filter(|value| !contains(base, value) || contains(local, value))
                    .cloned()
                    .collect::<Vec<_>>();
                for value in local {
                    if !contains(base, value) && !contains(&result, value) {
                        result.push(value.clone());
                    }
                }
                if base.iter().all(|value| contains(local, value))
                    && local.iter().all(|value| contains(base, value))
                {
                    let mut ordered = local
                        .iter()
                        .filter(|value| contains(remote, value))
                        .cloned()
                        .collect::<Vec<_>>();
                    ordered.extend(
                        remote
                            .iter()
                            .filter(|value| !contains(base, value))
                            .cloned(),
                    );
                    return Value::Array(ordered);
                }
                Value::Array(result)
            } else {
                desired.clone()
            }
        }
        _ => desired.clone(),
    }
}

fn path_value_eq(left: &Value, right: &Value) -> bool {
    match (left.as_str(), right.as_str()) {
        (Some(left), Some(right)) => crate::bookmarks::path_eq(Path::new(left), Path::new(right)),
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn stale_fields_and_independent_nested_and_collection_edits_merge() {
        let base = json!({"bookmarks":["a"],"launcher":{"hotkey":"old","background":true},"hotkeys":[{"command":"Open","sequence":[1]},{"command":"Copy","sequence":[2]}]});
        let remote = json!({"bookmarks":["a","b"],"launcher":{"hotkey":"new","background":true},"hotkeys":[{"command":"Open","sequence":[3]},{"command":"Copy","sequence":[2]}]});
        let local = json!({"bookmarks":["a","c"],"launcher":{"hotkey":"old","background":false},"hotkeys":[{"command":"Open","sequence":[1]},{"command":"Copy","sequence":[4]}]});
        let merged = merge(&base, &remote, &local);
        assert_eq!(merged["bookmarks"], json!(["a", "b", "c"]));
        assert_eq!(
            merged["launcher"],
            json!({"hotkey":"new","background":false})
        );
        assert_eq!(merged["hotkeys"][0]["sequence"], json!([3]));
        assert_eq!(merged["hotkeys"][1]["sequence"], json!([4]));
    }
    #[test]
    fn removals_do_not_resurrect_and_stale_saves_are_idempotent() {
        let base = json!({"bookmarks":["a","b"],"saved_layouts":[{"name":"One","value":1}]});
        let remote = json!({"bookmarks":["a","b","c"],"saved_layouts":[{"name":"One","value":2}]});
        let local = json!({"bookmarks":["a"],"saved_layouts":[]});
        let merged = merge(&base, &remote, &local);
        assert_eq!(merged, json!({"bookmarks":["a","c"],"saved_layouts":[]}));
        assert_eq!(merge(&base, &merged, &base), merged);
    }
    #[test]
    fn history_combines_usage_and_newest_timestamp() {
        let base = json!({"entries":[{"path":"a","uses":4,"last_used":10}]});
        let remote = json!({"entries":[{"path":"a","uses":6,"last_used":30}]});
        let local = json!({"entries":[{"path":"a","uses":5,"last_used":20}]});
        assert_eq!(
            merge(&base, &remote, &local),
            json!({"entries":[{"path":"a","uses":6,"last_used":30}]})
        );
    }
    #[test]
    fn keyboard_sequences_are_atomic_and_reordering_keeps_remote_additions() {
        assert_eq!(
            merge(
                &json!({"sequence":[1]}),
                &json!({"sequence":[2]}),
                &json!({"sequence":[3,4]})
            ),
            json!({"sequence":[3,4]})
        );
        assert_eq!(
            merge(
                &json!({"bookmarks":["a","b"]}),
                &json!({"bookmarks":["a","b","c"]}),
                &json!({"bookmarks":["b","a"]})
            ),
            json!({"bookmarks":["b","a","c"]})
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_path_spellings_do_not_duplicate_membership_or_folder_views() {
        let base = json!({"bookmarks":["C:/Work"],"folder_views":[{"path":"C:/Work","zoom":0}]});
        let remote =
            json!({"bookmarks":["C:/Work","C:/Docs"],"folder_views":[{"path":"C:/Work","zoom":1}]});
        let local =
            json!({"bookmarks":["c:/work","c:/docs"],"folder_views":[{"path":"c:/work","zoom":0}]});
        let merged = merge(&base, &remote, &local);
        assert_eq!(merged["bookmarks"].as_array().unwrap().len(), 2);
        assert_eq!(merged["folder_views"].as_array().unwrap().len(), 1);
    }
}
