import type { APIRoute } from 'astro';
import { canIndex } from '../config/seo';

// Prerendered to a plain text file by Astro; this is not a server route.
export const GET: APIRoute = ({ site }) => {
  const body = canIndex(site)
    ? `User-agent: *\nAllow: /\n\nSitemap: ${new URL('sitemap-index.xml', site).href}\n`
    : 'User-agent: *\nDisallow: /\n';

  return new Response(body, {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
