// Ports the manifest cases from src/desktop/updates.rs so the site and the desktop
// updater accept exactly the same installer links.
import { describe, expect, test } from 'bun:test';
import {
  DEFAULT_DOWNLOAD_ORIGIN,
  downloadHref,
  type FetchLike,
  fetchRelease,
  formatSize,
  isValidOrigin,
  manifestUrl,
  normalizeVersion,
  parseChecksum,
  parseManifest,
  parseManifestArchives,
  RELEASE_DOWNLOAD_PREFIX,
  selectArchives,
} from './release';

const ORIGIN = 'https://regressify.eshn.dev';
const SHA = 'ab'.repeat(32);
const URL_1_10 = `${RELEASE_DOWNLOAD_PREFIX}/v1.10.0/Regressify-Setup-1.10.0.0-x64.exe`;
const ARCHIVES = {
  'archive.regressify-cli-x86_64-unknown-linux-gnu.zip': `42:${SHA}`,
  'archive.regressify-cli-x86_64-pc-windows-msvc.zip': `11:${SHA}`,
  'archive.regressify-desktop-x86_64-pc-windows-msvc.zip': `15:${SHA}`,
};
const TEXT = `version=1.10.0.0\r\ntag=v1.10.0\r\ninstaller=${URL_1_10}\r\nsha256=${SHA}\r\nsize=9\r\n${Object.entries(ARCHIVES).map(([key, value]) => `${key}=${value}`).join('\r\n')}\r\nnotes=https://example.com/notes\r\n`;

function release(text: string, origin = ORIGIN) {
  const result = parseManifest(text, origin);
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
      fileName: 'Regressify-Setup-1.10.0.0-x64.exe',
      sha256: SHA,
      size: 9,
      archives: selectArchives(Object.entries(ARCHIVES).map(([key, value]) => ({
        name: key.slice('archive.'.length), size: Number(value.split(':')[0]), checksum: SHA,
      })), 'v1.10.0'),
    });
  });

  test('accepts JSON with a short version, uppercase hash, and extra keys', () => {
    const json = JSON.stringify({
      version: '1.10.0',
      installer: URL_1_10,
      sha256: SHA.toUpperCase(),
      size: 9,
      pub_date: '2026-09-18T00:00:00.000Z',
      ...ARCHIVES,
    });
    expect(release(json)).toEqual(release(TEXT));
    expect(release(JSON.stringify({ version: '1.10', installer: URL_1_10, sha256: SHA, size: '9', ...ARCHIVES }))).toEqual(
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
    ['http scheme', TEXT.replace('https://github.com', 'http://github.com')],
    ['another host', TEXT.replace('github.com/', 'evil.example/')],
    ['look-alike host', TEXT.replace('github.com/', 'github.com.evil.example/')],
    ['explicit port', TEXT.replace('github.com/', 'github.com:8443/')],
    ['another repo', TEXT.replace('/precise-alloy/regressify-rs/', '/other/repo/')],
    ['nested directory', TEXT.replace('/download/v1.10.0/', '/download/nested/v1.10.0/')],
    ['dot segments', TEXT.replace('/download/v1.10.0/', '/download/../v1.10.0/')],
    ['tag without v prefix', TEXT.replace('/download/v1.10.0/', '/download/1.10.0/')],
    ['mismatched tag field', TEXT.replace('tag=v1.10.0', 'tag=v1.11.0')],
    ['query string', TEXT.replace('-x64.exe', '-x64.exe?redirect=elsewhere')],
    ['fragment', TEXT.replace('-x64.exe', '-x64.exe#fragment')],
    ['non-canonical file name', TEXT.replace('Regressify-Setup-1.10.0.0-x64.exe', 'latest/installer')],
    ['old desktop installer name', TEXT.replace('Regressify-Setup-1.10.0.0-x64.exe', 'Regressify-Desktop-Setup-1.10.0.0-x64.exe')],
    ['version mismatch with URL', TEXT.replace('version=1.10.0.0', 'version=1.11')],
    ['non-normalized URL', TEXT.replace('https://github.com', 'https://GITHUB.com')],
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
    expect(parseManifest(text, ORIGIN).ok).toBe(false);
  });

  test('rejects invalid origins', () => {
    for (const origin of [
      '',
      'regressify.eshn.dev',
      'http://regressify.eshn.dev',
      'https://regressify.eshn.dev/',
      'https://regressify.eshn.dev/path',
      'https://regressify.eshn.dev:8443',
      'https://user@regressify.eshn.dev',
      'https://Regressify.eshn.dev',
      'https://.eshn.dev',
      'https://regressify..dev',
      'https://regressify.eshn.dev?x',
    ]) {
      expect(isValidOrigin(origin)).toBe(false);
      expect(parseManifest(TEXT, origin).ok).toBe(false);
    }
    expect(isValidOrigin(ORIGIN)).toBe(true);
    expect(DEFAULT_DOWNLOAD_ORIGIN).toBe(ORIGIN);
  });
});

