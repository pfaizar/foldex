//! Read-only menu integration probe. Never invokes any enumerated command.
use foldex_shell::{Event, ShellItem, ShellMenu};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

fn tree(items: &[ShellItem]) -> Value {
    items
        .iter()
        .map(|item| {
            json!({
                "id": item.id, "label": item.label, "verb": item.canonical_verb,
                "enabled": item.enabled, "submenu": item.has_submenu,
                "owner_draw": item.owner_draw, "children": tree(&item.children),
            })
        })
        .collect()
}

fn submenus(items: &[ShellItem], result: &mut VecDeque<u32>) {
    for item in items {
        if item.has_submenu && item.enabled {
            result.push_back(item.id);
        }
        submenus(&item.children, result);
    }
}

fn receive(menu: &ShellMenu) -> Result<Event, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = menu.try_recv() {
            return Ok(event);
        }
        if Instant::now() >= deadline {
            return Err("Native menu exceeded 30 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let output = PathBuf::from(args.next().ok_or("output JSON path required")?);
    let paths: Vec<_> = args.map(PathBuf::from).collect();
    if paths.is_empty() {
        return Err("at least one absolute fixture path required".into());
    }
    let menu = ShellMenu::new(Arc::new(|| {}))?;
    let mut reports = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let request = index as u64 + 1;
        let start = Instant::now();
        menu.open(request, 0, vec![path.clone()], false)?;
        let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
        let Event::Ready {
            mut items,
            truncated,
            ..
        } = receive(&menu)?
        else {
            return Err("Native menu failed to enumerate".into());
        };
        let ready_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut pending = VecDeque::new();
        let mut expanded = std::collections::HashSet::new();
        submenus(&items, &mut pending);
        while let Some(id) = pending.pop_front() {
            if expanded.len() == 16 || !expanded.insert(id) {
                continue;
            }
            menu.expand(request, id)?;
            match receive(&menu)? {
                Event::Updated { items: new, .. } => {
                    items = new;
                    submenus(&items, &mut pending);
                }
                event => return Err(format!("Native expansion failed: {event:?}").into()),
            }
        }
        reports.push(
            json!({"path": path, "submit_ms": submit_ms, "ready_ms": ready_ms,
            "all_expansions_ms": start.elapsed().as_secs_f64() * 1000.0,
            "expanded": expanded.len(), "truncated": truncated, "items": tree(&items)}),
        );
        menu.close(request)?;
    }
    menu.shutdown();
    let start = Instant::now();
    while !menu.is_worker_finished() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({
            "commands_invoked": 0, "worker_finished": menu.is_worker_finished(), "reports": reports,
        }))?,
    )?;
    if !menu.is_worker_finished() {
        return Err("Native menu worker did not retire".into());
    }
    Ok(())
}
