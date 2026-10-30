import { describe, expect, test } from 'bun:test';
import { DESKTOP_LEGACY_ANCHORS, resolveLegacyHash } from './docs-redirects';

describe('resolveLegacyHash', () => {
  test('maps old section anchors to the new section page', () => {
    expect(resolveLegacyHash('#install')).toBe('/docs/desktop-app/install/');
    expect(resolveLegacyHash('#git')).toBe('/docs/desktop-app/git/');
    expect(resolveLegacyHash('#troubleshooting')).toBe('/docs/desktop-app/troubleshooting/');
  });

  test('maps old subsection anchors to the new page and anchor', () => {
    expect(resolveLegacyHash('#run-settings')).toBe('/docs/desktop-app/running-tests/#run-settings');
    expect(resolveLegacyHash('#overview')).toBe('/docs/desktop-app/workspaces/#overview');
    expect(resolveLegacyHash('#clean-up-old-test-runs')).toBe(
      '/docs/desktop-app/reviewing-results/#clean-up-old-test-runs',
    );
  });

  test('accepts a hash without the leading #', () => {
    expect(resolveLegacyHash('updates')).toBe('/docs/desktop-app/install/#updates');
  });

  test('does not redirect an unknown anchor', () => {
    expect(resolveLegacyHash('#in-this-guide')).toBeNull();
    expect(resolveLegacyHash('#not-a-section')).toBeNull();
    // Ids are case-sensitive, as in the DOM.
    expect(resolveLegacyHash('#Run-Settings')).toBeNull();
  });

  test('does not redirect an empty hash', () => {
    expect(resolveLegacyHash('')).toBeNull();
    expect(resolveLegacyHash('#')).toBeNull();
  });

  test('decodes percent-encoded hashes', () => {
    expect(resolveLegacyHash('#run%2Dsettings')).toBe('/docs/desktop-app/running-tests/#run-settings');
    expect(resolveLegacyHash('#%67%69%74')).toBe('/docs/desktop-app/git/');
  });

  test('ignores malformed percent-encoding instead of throwing', () => {
    expect(resolveLegacyHash('#%E2')).toBeNull();
    expect(resolveLegacyHash('#run%ZZsettings')).toBeNull();
  });

  test('ignores inherited object keys', () => {
    for (const key of ['constructor', '__proto__', 'toString', 'hasOwnProperty']) {
      expect(resolveLegacyHash(`#${key}`)).toBeNull();
    }
  });

  test('stays on the page when the id exists there', () => {
    expect(resolveLegacyHash('#install', (id) => id === 'install')).toBeNull();
    expect(resolveLegacyHash('#install', (id) => id === 'other')).toBe('/docs/desktop-app/install/');
  });

  test('passes the decoded id to the element check', () => {
    const seen: string[] = [];
    resolveLegacyHash('#run%2Dsettings', (id) => {
      seen.push(id);
      return false;
    });
    expect(seen).toEqual(['run-settings']);
  });
});

describe('DESKTOP_LEGACY_ANCHORS', () => {
  test('covers every heading of the old single-page guide', () => {
    expect(Object.keys(DESKTOP_LEGACY_ANCHORS)).toHaveLength(34);
  });

  test('only targets same-site desktop pages', () => {
    for (const target of Object.values(DESKTOP_LEGACY_ANCHORS)) {
      expect(target).toMatch(/^\/docs\/desktop-app\/[a-z-]+\/(#[a-z-]+)?$/);
    }
  });

  test('never targets the Overview page itself', () => {
    for (const target of Object.values(DESKTOP_LEGACY_ANCHORS)) {
      expect(target.startsWith('/docs/desktop-app/#')).toBe(false);
    }
  });

  test('cannot be modified at runtime', () => {
    expect(Object.isFrozen(DESKTOP_LEGACY_ANCHORS)).toBe(true);
  });
});