describe('fetchRelease', () => {
  const respond =
    (status: number, body = '') =>
    async () =>
      new Response(body, { status });

  test('maps 404 to unpublished', async () => {
    expect(await fetchRelease({ origin: ORIGIN, fetchImpl: respond(404) })).toEqual({ status: 'unpublished' });
  });

  test('maps other HTTP failures to errors', async () => {
    for (const status of [302, 401, 503]) {
      const result = await fetchRelease({ origin: ORIGIN, fetchImpl: respond(status, TEXT) });
      expect(result.status).toBe('error');
    }
  });

  test('returns a validated release', async () => {
    expect(await fetchRelease({ origin: ORIGIN, fetchImpl: respond(200, TEXT) })).toEqual(release(TEXT));
  });

  test('retains the platform inventory from the hosted feed', async () => {
    const result = await fetchRelease({ origin: ORIGIN, fetchImpl: respond(200, TEXT) });
    expect('archives' in result).toBe(true);
    if (result.status === 'available') expect(result.archives).toHaveLength(3);
  });

  test('requests the site manifest without following redirects', async () => {
    let seen: { url: string; init?: RequestInit } | undefined;
    const fetchImpl: FetchLike = async (url, init) => {
      seen = { url, init };
      return new Response('', { status: 404 });
    };
    await fetchRelease({ origin: ORIGIN, fetchImpl });
    expect(seen?.url).toBe(`${ORIGIN}/downloads/latest.txt`);
    expect(seen?.url).toBe(manifestUrl(ORIGIN));
    expect(seen?.init?.redirect).toBe('error');
  });

  test('rejects oversized and invalid bodies', async () => {
    const big = await fetchRelease({ origin: ORIGIN, fetchImpl: respond(200, 'x'.repeat(16 * 1024 + 1)) });
    expect(big).toEqual({ status: 'error', reason: 'manifest is too large' });
    const bad = await fetchRelease({ origin: ORIGIN, fetchImpl: respond(200, 'nope') });
    expect(bad.status).toBe('error');
  });

  test('never throws on network failures or timeouts', async () => {
    const failing: FetchLike = async () => {
      throw new TypeError('network down');
    };
    expect(await fetchRelease({ origin: ORIGIN, fetchImpl: failing })).toEqual({
      status: 'error',
      reason: 'request failed',
    });
    const hanging: FetchLike = (_url, init) =>
      new Promise((_resolve, reject) => {
        init.signal?.addEventListener('abort', () => reject(init.signal?.reason));
      });
    expect(await fetchRelease({ origin: ORIGIN, fetchImpl: hanging, timeoutMs: 20 })).toEqual({
      status: 'error',
      reason: 'timed out',
    });
  });

  test('rejects an invalid origin without a request', async () => {
    let called = false;
    const fetchImpl: FetchLike = async () => {
      called = true;
      return new Response('');
    };
    expect((await fetchRelease({ origin: 'https://evil.example/x', fetchImpl })).status).toBe('error');
    expect(called).toBe(false);
  });
});

