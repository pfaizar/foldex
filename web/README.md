# Foldex website

The landing page is a static Astro project. It has no server adapter, UI framework, client router, remote fonts, or runtime API calls.

The website's original code is licensed under AGPL-3.0-only, like Foldex. The
public `LICENSE.txt` mirrors the repository's `LICENSE`; bundled fonts retain
their own notices in `public/fonts/OFL.txt`.

## Develop

Use Node.js 24 LTS (or a version satisfying `package.json`), then:

```sh
cd web
npm ci
npm run dev
```

The local URL is printed by Astro. To check the production output:

```sh
npm run check
npm run format:check
npm run build
npm run preview
```

## Edit the page

`src/pages/index.astro` composes four independent sections. Reorder, replace, or remove their imports and markup there:

- `src/components/sections/Hero.astro`: headline, actions, workspace preview.
- `src/components/sections/Features.astro`: three short, unboxed product descriptions.
- `src/components/sections/Keyboard.astro`: keyboard shortcuts.
- `src/components/sections/OpenSource.astro`: license, download, build instructions.

Each section owns its scoped styles. The single workspace illustration lives in `src/components/product/`; shared primitives are in `src/components/ui/`. It is an illustrative HTML/CSS preview, not a screenshot of an actual user’s files. The light page uses regular-weight headings, restrained spacing, rounded buttons, and short copy based on the project README.

Edit colors, typography, spacing, and the content width in `src/styles/global.css`. Update shared metadata, navigation, and links in `src/config/site.ts`. `src/layouts/PageLayout.astro` owns semantic HTML, the font preload, canonical metadata, social cards, and SoftwareApplication structured data. Add future routes in `src/pages/` and reuse that layout.

Small scripts are colocated with the components that need them: mobile navigation, opening the source-build instructions from “Get Foldex”, and copying commands. Core content, navigation, and build instructions work without JavaScript. Fonts and the existing Foldex icon are bundled locally. Astro optimizes the logo and renders the SVG social-card source into a PNG at build time.

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
npm run build
```

## Cloudflare Pages

Connect the repository in Cloudflare Pages with:

| Setting                    | Value                                           |
| -------------------------- | ----------------------------------------------- |
| Root directory             | `web`                                           |
| Build command              | `npm run build`                                 |
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
