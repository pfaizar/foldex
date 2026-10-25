//! Read-only by default. --exercise creates its own temporary fixture and only
//! invokes system Folder/ZIP commands against that fixture, never caller files.
use foldex_shell::{BuiltinEvent, BuiltinKind, BuiltinMenus};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn event(menu: &BuiltinMenus) -> Result<BuiltinEvent, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = menu.try_recv() {
            return Ok(event);
        }
        if Instant::now() >= deadline {
            return Err("System menu exceeded 30 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn invoke(
    menu: &BuiltinMenus,
    request: u64,
    kind: BuiltinKind,
    paths: Vec<PathBuf>,
    background: bool,
    key: String,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match menu.invoke(request, kind, 0, paths.clone(), background, key.clone()) {
            Ok(()) => break,
            Err(error) if error.contains("mailbox busy") && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    }
    match event(menu)? {
        BuiltinEvent::Invoked { request: actual } if actual == request => Ok(()),
        other => Err(format!("Unexpected system invocation result: {other:?}")),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let output = PathBuf::from(args.next().ok_or("output JSON path required")?);
    let next = args
        .next()
        .ok_or("absolute folder path or --exercise required")?;
    let exercise = next == "--exercise";
    let folder = if exercise {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let fixture = std::env::temp_dir().join(format!(
            "foldex-system-menu-probe-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&fixture)?;
        fs::write(
            fixture.join("sample.foldexzipprobe"),
            b"Foldex generated native compression fixture\r\n",
        )?;
        fixture
    } else {
        PathBuf::from(next)
    };
    let paths = if exercise {
        vec![folder.join("sample.foldexzipprobe")]
    } else {
        let selected: Vec<_> = args.map(PathBuf::from).collect();
        if selected.is_empty() {
            vec![folder.clone()]
        } else {
            selected
        }
    };
    let menu = BuiltinMenus::new(Arc::new(|| {}))?;
    let mut reports = Vec::new();
    let mut zip = None;
    for kind in [BuiltinKind::New, BuiltinKind::Compress] {
        let start = Instant::now();
        let (targets, background) = if kind == BuiltinKind::New {
            (vec![folder.clone()], true)
        } else {
            (paths.clone(), false)
        };
        menu.discover(kind, 0, targets.clone(), background)?;
        let snapshot = match event(&menu)? {
            BuiltinEvent::Ready {
                kind: actual,
                snapshot,
            } if actual == kind => snapshot,
            other => return Err(format!("Unexpected system definition result: {other:?}").into()),
        };
        if kind == BuiltinKind::Compress {
            zip = snapshot
                .items
                .iter()
                .find(|item| {
                    item.key == "compress:Windows.CompressTo.Zip"
                        || item.key == "compress:legacy-zip"
                })
                .map(|item| item.key.clone());
        }
        let loaded_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let cached = menu
            .discover(kind, 0, targets, background)?
            .ok_or("Definitions were not cached")?;
        let cache_ms = start.elapsed().as_secs_f64() * 1000.0;
        if !Arc::ptr_eq(&snapshot, &cached) {
            return Err("Cache replaced its immutable definition snapshot".into());
        }
        reports.push(json!({
            "kind": format!("{kind:?}"), "load_ms": loaded_ms, "cache_ms": cache_ms,
            "truncated": snapshot.truncated,
            "items": snapshot.items.iter().map(|item| json!({"key": item.key, "label": item.label})).collect::<Vec<Value>>(),
        }));
    }
    let mut verification = Value::Null;
    if exercise {
        invoke(
            &menu,
            10,
            BuiltinKind::New,
            vec![folder.clone()],
            true,
            "new:folder".into(),
        )?;
        let folders = fs::read_dir(&folder)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count();
        if folders != 1 {
            return Err(
                "Native Folder command did not create exactly one folder in the generated fixture"
                    .into(),
            );
        }
        let zip = zip.ok_or("Windows did not expose a ZIP compression command")?;
        invoke(&menu, 11, BuiltinKind::Compress, paths, false, zip)?;
        // Native dispatch acceptance is not proof of async file completion.
        // Wait for the one expected archive in our own unassociated fixture;
        // never retry/activate another command based on filesystem timing.
        let archive = folder.join("sample.zip");
        let deadline = Instant::now() + Duration::from_secs(10);
        let bytes = loop {
            if let Ok(bytes) = fs::read(&archive) {
                if bytes.starts_with(b"PK\x03\x04") {
                    break bytes;
                }
            }
            if Instant::now() >= deadline {
                return Err("Native ZIP command did not create the expected sample.zip archive in the generated fixture".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        verification =
            json!({"native_new_folder": true, "native_zip": archive, "zip_bytes": bytes.len()});
    }
    menu.shutdown();
    let start = Instant::now();
    while !menu.is_worker_finished() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    fs::write(
        output,
        serde_json::to_vec_pretty(
            &json!({"folder": folder, "exercise": exercise, "worker_finished": menu.is_worker_finished(), "reports": reports, "verification": verification}),
        )?,
    )?;
    // Keep generated outputs for the parent's independent archive inspection.
    if !menu.is_worker_finished() {
        return Err("System menu worker did not retire".into());
    }
    Ok(())
}
