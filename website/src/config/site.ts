// Single source of truth for site-wide facts. Keep every claim here backed by the
// repository (README.md, docs/, Cargo.toml) so the site never outruns the product.

export const SITE = {
  name: 'Regressify',
  title: 'Regressify: visual regression testing for web pages',
  description:
    'Visual regression testing for web pages, from the command line or a native desktop app. It drives Chromium with no Node or Playwright to install.',
  /**
   * Public source repository. Verified reachable on 2026-10-22; set to `null` to
   * hide every repository link if the project moves or becomes private. Content
   * Markdown must not hard-code repository URLs, so this stays the only switch.
   */
  repoUrl: 'https://github.com/precise-alloy/regressify-rs' as string | null,
  // TODO(maintainer): the repository has no LICENSE file yet. Add one before the
  // site describes Regressify as open source, then set its SPDX id here.
  license: null as string | null,
} as const;

/** Fallback for every download button when no verified release is available. */
export const DOWNLOAD_PAGE_PATH = '/download/';

export const NAV_LINKS = [
  { href: '/docs/', label: 'Docs' },
  // { href: '/blog/', label: 'Blog' },
  { href: '/download/', label: 'Download' },
] as const;

export const HEADER_LINKS = [
  ...NAV_LINKS,
  { href: '/admin', label: 'Login' },
] as const;

export const THEME_STORAGE_KEY = 'regressify-theme';
