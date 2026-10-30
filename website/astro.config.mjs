// @ts-check
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';
import { defineConfig } from 'astro/config';
import { SITE } from './src/config/site.ts';

// Production origin (the custom domain in wrangler.jsonc). It is used for
// canonical URLs, Open Graph URLs, the sitemap, and the RSS feed. Override per
// build with SITE_URL, e.g. for preview deployments.
const SITE_URL = process.env.SITE_URL || 'https://regressify.eshn.dev';

// https://astro.build/config
export default defineConfig({
  site: SITE_URL,
  trailingSlash: 'ignore',
  // Starlight would otherwise turn on hover prefetch for every link on every
  // page, including /admin/ (a Worker route). Keep the site's previous behavior.
  prefetch: false,
  integrations: [
    // Docs at /docs/** use Starlight for the Astro docs layout: sidebar groups,
    // one page per section, "On this page", prev/next, and Pagefind search.
    // Pages live in src/content/docs/docs/ (see src/content.config.ts).
    // Note: Starlight sets `scopedStyleStrategy: 'where'` for the whole site, so
    // component styles carry no extra specificity from their scoping attribute.
    starlight({
      title: SITE.name,
      description: SITE.description,
      // Logo and social links feed Starlight's stock SiteTitle and SocialIcons. The
      // Header override renders the site's Brand and GitHub button instead, but
      // these keep the stock components correct if an override is removed.
      logo: { src: './public/icon.svg', alt: '' },
      social: SITE.repoUrl ? [{ icon: 'github', label: 'GitHub', href: SITE.repoUrl }] : [],
      editLink: SITE.repoUrl ? { baseUrl: `${SITE.repoUrl}/edit/master/website/` } : {},
      sidebar: [
        { label: 'Start here', items: [{ slug: 'docs', label: 'Introduction' }, 'docs/getting-started'] },
        { label: 'Desktop app', items: [{ autogenerate: { directory: 'docs/desktop-app' } }] },
      ],
      tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
      // Dates come from `lastUpdated` front matter only, not Git history.
      lastUpdated: false,
      pagination: true,
      // The site has its own 404 page (src/pages/404.astro), which the Worker's
      // `not_found_handling: "404-page"` serves.
      disable404Route: true,
      // Code stays monochrome to match the desktop's neutral palette and keep
      // contrast predictable in both themes.
      expressiveCode: false,
      customCss: ['./src/styles/tokens.css', './src/styles/starlight.css'],
      components: {
        Head: './src/components/starlight/Head.astro',
        Header: './src/components/starlight/Header.astro',
        MarkdownContent: './src/components/starlight/MarkdownContent.astro',
        MobileMenuFooter: './src/components/starlight/MobileMenuFooter.astro',
        ThemeProvider: './src/components/starlight/ThemeProvider.astro',
        ThemeSelect: './src/components/starlight/ThemeSelect.astro',
      },
      head: [
        { tag: 'meta', attrs: { property: 'og:image', content: new URL('/icon.png', SITE_URL).href } },
        // The image is the square app icon, not a wide card (same as BaseLayout).
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary' } },
        { tag: 'meta', attrs: { name: 'theme-color', media: '(prefers-color-scheme: light)', content: '#f8f8f8' } },
        { tag: 'meta', attrs: { name: 'theme-color', media: '(prefers-color-scheme: dark)', content: '#171717' } },
        { tag: 'link', attrs: { rel: 'icon', href: '/favicon.ico', sizes: 'any' } },
        { tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/icon.png' } },
        { tag: 'link', attrs: { rel: 'alternate', type: 'application/rss+xml', title: `${SITE.name} blog`, href: '/rss.xml' } },
      ],
    }),
    sitemap({ filter: (page) => !/^\/admin(?:\/|$)/.test(new URL(page).pathname) }),
  ],
  markdown: {
    // Code stays monochrome to match the desktop's neutral palette and keep
    // contrast predictable in both themes.
    syntaxHighlight: false,
  },
});
