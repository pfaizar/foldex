//! On-demand, per-user Windows folder registration. Never called from rendering.
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[cfg(windows)]
mod windows;

const BACKUP_KEY: &str = r"Software\Foldex\ExplorerIntegration";
const BACKUP_VALUE: &str = "RestoreV1";
const CLASSES: &str = r"Software\Classes";
const WIN_E: &str =
    r"Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}\shell\opennewwindow\command";
const VERB: &str = "Foldex.Open";
const MAX_VALUE: usize = 64 * 1024;
const MAX_BACKUP: usize = 4 * 1024 * 1024;

/// Remove the quoting guard appended by our registry command, before normal
/// startup resolution. Verbatim paths must not be asked to resolve a fake dot.
pub(crate) fn folder_argument(argument: &OsStr) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut value: Vec<u16> = argument.encode_wide().collect();
        if value.ends_with(&[92, 46]) {
            value.truncate(value.len() - 2);
        }
        PathBuf::from(OsString::from_wide(&value))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(argument)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub partial: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}
impl Value {
    fn text(value: &str) -> Self {
        Self::wide(value.encode_utf16())
    }
    fn wide(value: impl Iterator<Item = u16>) -> Self {
        Self {
            kind: 1, // REG_SZ
            bytes: value.chain([0]).flat_map(u16::to_le_bytes).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Change {
    key: String,
    name: String,
    previous: Option<Value>,
    applied: Value,
}
#[derive(Serialize, Deserialize)]
struct Backup {
    version: u32,
    changes: Vec<Change>,
    created: Vec<String>,
}

trait Registry {
    fn read(&self, key: &str, name: &str) -> Result<Option<Value>, String>;
    fn exists(&self, key: &str) -> Result<bool, String>;
    fn write(&mut self, key: &str, name: &str, value: &Value) -> Result<(), String>;
    fn remove(&mut self, key: &str, name: &str) -> Result<(), String>;
    fn remove_empty_key(&mut self, key: &str) -> Result<(), String>;
}

#[cfg(windows)]
fn shell_path(path: &Path) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        value.drain(..8);
        value.splice(..0, [92, 92]);
    } else if value.starts_with(&[92, 92, 63, 92])
        && value.len() >= 7
        && matches!(value[4], 65..=90 | 97..=122)
        && value[5] == 58
    {
        value.drain(..4);
    }
    OsString::from_wide(&value)
}

#[cfg(windows)]
fn command(executable: &Path, folder: bool) -> Value {
    use std::os::windows::ffi::OsStrExt;
    let mut line = OsString::from("\"");
    line.push(shell_path(executable));
    // A final dot keeps a drive root's trailing slash away from the closing
    // quote. Windows resolves "C:\\." and "folder\." to the same directories.
    line.push(if folder {
        "\" --shell-open \"%1\\.\""
    } else {
        "\" --shell-home"
    });
    Value::wide(line.encode_wide())
}
#[cfg(not(windows))]
fn command(executable: &Path, folder: bool) -> Value {
    Value::text(&format!(
        "\"{}\" {}",
        executable.display(),
        if folder {
            "--shell-open \"%1\\.\""
        } else {
            "--shell-home"
        }
    ))
}

fn plan(executable: &Path) -> Vec<Change> {
    let mut result = Vec::new();
    let mut add = |key: String, name: &str, applied: Value| {
        result.push(Change {
            key,
            name: name.into(),
            previous: None,
            applied,
        });
    };
    for class in ["Directory", "Drive"] {
        let shell = format!(r"{CLASSES}\{class}\shell");
        let verb = format!(r"{shell}\{VERB}");
        add(shell, "", Value::text(VERB));
        add(verb.clone(), "", Value::text("Open in Foldex"));
        add(verb.clone(), "MultiSelectModel", Value::text("Document"));
        add(format!(r"{verb}\command"), "", command(executable, true));
    }
    add(WIN_E.into(), "", command(executable, false));
    add(WIN_E.into(), "DelegateExecute", Value::text(""));
    result
}

fn prefixes(key: &str) -> Vec<String> {
    let mut result = Vec::new();
    let Some(rest) = key.strip_prefix(&format!(r"{CLASSES}\")) else {
        return result;
    };
    let mut path = CLASSES.to_owned();
    for part in rest.split('\\') {
        path.push('\\');
        path.push_str(part);
        result.push(path.clone());
    }
    result
}

fn load(registry: &impl Registry) -> Result<Option<Backup>, String> {
    let Some(value) = registry.read(BACKUP_KEY, BACKUP_VALUE)? else {
        return Ok(None);
    };
    if value.kind != 3 || value.bytes.len() > MAX_BACKUP {
        return Err("The saved Explorer integration backup is invalid.".into());
    }
    let backup: Backup = serde_json::from_slice(&value.bytes)
        .map_err(|_| "The saved Explorer integration backup could not be read.")?;
    let expected = plan(Path::new("Foldex.exe"));
    if backup.version != 1
        || backup.changes.len() != expected.len()
        || backup.created.len() > 64
        || !backup.changes.iter().zip(&expected).all(|(a, b)| {
            a.key == b.key
                && a.name == b.name
                && a.applied.kind == 1
                && a.applied.bytes.len() <= MAX_VALUE
                && a.previous
                    .as_ref()
                    .is_none_or(|p| p.bytes.len() <= MAX_VALUE)
        })
        || backup
            .created
            .iter()
            .any(|key| !expected.iter().any(|c| prefixes(&c.key).contains(key)))
    {
        return Err("The saved Explorer integration backup has unsupported contents.".into());
    }
    Ok(Some(backup))
}

fn inspect(registry: &impl Registry) -> Result<Status, String> {
    let Some(backup) = load(registry)? else {
        return Ok(Status::default());
    };
    let mut matching = 0;
    for change in &backup.changes {
        matching += usize::from(
            registry.read(&change.key, &change.name)?.as_ref() == Some(&change.applied),
        );
    }
    Ok(Status {
        enabled: matching == backup.changes.len(),
        partial: matching > 0 && matching < backup.changes.len(),
    })
}

fn foreign_win_e(registry: &impl Registry, changes: &[Change]) -> Result<bool, String> {
    for change in changes.iter().filter(|change| change.key == WIN_E) {
        let current = registry.read(&change.key, &change.name)?;
        // Applied/previous mixtures can be an interrupted installation. A value
        // outside both states belongs to a later editor/manager; preserve the
        // command and DelegateExecute together because they form one endpoint.
        if current.as_ref() != Some(&change.applied) && current != change.previous {
            return Ok(true);
        }
    }
    Ok(false)
}

fn restore(registry: &mut impl Registry, backup: &Backup) -> Result<(), String> {
    let preserve_win_e = foreign_win_e(registry, &backup.changes)?;
    for change in backup.changes.iter().rev() {
        if preserve_win_e && change.key == WIN_E {
            continue;
        }
        // Preserve choices made by another application after Foldex was enabled.
        if registry.read(&change.key, &change.name)?.as_ref() == Some(&change.applied) {
            if let Some(previous) = &change.previous {
                registry.write(&change.key, &change.name, previous)?;
            } else {
                registry.remove(&change.key, &change.name)?;
            }
        }
    }
    let mut keys = backup.created.clone();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    for key in keys {
        registry.remove_empty_key(&key)?;
    }
    Ok(())
}

fn apply(registry: &mut impl Registry, executable: &Path, enabled: bool) -> Result<Status, String> {
    let old = load(registry)?;
    if !enabled {
        if let Some(backup) = old {
            restore(registry, &backup)?;
            registry.remove(BACKUP_KEY, BACKUP_VALUE)?;
        }
        return inspect(registry);
    }
    let mut changes = plan(executable);
    let win_e_changed = if let Some(old) = &old {
        foreign_win_e(registry, &old.changes)?
    } else {
        false
    };
    let mut immediate = Vec::new();
    let mut newly_created = Vec::new();
    let mut created = old.as_ref().map_or_else(Vec::new, |b| b.created.clone());
    for change in &mut changes {
        let current = registry.read(&change.key, &change.name)?;
        if current
            .as_ref()
            .is_some_and(|value| value.bytes.len() > MAX_VALUE)
            || change.applied.bytes.len() > MAX_VALUE
        {
            return Err("An Explorer registration value exceeds the supported size.".into());
        }
        immediate.push(Change {
            key: change.key.clone(),
            name: change.name.clone(),
            previous: current.clone(),
            applied: change.applied.clone(),
        });
        change.previous = old
            .as_ref()
            .and_then(|b| {
                b.changes
                    .iter()
                    .find(|c| c.key == change.key && c.name == change.name)
            })
            .filter(|previous| {
                current.as_ref() == Some(&previous.applied)
                    && !(win_e_changed && change.key == WIN_E)
            })
            .map_or(current, |previous| previous.previous.clone());
        for key in prefixes(&change.key) {
            if !registry.exists(&key)? {
                if !newly_created.contains(&key) {
                    newly_created.push(key.clone());
                }
                if !created.contains(&key) {
                    created.push(key);
                }
            }
        }
    }
    let backup = Backup {
        version: 1,
        changes,
        created,
    };
    let bytes = serde_json::to_vec(&backup).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BACKUP {
        return Err("Explorer integration backup exceeds its size limit.".into());
    }
    // Persist recovery data before changing any shell association.
    registry.write(BACKUP_KEY, BACKUP_VALUE, &Value { kind: 3, bytes })?;
    for change in &backup.changes {
        if let Err(error) = registry.write(&change.key, &change.name, &change.applied) {
            let rollback = Backup {
                version: 1,
                changes: immediate,
                created: newly_created,
            };
            return match restore(registry, &rollback).and_then(|()| {
                if let Some(old) = &old {
                    registry.write(
                        BACKUP_KEY,
                        BACKUP_VALUE,
                        &Value {
                            kind: 3,
                            bytes: serde_json::to_vec(old).map_err(|e| e.to_string())?,
                        },
                    )
                } else {
                    registry.remove(BACKUP_KEY, BACKUP_VALUE)
                }
            }) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!("{error} Restore was incomplete: {rollback}")),
            };
        }
    }
    inspect(registry)
}

