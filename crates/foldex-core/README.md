# foldex-core

Native filesystem models, natural sorting, Unicode lowercase substring queries, and a bounded Tokio background engine. No UI toolkit, network service, or shell extension is required.

```rust,no_run
use foldex_core::{Engine, Event, Query};
use std::{path::PathBuf, sync::Arc};

let engine = Engine::new(Arc::new(|| { /* wake the window */ }))?;
let mut browser = engine.browser();
browser.navigate(PathBuf::from(r"D:\Workspace"), false);

// Run while draining the window's bounded per-frame event allowance.
if let Some(event) = browser.try_recv() {
    match event {
        Event::Directory { snapshot, done, .. } => {
            // `done` means enumeration finished; `complete` also requires no skipped/capped data.
            let _finished = done;
            browser.filter(snapshot, Query::default());
        }
        Event::View { snapshot, indices, .. } => {
            // Paint only visible indices. Filesystem actions use the exact original path.
            if let Some(&index) = indices.first() {
                let _exact_path = snapshot.resolve(index);
            }
        }
        _ => {}
    }
}
# Ok::<(), std::io::Error>(())
```

`EntryStore` provides `len`, `is_empty`, `get`, `iter`, and indexed access. Progressive snapshots share immutable 256-entry chunks. `Entry.name` is display text; use `Entry::path` or `Snapshot::resolve` to preserve non-Unicode names. Filtering uses Unicode lowercase keys, not locale collation or full Unicode case folding. Folders remain first when descending.

Navigation, filtering, and recursive-scope intents are latest-only. Every result has a generation, and the browser drops superseded results. UI methods never enumerate or wait for workers. Directory snapshots arrive as cached/progressive/final events; `done` is independent from `Snapshot.complete`. Scoped recursive scans publish `ScanProgress` with the same distinction through `done` and snapshot coverage.

Defaults: 64 MiB shared model reservations; 16 MiB/eight-directory LRU; recursive traversal stops at 250k entries or 32 MiB of conservative model/index/workspace reservations. Pinned snapshot clones remain charged. Limits are explicit through `Snapshot.limit`; capped results are incomplete. Reservations include index headroom for an eight-tab shell and are not a process/GPU memory measurement. Unbounded retention of public indices by an outside consumer is not supported.

The engine never writes, copies, renames, or deletes user files. Recursive scans do not follow symlinks/reparse points or read file contents. Active-folder freshness currently uses directory modified-time polling every two seconds. It can miss in-place/metadata-only changes; explicit refresh is supported. `set_active(false)` stops hidden-tab polling, and a recursive scope suppresses ordinary polling until navigation/cancellation. A native notification watcher and disk-backed search beyond the cap remain later stages.

`Engine::retire` and `Browser::retire` move snapshot destruction to a bounded retirement worker. These nonblocking methods return the snapshot on saturation; keep it for a later retry. Keep the engine alive while its browsers are used. `Engine::managed_bytes()` reports conservative reserved model bytes.

```powershell
cargo +1.95.0 test -p foldex-core
cargo +1.95.0 run --release -p foldex-core --bin foldex-bench -- --fixture-entries 1024 --filter-entries 100000
```

The benchmark prints JSON and creates disposable fixtures only under this crate's `target/bench-fixtures/`. Creation is outside timed regions. It labels the actual filesystem cache state as recently created/warm. `--keep-fixture` retains the generated fixture; `--repeats` controls filter samples. Tests also use owned fixture folders and verify canonical containment before cleanup.
