// The desktop guide used to be one long page at /docs/desktop-app/. Its sections
// are now separate pages, so old deep links such as /docs/desktop-app/#run-settings
// must reach the page that holds that section. Hashes never reach the server, so
// the redirect runs in the browser (see components/starlight/LegacyHashRedirect.astro).

/** Route id (Starlight slug) of the page that runs the legacy hash redirect. */
export const LEGACY_HASH_PAGE_ID = 'docs/desktop-app';

/**
 * Old heading id on the single-page guide -> new page URL (and anchor).
 * Keep it in step when a desktop page or heading is renamed. No heading on the
 * Overview page may use one of these ids, or the old link would be redirected
 * away from content that still exists there.
 */
export const DESKTOP_LEGACY_ANCHORS: Readonly<Record<string, string>> = Object.freeze({
  install: '/docs/desktop-app/install/',
  updates: '/docs/desktop-app/install/#updates',
  'open-a-workspace': '/docs/desktop-app/workspaces/',
  navigator: '/docs/desktop-app/workspaces/#navigator',
  overview: '/docs/desktop-app/workspaces/#overview',
  'edit-suites': '/docs/desktop-app/editing-suites/',
  'create-a-suite': '/docs/desktop-app/editing-suites/#create-a-suite',
  'rename-or-delete-a-suite': '/docs/desktop-app/editing-suites/#rename-or-delete-a-suite',
  'source-fields-and-effective': '/docs/desktop-app/editing-suites/#source-fields-and-effective',
  'fields-form': '/docs/desktop-app/editing-suites/#fields-form',
  'add-actions': '/docs/desktop-app/editing-suites/#add-actions',
  'save-and-undo': '/docs/desktop-app/editing-suites/#save-and-undo',
  'run-tests': '/docs/desktop-app/running-tests/',
  'first-run': '/docs/desktop-app/running-tests/#first-run',
  'run-settings': '/docs/desktop-app/running-tests/#run-settings',
  'watch-progress': '/docs/desktop-app/running-tests/#watch-progress',
  logs: '/docs/desktop-app/running-tests/#logs',
  'review-results': '/docs/desktop-app/reviewing-results/',
  'capture-details': '/docs/desktop-app/reviewing-results/#capture-details',
  'approve-a-change': '/docs/desktop-app/reviewing-results/#approve-a-change',
  'clean-up-old-test-runs': '/docs/desktop-app/reviewing-results/#clean-up-old-test-runs',
  'simplify-images': '/docs/desktop-app/simplify-images/',
  'turn-it-on': '/docs/desktop-app/simplify-images/#turn-it-on',
  'what-changes': '/docs/desktop-app/simplify-images/#what-changes',
  'after-turning-it-on': '/docs/desktop-app/simplify-images/#after-turning-it-on',
  git: '/docs/desktop-app/git/',
  'publish-reports': '/docs/desktop-app/publishing-reports/',
  'deployment-tokens': '/docs/desktop-app/publishing-reports/#deployment-tokens',
  'landing-apps': '/docs/desktop-app/publishing-reports/#landing-apps',
  'appearance-and-commands': '/docs/desktop-app/appearance-and-commands/',
  'change-the-theme': '/docs/desktop-app/appearance-and-commands/#change-the-theme',
  'search-commands': '/docs/desktop-app/appearance-and-commands/#search-commands',
  'keyboard-shortcuts': '/docs/desktop-app/keyboard-shortcuts/',
  troubleshooting: '/docs/desktop-app/troubleshooting/',
});

/**
 * Returns the new URL for an old `location.hash` on the desktop Overview page,
 * or null when the page should stay put: empty hash, unknown or malformed id,
 * or an id that exists on the current page (`hasElement`).
 */
export function resolveLegacyHash(
  hash: string,
  hasElement: (id: string) => boolean = () => false,
  map: Readonly<Record<string, string>> = DESKTOP_LEGACY_ANCHORS,
): string | null {
  const raw = hash.startsWith('#') ? hash.slice(1) : hash;
  if (!raw) return null;
  let id: string;
  try {
    id = decodeURIComponent(raw);
  } catch {
    // Malformed percent-encoding (for example a truncated "%E2"): not an old link.
    return null;
  }
  // Own keys only, so ids such as "constructor" or "__proto__" never resolve.
  if (!Object.hasOwn(map, id)) return null;
  if (hasElement(id)) return null;
  return map[id] ?? null;
}
