import { describe, expect, test } from 'bun:test';
import { isActivePath } from './nav';

describe('isActivePath', () => {
  test('matches the section page with or without a trailing slash', () => {
    expect(isActivePath('/docs/', '/docs/')).toBe(true);
    expect(isActivePath('/docs', '/docs/')).toBe(true);
  });

  test('matches pages nested below the section', () => {
    expect(isActivePath('/docs/getting-started/', '/docs/')).toBe(true);
    expect(isActivePath('/docs/desktop-app/running-tests/', '/docs/')).toBe(true);
  });

  test('does not match sibling paths that share a prefix', () => {
    expect(isActivePath('/docsearch/', '/docs/')).toBe(false);
    expect(isActivePath('/download/', '/docs/')).toBe(false);
  });
});
