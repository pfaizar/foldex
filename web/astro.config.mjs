import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import sitemap from '@astrojs/sitemap';
import tailwindcss from '@tailwindcss/vite';

// Supply the real production origin in Cloudflare Pages' build environment.
// A local build never invents a public domain or publishes localhost canonicals.
const site = process.env.SITE_URL;

if (
  site &&
  (new URL(site).protocol !== 'https:' || new URL(site).pathname !== '/')
) {
  throw new Error('SITE_URL must be an HTTPS origin, without a path.');
}

export default defineConfig({
  site,
  output: 'static',
  server: { port: 5173 },
  // Rasterize the checked-in social-card SVG for Open Graph image support.
  image: { dangerouslyProcessSVG: true },
  trailingSlash: 'always',
  // React only renders the benchmark chart, as an island.
  integrations: [
    react(),
    ...(site ? [sitemap({ filter: (page) => !page.endsWith('/404/') })] : []),
  ],
  vite: { plugins: [tailwindcss()] },
  devToolbar: { enabled: false },
});