#[cfg(windows)]
pub fn run(enabled: Option<bool>) -> Result<Status, String> {
    let _guard = windows::RegistrationLock::acquire()?;
    let mut registry = windows::UserRegistry::default();
    if let Some(enabled) = enabled {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let result = apply(&mut registry, &executable, enabled);
        windows::notify();
        result
    } else {
        inspect(&registry)
    }
}
#[cfg(not(windows))]
pub fn run(_: Option<bool>) -> Result<Status, String> {
    Err("Default file explorer integration is available on Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    #[derive(Default)]
    struct MemoryRegistry {
        values: BTreeMap<(String, String), Value>,
        keys: BTreeSet<String>,
        fail_after: Option<usize>,
    }
    impl Registry for MemoryRegistry {
        fn read(&self, key: &str, name: &str) -> Result<Option<Value>, String> {
            Ok(self.values.get(&(key.into(), name.into())).cloned())
        }
        fn exists(&self, key: &str) -> Result<bool, String> {
            Ok(self.keys.contains(key))
        }
        fn write(&mut self, key: &str, name: &str, value: &Value) -> Result<(), String> {
            if let Some(left) = &mut self.fail_after {
                if *left == 0 {
                    self.fail_after = None;
                    return Err("Injected registry write failure".into());
                }
                *left -= 1;
            }
            self.keys.insert(key.into());
            for prefix in prefixes(key) {
                self.keys.insert(prefix);
            }
            self.values.insert((key.into(), name.into()), value.clone());
            Ok(())
        }
        fn remove(&mut self, key: &str, name: &str) -> Result<(), String> {
            self.values.remove(&(key.into(), name.into()));
            Ok(())
        }
        fn remove_empty_key(&mut self, key: &str) -> Result<(), String> {
            if !self.values.keys().any(|(k, _)| k == key)
                && !self.keys.iter().any(|k| k.starts_with(&format!(r"{key}\")))
            {
                self.keys.remove(key);
            }
            Ok(())
        }
    }
    fn preexisting() -> MemoryRegistry {
        let mut r = MemoryRegistry::default();
        r.write(
            r"Software\Classes\Directory\shell",
            "",
            &Value::text("Other.Explorer"),
        )
        .unwrap();
        r.write(
            WIN_E,
            "DelegateExecute",
            &Value {
                kind: 2,
                bytes: vec![1, 0, 9, 0, 0, 0],
            },
        )
        .unwrap();
        r
    }
    #[cfg(windows)]
    #[test]
    fn shell_argument_guard_preserves_raw_root_unc_and_verbatim_paths() {
        for (given, expected) in [
            (r"C:\\.", r"C:\"),
            (r"C:\Folder Space\東京\.", r"C:\Folder Space\東京"),
            (r"\\server\share\.", r"\\server\share"),
            (r"\\?\C:\ends.\.", r"\\?\C:\ends."),
            (r"\\?\UNC\server\share\.", r"\\?\UNC\server\share"),
        ] {
            assert_eq!(
                folder_argument(OsStr::new(given)).as_os_str(),
                OsStr::new(expected)
            );
        }
        use std::os::windows::ffi::OsStringExt;
        let raw = OsString::from_wide(&[67, 58, 92, 0xd800, 92, 46]);
        assert_eq!(
            folder_argument(&raw).as_os_str(),
            OsString::from_wide(&[67, 58, 92, 0xd800])
        );
    }
    #[test]
    fn disabling_restores_exact_previous_values_and_absence() {
        let mut r = preexisting();
        let values = r.values.clone();
        let keys = r.keys.clone();
        assert!(
            apply(&mut r, Path::new(r"C:\Program Space\Foldex.exe"), true)
                .unwrap()
                .enabled
        );
        assert_eq!(
            apply(&mut r, Path::new("unused"), false).unwrap(),
            Status::default()
        );
        assert_eq!(r.values, values);
        assert!(
            r.keys
                .iter()
                .filter(|k| k.starts_with(CLASSES))
                .all(|k| keys.contains(k))
        );
    }
    #[test]
    fn failed_enable_rolls_back_and_preserves_other_app_changes() {
        let mut r = preexisting();
        let before = r.values.clone();
        r.fail_after = Some(5);
        assert!(apply(&mut r, Path::new("Foldex.exe"), true).is_err());
        assert_eq!(r.values, before);
        apply(&mut r, Path::new("Foldex.exe"), true).unwrap();
        let key = r"Software\Classes\Directory\shell";
        r.write(key, "", &Value::text("Newer.Explorer")).unwrap();
        let newer = Value::text(r#""C:\Other\Explorer.exe""#);
        r.write(WIN_E, "", &newer).unwrap();
        assert!(inspect(&r).unwrap().partial);
        apply(&mut r, Path::new("unused"), false).unwrap();
        assert_eq!(
            r.read(key, "").unwrap(),
            Some(Value::text("Newer.Explorer"))
        );
        assert_eq!(r.read(WIN_E, "").unwrap(), Some(newer));
        assert_eq!(
            r.read(WIN_E, "DelegateExecute").unwrap(),
            Some(Value::text(""))
        );
    }
    #[test]
    fn a_later_win_e_handler_becomes_the_new_restore_point() {
        let mut r = preexisting();
        apply(&mut r, Path::new("Foldex.exe"), true).unwrap();
        let newer = Value::text(r#""C:\Other\Explorer.exe""#);
        r.write(WIN_E, "", &newer).unwrap();
        apply(&mut r, Path::new("New-Foldex.exe"), true).unwrap();
        apply(&mut r, Path::new("unused"), false).unwrap();
        assert_eq!(r.read(WIN_E, "").unwrap(), Some(newer));
        assert_eq!(
            r.read(WIN_E, "DelegateExecute").unwrap(),
            Some(Value::text(""))
        );
    }
    #[test]
    fn failure_between_win_e_values_restores_the_original_endpoint() {
        let mut r = preexisting();
        let original = r.values.clone();
        r.fail_after = Some(10);
        assert!(apply(&mut r, Path::new("Foldex.exe"), true).is_err());
        assert_eq!(r.values, original);
    }
    #[test]
    fn updating_the_executable_keeps_the_original_restore_point() {
        let mut r = preexisting();
        let original = r.values.clone();
        apply(&mut r, Path::new(r"C:\Old\Foldex.exe"), true).unwrap();
        apply(&mut r, Path::new(r"C:\New\Foldex.exe"), true).unwrap();
        apply(&mut r, Path::new("unused"), false).unwrap();
        assert_eq!(r.values, original);
    }
    #[test]
    fn a_failed_update_preserves_the_active_registration_and_its_backup() {
        let mut r = preexisting();
        apply(&mut r, Path::new(r"C:\Old\Foldex.exe"), true).unwrap();
        let before = r.values.clone();
        r.fail_after = Some(6);
        assert!(apply(&mut r, Path::new(r"C:\New\Foldex.exe"), true).is_err());
        assert_eq!(r.values, before);
        assert!(inspect(&r).unwrap().enabled);
    }
    #[test]
    fn untrusted_backup_cannot_target_arbitrary_registry_keys() {
        let mut r = preexisting();
        apply(&mut r, Path::new("Foldex.exe"), true).unwrap();
        let mut backup = load(&r).unwrap().unwrap();
        backup.changes[0].key = r"Software\Microsoft\Windows\CurrentVersion\Run".into();
        r.write(
            BACKUP_KEY,
            BACKUP_VALUE,
            &Value {
                kind: 3,
                bytes: serde_json::to_vec(&backup).unwrap(),
            },
        )
        .unwrap();
        let before = r.values.clone();
        assert!(apply(&mut r, Path::new("unused"), false).is_err());
        assert_eq!(r.values, before);
    }
}
