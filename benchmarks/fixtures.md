# Fixtures

A deterministic test corpus. `fxbench fixtures generate --seed 20261028` must
produce byte-identical names, sizes and timestamps on any machine. Content may
differ only where noted (sparse files).

Root: `D:\FoldexBench\` (configurable). Each set lives in its own folder and
has a `manifest.json` beside it (below), never inside it, so the manifest never
shows up in a listing.

## Name and metadata rules

- **Names:** drawn from a weighted mix of realistic patterns:
  `IMG_2041.jpg`, `invoice-2023-04-17.pdf`, `Quarterly report (final) v3.docx`,
  `main.rs`, `node_modules`-style package names, and hash-like
  `a3f9c1…`. Lengths 4–80 characters, about 5% with spaces, about 3% non-ASCII.
- **Extension mix** (by count): images 25%, documents 20%, source code 20%,
  archives 5%, media 5%, data and JSON 10%, no extension 5%, other 10%. It
  matters because icon lookup is per type.
- **Sizes:** log-normal, 0 B to 4 GB. Files ≥ 1 MB are created **sparse**
  (`FSCTL_SET_SPARSE` + `SetEndOfFile`), so they report a real size without
  using disk space. Files < 1 MB get real content.
- **Timestamps:** created, modified and accessed spread over 2015–2026 with
  `SetFileTime`, so sorting by date does real work.
- **Attributes:** about 1% hidden, 0.2% read-only, no system files.

## Sets

| ID | Shape | Items | Used by |
| :--- | :--- | ---: | :--- |
| `F-100` | Flat folder | 100 | S1 target, S4 idle |
| `F-1k` | Flat | 1,000 | S2 |
| `F-10k` | Flat | 10,000 | S2, S5 filter, pilot |
| `F-100k` | Flat | 100,000 | S2, S4, S5 filter |
| `F-1M` | Flat | 1,000,000 | S2, S3 |
| `D-50k` | Flat, folders only | 50,000 folders | S2, S3 |
| `T-250k` | Realistic tree: a projects/documents/photos mix plus a `node_modules`-like subtree, depth ≤ 14 | 250,000 total | S5 search |
| `I-5k` | Flat, real JPEG/PNG images, 0.5–12 MP, generated (gradients and noise, varied aspect ratios) | 5,000 | S3 thumbnails |
| `P-long` | Nested folders reaching full paths of 300, 1,000 and 4,000 characters | 3 leaf folders × 100 items | S3 |
| `U-names` | Emoji, CJK, RTL, combining marks, 255-character names, names that differ only in Unicode normalization | 2,000 | S3 |
| `C-churn` | Starts with 10,000 files; a writer process adds, renames and deletes 1,000 files per second while it is viewed | 10,000 ± churn | S3 |
| `T-250k-indexed` | Copy of `T-250k` under `%USERPROFILE%\FoldexBench` (in the Windows Search index by default) | 250,000 | S5, Explorer indexed variant |

`C-churn`'s writer is part of the harness (`fxbench churn D:\FoldexBench\C-churn --rate 1000`)
and logs every change it makes, so the expected count at any moment is known.

## Manifest

```json
{
  "schema": 1,
  "set": "T-250k",
  "seed": 20261028,
  "root": "D:\\FoldexBench\\T-250k",
  "items": 250000,
  "files": 231877,
  "folders": 18123,
  "hidden": 2511,
  "direct_children": 14,
  "queries": {
    "invoice": { "matches": 412, "match": "name-contains, case-insensitive" },
    "*.pdf": { "matches": 18530, "match": "extension" },
    "zq7": { "matches": 5, "match": "name-contains, rare" },
    "e": { "matches": 171204, "match": "name-contains, very common" }
  },
  "filter_prefixes": {
    "F-100k": { "r": 41022, "re": 9011, "rep": 812, "repo": 640, "repor": 455, "report": 455 }
  }
}
```

The numbers above are examples. The generator writes the real ones. Each count
is written once for each matching rule an app uses by default
(`name-contains`, `name-glob`, `fuzzy-<algorithm>`). The manifest stores
`{"name-contains": 412, "fuzzy-frizbee": 530, …}` per query; see
[S5](scenarios/S5-search-filter.md#matching-rules).

**The manifest counts what a default-settings app should show.** Hidden files
are hidden by default in Explorer. Check whether Foldex shows them by default
in the pilot. If an app's default differs, its
expected count is adjusted and the difference is noted in the app file. Hidden
files are **not** switched on or off to make the apps match.

## Preparation rules

- Generate once per results series, then reboot (so creation does not leave the
  files in the cache).
- `D:\FoldexBench` is not in the Windows Search index. The generator checks
  this through `ISearchCrawlScopeManager` and refuses to continue if it is.
- For `T-250k-indexed`, wait until indexing finishes:
  `ISearchCatalogManager::NumberOfItemsToIndex()` reports 0 for 10 minutes.
- Thumbnail caches start empty for the thumbnail runs:
  - Windows: delete `%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db`
    with Explorer stopped. Explorer and Foldex both use this cache through the
    shell.
  - Apps added later that keep their own thumbnail cache list its location in
    their app file, and it is cleared too.
