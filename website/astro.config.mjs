// @ts-check
import sitemap from '@astrojs/sitemap';
import { defineConfig } from 'astro/config';

// TODO(maintainer): replace with the production origin before deploying. It is
// used for canonical URLs, Open Graph URLs, the sitemap, and the RSS feed.
// Override per build with SITE_URL, e.g. for preview deployments.
const SITE_URL = process.env.SITE_URL || 'https://regressify.example.com';

// https://astro.build/config
export default defineConfig({
  site: SITE_URL,
  trailingSlash: 'ignore',
  integrations: [sitemap()],
  markdown: {
    // Code stays monochrome to match the desktop's neutral palette and keep
    // contrast predictable in both themes.
    syntaxHighlight: false,
  },
});
