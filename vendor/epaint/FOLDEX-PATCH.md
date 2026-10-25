# Foldex patch on epaint 0.36.2

Base: pinned crates.io source, upstream commit `49682f8baa058bf49e011035cfbd6e825f88a5ef`, `crates/epaint`. Original MIT and Apache licenses are retained.

`src/text/fonts.rs` adds an `atlas_revision` to `FontsImpl` and an inline `FontsView::atlas_revision()` accessor. A relaxed atomic assigns a new identifier only when an atlas is created. Reusing the atlas, appending glyphs and increasing the image height retain that identifier, since those operations preserve the glyph coordinates. Replacing font definitions, changing text options or recycling a full atlas creates a new identifier.

Foldex's retained text cache keys previously counted font definitions. That missed atlas recreation with the same definitions, leaving file names, breadcrumb labels and other retained text pointing at unrelated or blank pixels. Every retained text cache already uses the common revision accessor; the corrected accessor is an O(1) integer read, with no hashing, allocation or texture copying. There is one extra machine word per FontsImpl and one process-wide counter.

The focused regression first failed against the original Foldex accessor with a retained filename pointing into a replaced atlas (`text-atlas-before.log`). The companion workload exercises the actual 80% atlas recycling threshold and checks that ordinary glyph appends do not invalidate the key.
