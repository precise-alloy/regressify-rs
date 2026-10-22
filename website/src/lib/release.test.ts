// Ports the manifest cases from src/desktop/updates.rs so the site and the desktop
// updater accept exactly the same installer links.
import { describe, expect, test } from 'bun:test';
import {
  DEFAULT_APP_SLUG,
  downloadHref,
  type FetchLike,
  fetchRelease,
  formatSize,
  isValidSlug,
  latestInstallerUrl,
  normalizeVersion,
  parseManifest,
} from './release';

const SHA = 'ab'.repeat(32);
const URL_1_10 =
  'https://drive.tuyen.blog/downloads/regressify/Regressify-Desktop-Setup-1.10.0.0-x64.exe';
const TEXT = `version=1.10.0.0\r\ninstaller=${URL_1_10}\r\nsha256=${SHA}\r\nsize=9\r\nnotes=https://example.com/notes\r\n`;

function release(text: string, slug = 'regressify') {
  const result = parseManifest(text, slug);
  if (!result.ok) throw new Error(result.reason);
  return result.release;
}

describe('normalizeVersion', () => {
  test('normalizes to four numeric parts', () => {
    expect(normalizeVersion('01.03')).toBe('1.3.0.0');
    expect(normalizeVersion('1.10')).toBe('1.10.0.0');
    expect(normalizeVersion('9999.0.0.1')).toBe('9999.0.0.1');
  });

  test.each(['', 'v1.2.3', '1.2.3-beta', '1..2', '-1', '1.10000', '1.2.3.4.5', ' 1', '1.', '１', `1.${'0'.repeat(31)}`])(
    'rejects %p',
    (raw: string) => {
      expect(normalizeVersion(raw)).toBeNull();
    },
  );
});

describe('parseManifest', () => {
  test('accepts key=value text and ignores unknown keys', () => {
    expect(release(TEXT)).toEqual({
      status: 'available',
      version: '1.10.0.0',
      url: URL_1_10,
      fileName: 'Regressify-Desktop-Setup-1.10.0.0-x64.exe',
      sha256: SHA,
      size: 9,
    });
  });

  test('accepts JSON with a short version, uppercase hash, and extra keys', () => {
    const json = JSON.stringify({
      version: '1.10.0',
      installer: URL_1_10,
      sha256: SHA.toUpperCase(),
      size: 9,
      pub_date: '2026-09-18T00:00:00.000Z',
    });
    expect(release(json)).toEqual(release(TEXT));
    expect(release(JSON.stringify({ version: '1.10', installer: URL_1_10, sha256: SHA, size: '9' }))).toEqual(
      release(TEXT),
    );
  });

  test('strips a leading BOM and surrounding whitespace', () => {
    expect(release(`\uFEFF  \n${TEXT}\n  `)).toEqual(release(TEXT));
  });

  test('accepts size boundaries of 1 byte and 1 GiB', () => {
    expect(release(TEXT.replace('size=9', 'size=1')).size).toBe(1);
    expect(release(TEXT.replace('size=9', 'size=1073741824')).size).toBe(1073741824);
  });

  const invalid: Array<[string, string]> = [
    ['http scheme', TEXT.replace('https://drive', 'http://drive')],
    ['another host', TEXT.replace('drive.tuyen.blog/downloads', 'evil.example/downloads')],
    ['look-alike host', TEXT.replace('drive.tuyen.blog', 'drive.tuyen.blog.evil.example')],
    ['another slug', TEXT.replace('/regressify/', '/another-app/')],
    ['query string', TEXT.replace('-x64.exe', '-x64.exe?redirect=elsewhere')],
    ['fragment', TEXT.replace('-x64.exe', '-x64.exe#fragment')],
    ['non-canonical file name', TEXT.replace('Regressify-Desktop-Setup-1.10.0.0-x64.exe', 'latest/installer')],
    ['version mismatch with URL', TEXT.replace('version=1.10.0.0', 'version=1.11')],
    ['non-normalized URL', TEXT.replace('https://drive.tuyen.blog', 'https://DRIVE.tuyen.blog')],
    ['javascript URL', TEXT.replace(URL_1_10, 'javascript:alert(1)')],
    ['size 0', TEXT.replace('size=9', 'size=0')],
    ['size above 1 GiB', TEXT.replace('size=9', 'size=1073741825')],
    ['fractional size', TEXT.replace('size=9', 'size=9.5')],
    ['negative size', TEXT.replace('size=9', 'size=-9')],
    ['duplicate key', `${TEXT}version=2\n`],
    ['missing sha256', TEXT.replace('sha256=', 'hash=')],
    ['short sha256', TEXT.replace(SHA, SHA.slice(1))],
    ['non-hex sha256', TEXT.replace(SHA, `${'g'.repeat(64)}`)],
    ['line without equals', `${TEXT}garbage\n`],
    ['invalid version', TEXT.replace('version=1.10.0.0', 'version=v1.10')],
    ['oversized manifest', 'x'.repeat(16 * 1024 + 1)],
    ['invalid JSON', '{"version": '],
    ['JSON array', '[1,2]'],
    ['JSON non-string version', JSON.stringify({ version: 1, installer: URL_1_10, sha256: SHA, size: 9 })],
    ['JSON fractional size', JSON.stringify({ version: '1.10', installer: URL_1_10, sha256: SHA, size: 9.5 })],
    ['empty', ''],
  ];
  test.each(invalid)('rejects %s', (_label: string, text: string) => {
    expect(parseManifest(text, 'regressify').ok).toBe(false);
  });

  test('rejects invalid slugs', () => {
    for (const slug of ['', '../other', 'app/path', 'app?x', 'app\n', 'App', 'a'.repeat(65)]) {
      expect(isValidSlug(slug)).toBe(false);
      expect(parseManifest(TEXT, slug).ok).toBe(false);
    }
    expect(isValidSlug('a'.repeat(64))).toBe(true);
  });
});

