import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

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
  integrations: site
    ? [sitemap({ filter: (page) => !page.endsWith('/404/') })]
    : [],
  devToolbar: { enabled: false },
});