test('formatSize uses one decimal place', () => {
  expect(formatSize(12_345_678)).toBe('11.8 MB');
});

describe('downloadHref', () => {
  test('links the verified versioned installer only for an available release', () => {
    expect(downloadHref(release(TEXT))).toBe(URL_1_10);
    expect(downloadHref({ status: 'unpublished' })).toBeNull();
    expect(downloadHref({ status: 'error', reason: 'timed out' })).toBeNull();
  });
});

describe('release archives', () => {
  const name = 'regressify-cli-x86_64-unknown-linux-gnu.zip';

  test('parses sha256sum and shasum sidecars for the named file only', () => {
    expect(parseChecksum(`${SHA.toUpperCase()}  ${name}\n`, name)).toBe(SHA);
    expect(parseChecksum(`${SHA} *${name}`, name)).toBe(SHA);
    expect(parseChecksum(`${SHA}  other.zip`, name)).toBeNull();
    expect(parseChecksum(`${SHA.slice(1)}  ${name}`, name)).toBeNull();
    expect(parseChecksum('', name)).toBeNull();
  });

  test('lists only recognised archives with checksums, in a stable order', () => {
    const archives = selectArchives([
      { name: 'regressify-desktop-x86_64-pc-windows-msvc.zip', size: 3, checksum: SHA },
      { name: 'regressify-cli-x86_64-unknown-linux-gnu.zip', size: 2, checksum: SHA },
      { name: 'regressify-cli-x86_64-pc-windows-msvc.7z', size: 1, checksum: SHA },
      { name: 'regressify-cli-x86_64-pc-windows-msvc.zip', size: 1, checksum: SHA },
      { name: 'regressify-cli-x86_64-pc-windows-msvc.zip.sha256', size: 1, checksum: SHA },
      { name: 'regressify-cli-riscv64-unknown-linux-gnu.zip', size: 1, checksum: SHA },
      { name: 'regressify-cli-x86_64-unknown-linux-musl.zip', size: 1, checksum: SHA },
      { name: 'regressify-cli-aarch64-unknown-linux-musl.zip', size: 1, checksum: SHA },
      { name: 'regressify-cli-aarch64-apple-darwin.zip', size: 1, checksum: null },
      { name: 'regressify-cli-x86_64-apple-darwin.zip', size: 0, checksum: SHA },
      { name: '../regressify-cli-x86_64-pc-windows-msvc.zip', size: 1, checksum: SHA },
      { name: 'Regressify-Setup-1.0.0.0-x64.exe', size: 1, checksum: SHA },
    ], 'v1.10.0');
    expect(archives.map((archive) => archive.fileName)).toEqual([
      'regressify-cli-x86_64-pc-windows-msvc.7z',
      'regressify-cli-x86_64-pc-windows-msvc.zip',
      'regressify-cli-x86_64-unknown-linux-gnu.zip',
      'regressify-desktop-x86_64-pc-windows-msvc.zip',
    ]);
    expect(archives[0]).toEqual({
      application: 'cli',
      target: 'x86_64-pc-windows-msvc',
      platform: 'Windows x64',
      format: '7z',
      fileName: 'regressify-cli-x86_64-pc-windows-msvc.7z',
      url: `${RELEASE_DOWNLOAD_PREFIX}/v1.10.0/regressify-cli-x86_64-pc-windows-msvc.7z`,
      size: 1,
      sha256: SHA,
    });
    expect(parseManifestArchives(TEXT).filter((archive) => archive.fileName === name)).toEqual([
      {
        application: 'cli',
        target: 'x86_64-unknown-linux-gnu',
        platform: 'Linux x64 (glibc)',
        format: 'zip',
        fileName: 'regressify-cli-x86_64-unknown-linux-gnu.zip',
        url: `${RELEASE_DOWNLOAD_PREFIX}/v1.10.0/regressify-cli-x86_64-unknown-linux-gnu.zip`,
        size: 42,
        sha256: SHA,
      },
    ]);
  });
});
