# Foldex website

The landing page is a static Astro project. It has no server adapter, client router, remote fonts, or runtime API calls. React is used only for the benchmark chart, as an island that loads when the section scrolls into view.

The website's original code is licensed under AGPL-3.0-only, like Foldex. The
public `LICENSE.txt` mirrors the repository's `LICENSE`; bundled fonts retain
their own notices in `public/fonts/OFL.txt`.

## Develop

Use Node.js 24 LTS (or a version satisfying `package.json`) and pnpm (`corepack enable` picks the pinned version), then:

```sh
cd web
pnpm install
pnpm dev
```

The local URL is printed by Astro. To check the production output:

```sh
pnpm check
pnpm format:check
pnpm build
pnpm preview
```

## Edit the pages

The site has three pages, linked from the header and footer (`navigation` in `src/config/site.ts`):

- `src/pages/index.astro`, the home page, described below.
- `src/pages/changelog.astro`: every release on a timeline, newest first, filterable by New, Improved and Fixed. Entries are written in plain words in `src/data/changelog.ts`; add one with each release (the full notes stay in `docs/releases/`).
- `src/pages/docs.astro`: plain-language documentation for people using Foldex, with a sidebar that filters and highlights as you type (press `/`).

The header logo, footer and favicon use the app icon from `assets/brand/` (copied to `src/assets/foldex.png` and `public/favicon.ico`). The footer ends with a giant wordmark that a few files peek out from behind.

`src/pages/index.astro` composes the home page's sections. Reorder, replace, or remove their imports and markup there:

