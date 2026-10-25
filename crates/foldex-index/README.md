# foldex-index

Foldex's disk-backed, paged filename index. The library supplies cancellable
scans and bounded result windows; it does not materialize an entire volume in
the UI. The application enables `catalog-experiment` for the native catalog.

This crate is part of the root Cargo workspace and is built automatically with
the main app:

```powershell
cargo build --locked --release -p foldex
```

The library's regression tests remain under `tests/` and beside its source.
Standalone benchmark and profiling executables are omitted from this workspace.