describe('fetchRelease', () => {
  const respond =
    (status: number, body = '') =>
    async () =>
      new Response(body, { status });

  test('maps 404 to unpublished', async () => {
    expect(await fetchRelease({ slug: 'regressify', fetchImpl: respond(404) })).toEqual({
      status: 'unpublished',
    });
  });

  test('maps other HTTP failures to errors', async () => {
    for (const status of [302, 401, 503]) {
      const result = await fetchRelease({ slug: 'regressify', fetchImpl: respond(status, TEXT) });
      expect(result.status).toBe('error');
    }
  });

  test('returns a validated release', async () => {
    const result = await fetchRelease({ slug: 'regressify', fetchImpl: respond(200, TEXT) });
    expect(result).toEqual(release(TEXT));
  });

  test('requests the manifest without following redirects', async () => {
    let seen: { url: string; init?: RequestInit } | undefined;
    const fetchImpl: FetchLike = async (url, init) => {
      seen = { url, init };
      return new Response('', { status: 404 });
    };
    await fetchRelease({ slug: 'regressify', fetchImpl });
    expect(seen?.url).toBe('https://drive.tuyen.blog/downloads/regressify/latest.txt');
    expect(seen?.init?.redirect).toBe('error');
  });

  test('rejects oversized and invalid bodies', async () => {
    const big = await fetchRelease({
      slug: 'regressify',
      fetchImpl: respond(200, 'x'.repeat(16 * 1024 + 1)),
    });
    expect(big).toEqual({ status: 'error', reason: 'manifest is too large' });
    const bad = await fetchRelease({ slug: 'regressify', fetchImpl: respond(200, 'nope') });
    expect(bad.status).toBe('error');
  });

  test('never throws on network failures or timeouts', async () => {
    const failing: FetchLike = async () => {
      throw new TypeError('network down');
    };
    expect(await fetchRelease({ slug: 'regressify', fetchImpl: failing })).toEqual({
      status: 'error',
      reason: 'request failed',
    });
    const hanging: FetchLike = (_url, init) =>
      new Promise((_resolve, reject) => {
        init.signal?.addEventListener('abort', () => reject(init.signal?.reason));
      });
    expect(await fetchRelease({ slug: 'regressify', fetchImpl: hanging, timeoutMs: 20 })).toEqual({
      status: 'error',
      reason: 'timed out',
    });
  });

  test('rejects an invalid slug without a request', async () => {
    let called = false;
    const fetchImpl: FetchLike = async () => {
      called = true;
      return new Response('');
    };
    expect((await fetchRelease({ slug: '../x', fetchImpl })).status).toBe('error');
    expect(called).toBe(false);
  });
});

test('formatSize uses one decimal place', () => {
  expect(formatSize(12_345_678)).toBe('11.8 MB');
});

describe('downloadHref', () => {
  test('defaults to the published desktop feed slug', () => {
    expect(DEFAULT_APP_SLUG).toBe('regressify-desktop');
    expect(latestInstallerUrl(DEFAULT_APP_SLUG)).toBe(
      'https://drive.tuyen.blog/downloads/regressify-desktop/latest/installer',
    );
  });

  test('links the stable redirect, not the versioned file, for an available release', () => {
    expect(downloadHref(release(TEXT), 'regressify')).toBe(
      'https://drive.tuyen.blog/downloads/regressify/latest/installer',
    );
  });

  test('still links the stable redirect when the feed check failed', () => {
    expect(downloadHref({ status: 'error', reason: 'timed out' }, 'regressify')).toBe(
      'https://drive.tuyen.blog/downloads/regressify/latest/installer',
    );
  });

  test('falls back when nothing is published or the slug is invalid', () => {
    expect(downloadHref({ status: 'unpublished' }, 'regressify')).toBeNull();
    for (const slug of ['', '../other', 'app/path', 'App']) {
      expect(downloadHref(release(TEXT), slug)).toBeNull();
    }
  });
});