- `Hero.astro`: the headline, download button, and the command-palette loop on a colour plate. Behind it, `HeroField.astro` drifts files sideways on five depth layers: far ones small, slow and blurred, near ones large and sharp, and a few out-of-focus close-ups sweeping past in front. The pointer never moves the files; near ones can be clicked, ctrl-clicked, and rubber-banded together with the headline; now and then a ghost cursor picks a few. The files themselves (coloured folders with emblems, documents drawn by type, apps, and pictures and clips with little gradient scenes) are listed in `src/data/heroFiles.ts` and dealt out without repeats.
- `Benchmarks.astro`: a race between Foldex and File Explorer for each test, in `BenchmarkRace.tsx`. Stopwatches and multipliers use [Rolling Number](https://rolling.kitlangton.dev/), and an [EvilCharts](https://evilcharts.com/) area chart draws each app's progress live. It moves through the tests on its own until the visitor picks one. The chart is drawn once per test and revealed by a sliding curtain, so the race runs on the compositor. Figures and each chart's curve type live in `src/data/benchmarks.ts`.
- `Features.astro`: the search and navigation loops, and `ShortcutBoard.astro`, a keyboard that names Foldex's default shortcuts (`src/data/shortcuts.ts`) as the visitor presses them.
- `Launcher.astro`: a working launcher over a small desk of windows. Ctrl+Alt+Space (or clicking the keys) opens it; it searches the entries in `src/data/launcher.ts`, switches windows, and answers sums and unit conversions with `src/data/calculator.ts`, a port of the app's calculator. When idle on screen it runs a short demo, which stops the moment the visitor does anything.
- `Download.astro`: every download as a row, grouped by system. Windows builds link to the release; macOS, Linux and Windows on Arm are listed as coming soon. The visitor's own system moves to the top. The hero's split button (`DownloadSplit.astro`) offers the same list, detecting the system with `src/lib/platform.ts`; platforms live in `src/data/platforms.ts`.

Keycaps come from `ui/Keys.astro`; every cap on the page goes down while the visitor holds that key. Labels map to key names in `src/data/keys.ts`.

### Loop videos

`LoopVideo.astro` plays the clips in `public/media/` (WebM, then MP4) with posters from `src/assets/media/`. Clips play only while on screen, have a pause button, and stay paused under reduced motion. The keycast overlay replays the key presses recorded with each clip, listed in `src/data/loops.ts` in seconds from the start of the encoded video.

The clips are recordings of the actual app: a portable copy pointed at a fictional demo folder, captured window-only, so no personal files appear. Each is crossfaded into its own start so it loops seamlessly. When recording new ones, keep the demo folder fictional and regenerate the matching key timings.

### Colour backgrounds

`Aura.astro` is a small WebGL shader: domain-warped noise folded into soft, slowly drifting shapes, drawn at low resolution and covered with film grain (`public/grain.png`, blended at `--grain`). Its four-colour palettes (base, shadow, mid, light) live in `src/data/auras.ts`; keep them neutral and low in saturation, and avoid pairing blue with yellow in one palette, since they mix to olive. `Plate.astro` wraps an aura with rounded corners behind a product shot. Each aura starts its WebGL context only when it nears the screen, pauses offscreen, and draws a single still frame under reduced motion; without WebGL it falls back to a CSS gradient of the same colours.

The release version and asset sizes shown on the page are set in `src/config/site.ts`; update them with each release.

Edit colors, typography, spacing, and the content width in `src/styles/global.css`. `src/layouts/PageLayout.astro` owns semantic HTML, font preloads, canonical metadata, social cards, and SoftwareApplication structured data. Add future routes in `src/pages/` and reuse that layout.

Content, navigation, and downloads work without JavaScript; scripts only add motion and interaction. The benchmark island is rendered on the server too, so its figures read without JavaScript. Fonts (Geist and Geist Mono) are bundled locally.

### Charts

The EvilCharts components in `src/components/evilcharts/` are copied from its registry (it is distributed as shadcn copy-in code, on Recharts and Motion), with imports pointed at this project and one non-null assertion added for strict type checking. They are excluded from Prettier to stay close to upstream. They are styled with Tailwind CSS through `src/styles/charts.css`, which scans only that folder, leaves out Preflight, and places Tailwind in CSS layers so the page's own styles always win; shadcn's colour names map onto the page's tokens there.

## Public URLs

Repository and download links default to `https://github.com/pfaizar/foldex` and
its Releases page. The Releases page includes preview releases as well as stable
versions. The production website domain is configured separately.

Copy `.env.example` to `.env` to optionally override the public repository and
portable-release URLs:

```dotenv
PUBLIC_REPOSITORY_URL=https://github.com/pfaizar/foldex
PUBLIC_DOWNLOAD_URL=https://github.com/pfaizar/foldex/releases
```

Set **`SITE_URL` in the build environment** to the final HTTPS production origin. Do not include a path. This enables the correct canonical URLs, absolute Open Graph image URLs, sitemap, and indexable robots.txt. Without it, builds are intentionally marked `noindex` and robots.txt disallows crawling. `SITE_URL` is read by `astro.config.mjs` from the process environment, not from `.env`.

PowerShell example:

```powershell
$env:SITE_URL = 'https://your-domain.example'
pnpm build
```

## Cloudflare Pages

Connect the repository in Cloudflare Pages with:

| Setting                    | Value                                           |
| -------------------------- | ----------------------------------------------- |
| Root directory             | `web`                                           |
| Build command              | `pnpm build`                                    |
| Output directory           | `dist`                                          |
| Node version               | `24` (set `NODE_VERSION` if needed)             |
| Build environment variable | `SITE_URL` = your final HTTPS production origin |

Set the optional `PUBLIC_REPOSITORY_URL` and `PUBLIC_DOWNLOAD_URL` build variables when available. The public repository should link to the project root; its README contains the installation guide.

Production branch detection defaults to `main`; if yours differs, set `PRODUCTION_BRANCH` to match. Cloudflare preview branches receive `noindex` metadata and a disallowing robots.txt while retaining production canonical URLs.

Deploy the generated `dist/` as ordinary static files. No Cloudflare adapter, Worker, Pages Function, or Wrangler configuration is needed. The top-level `404.html` provides real not-found behavior rather than a single-page application fallback. `public/_headers` supplies security headers and immutable caching for Astro’s fingerprinted assets.

## Design references

The visual direction follows the light canvas, compact navigation, regular-weight typography, rounded buttons, and generous whitespace of [Devin](https://devin.ai/), adapted to Foldex’s existing brand and features. The page uses one workspace preview, short product descriptions, and no decorative badges. The repeated demo cards and dark presentation from the first version have been removed.

Implementation references:

- [Astro project structure](https://docs.astro.build/en/basics/project-structure/)
- [Astro scripts and event handling](https://docs.astro.build/en/guides/client-side-scripts/)
- [Astro sitemap integration](https://docs.astro.build/en/guides/integrations-guide/sitemap/)
- [Cloudflare Pages: Astro](https://developers.cloudflare.com/pages/framework-guides/deploy-an-astro-site/)
- [web.dev: responsive typography](https://web.dev/learn/design/typography/)

## Visual review

Screenshots and review notes from the redesign are saved in the ignored `artifacts/devin-redesign/` directory. Earlier screenshots remain in `artifacts/` for comparison. Check desktop and narrow mobile layouts after changing sections. Preserve keyboard focus, native disclosure behavior, the skip link, text contrast, and reduced-motion support.
