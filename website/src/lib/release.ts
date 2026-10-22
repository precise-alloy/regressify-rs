// Build-time resolution of the latest Windows installer.
//
// The desktop updater (src/desktop/updates.rs, UpdateFeed::parse_manifest) is the
// authority for the manifest format. This module mirrors its validation so the
// site never links to anything the app itself would reject. The manifest is
// untrusted remote data: only the validated, reconstructed installer URL and the
// validated scalar fields ever reach the page, and nothing is rendered as HTML.
//
// Resolution runs once per build. It never throws unless RELEASE_REQUIRED=1, so
// offline or unpublished builds still succeed. Download buttons link to the
// feed's stable `latest/installer` redirect (see downloadHref) rather than the
// versioned file, so a site built before a release still serves the newest
// installer; the manifest only supplies the version, size, and checksum shown.

export const DRIVE_BASE_URL = 'https://drive.tuyen.blog';
export const DEFAULT_APP_SLUG = 'regressify-desktop';
export const MAX_MANIFEST_BYTES = 16 * 1024;
export const MAX_INSTALLER_BYTES = 1024 * 1024 * 1024;
export const FETCH_TIMEOUT_MS = 10_000;

const SLUG_PATTERN = /^[a-z0-9_-]{1,64}$/;
const SHA256_PATTERN = /^[0-9a-fA-F]{64}$/;

export interface AvailableRelease {
  status: 'available';
  /** Four-part version exactly as the desktop app formats it, e.g. 1.10.0.0. */
  version: string;
  url: string;
  fileName: string;
  /** Lowercase hexadecimal SHA-256 of the installer. */
  sha256: string;
  size: number;
}

export type Release =
  | AvailableRelease
  | { status: 'unpublished' }
  | { status: 'error'; reason: string };

export type ManifestResult =
  | { ok: true; release: AvailableRelease }
  | { ok: false; reason: string };

export function isValidSlug(slug: string): boolean {
  return SLUG_PATTERN.test(slug);
}

/**
 * Parses one to four dot-separated components (ASCII digits, 0-9999, total length
 * at most 32) and returns the canonical four-part form, or null when invalid.
 * Leading zeros are accepted and dropped, matching Rust's u16 parsing.
 */
export function normalizeVersion(raw: string): string | null {
  if (raw.length > 32) return null;
  const parts = raw.split('.');
  if (parts.length > 4) return null;
  const numbers: number[] = [];
  for (const part of parts) {
    if (!/^[0-9]+$/.test(part)) return null;
    const value = Number(part);
    if (value > 9999) return null;
    numbers.push(value);
  }
  while (numbers.length < 4) numbers.push(0);
  return numbers.join('.');
}

export function installerFileName(version: string): string {
  return `Regressify-Desktop-Setup-${version}-x64.exe`;
}

export function expectedInstallerUrl(slug: string, version: string): string {
  return `${DRIVE_BASE_URL}/downloads/${slug}/${installerFileName(version)}`;
}

/** Stable redirect to the newest installer, served by the same release feed. */
export function latestInstallerUrl(slug: string): string {
  return `${DRIVE_BASE_URL}/downloads/${slug}/latest/installer`;
}

/**
 * Href for download buttons, or null when the caller should link to the download
 * page instead. A failed feed check (offline build, timeout, invalid manifest)
 * still links the stable redirect because it says nothing about whether a release
 * exists; only a confirmed 404 or an invalid slug falls back.
 */
export function downloadHref(release: Release, slug: string = configuredSlug()): string | null {
  if (!isValidSlug(slug) || release.status === 'unpublished') return null;
  return latestInstallerUrl(slug);
}

interface RawFields {
  version: unknown;
  installer: unknown;
  sha256: unknown;
  size: unknown;
}

function readFields(text: string): RawFields | string {
  if (text.startsWith('{')) {
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      return 'invalid JSON manifest';
    }
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      return 'invalid JSON manifest';
    }
    const record = value as Record<string, unknown>;
    return {
      version: record.version,
      installer: record.installer,
      sha256: record.sha256,
      size: record.size,
    };
  }

  const fields = new Map<string, string>();
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim();
    if (line === '') continue;
    const separator = line.indexOf('=');
    if (separator < 0) return 'invalid manifest line';
    const key = line.slice(0, separator).trim();
    if (fields.has(key)) return 'duplicate manifest field';
    fields.set(key, line.slice(separator + 1).trim());
  }
  // Unknown keys such as `notes` are ignored, as in the desktop updater.
  return {
    version: fields.get('version'),
    installer: fields.get('installer'),
    sha256: fields.get('sha256'),
    size: fields.get('size'),
  };
}

function readSize(value: unknown): number | null {
  if (typeof value === 'number') {
    return Number.isSafeInteger(value) && value >= 0 ? value : null;
  }
  // Rust's u64::from_str accepts an optional leading '+'.
  if (typeof value === 'string' && /^\+?[0-9]+$/.test(value)) {
    const parsed = Number(value);
    return Number.isSafeInteger(parsed) ? parsed : null;
  }
  return null;
}

/** Pure manifest validator; ported from UpdateFeed::parse_manifest. */
export function parseManifest(text: string, slug: string): ManifestResult {
  if (!isValidSlug(slug)) return { ok: false, reason: 'invalid app slug' };
  if (new TextEncoder().encode(text).length > MAX_MANIFEST_BYTES) {
    return { ok: false, reason: 'manifest is too large' };
  }
  const fields = readFields(text.replace(/^\uFEFF+/, '').trim());
  if (typeof fields === 'string') return { ok: false, reason: fields };

  const { version: rawVersion, installer, sha256, size: rawSize } = fields;
  if (
    typeof rawVersion !== 'string' ||
    typeof installer !== 'string' ||
    typeof sha256 !== 'string' ||
    rawSize === undefined
  ) {
    return { ok: false, reason: 'missing required manifest fields' };
  }

  const version = normalizeVersion(rawVersion);
  if (version === null) return { ok: false, reason: 'invalid version' };

  // Require the immutable, versioned HTTPS URL on the expected host and path, both
  // as the raw string and after URL normalisation, so no alias, query, fragment,
  // or look-alike URL can be linked.
  const expected = expectedInstallerUrl(slug, version);
  let normalized: string;
  try {
    normalized = new URL(installer).href;
  } catch {
    return { ok: false, reason: 'invalid installer URL' };
  }
  if (installer !== expected || normalized !== expected) {
    return { ok: false, reason: 'unexpected installer URL' };
  }

  if (!SHA256_PATTERN.test(sha256)) return { ok: false, reason: 'invalid SHA-256' };

  const size = readSize(rawSize);
  if (size === null) return { ok: false, reason: 'invalid installer size' };
  if (size < 1 || size > MAX_INSTALLER_BYTES) {
    return { ok: false, reason: 'installer size out of range' };
  }

  return {
    ok: true,
    release: {
      status: 'available',
      version,
      url: expected,
      fileName: installerFileName(version),
      sha256: sha256.toLowerCase(),
      size,
    },
  };
}

/** Reads at most `limit` bytes so an oversized body cannot exhaust memory. */
async function readLimited(response: Response, limit: number): Promise<string | null> {
  const reader = response.body?.getReader();
  if (!reader) return '';
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
}

/** The subset of fetch used here, so tests can inject a stub. */
export type FetchLike = (url: string, init: RequestInit) => Promise<Response>;

export interface FetchOptions {
  slug: string;
  fetchImpl?: FetchLike;
  timeoutMs?: number;
}

export async function fetchRelease({
  slug,
  fetchImpl = fetch,
  timeoutMs = FETCH_TIMEOUT_MS,
}: FetchOptions): Promise<Release> {
  if (!isValidSlug(slug)) return { status: 'error', reason: 'invalid app slug' };
  try {
    const response = await fetchImpl(`${DRIVE_BASE_URL}/downloads/${slug}/latest.txt`, {
      // A redirect could point anywhere; the release job verifies the manifest
      // without redirects, so the site refuses them too.
      redirect: 'error',
      cache: 'no-store',
      headers: { 'Cache-Control': 'no-cache' },
      signal: AbortSignal.timeout(timeoutMs),
    });
    // 404 is the documented "nothing published yet" state, not a failure.
    if (response.status === 404) return { status: 'unpublished' };
    if (!response.ok) return { status: 'error', reason: `HTTP ${response.status}` };
    const text = await readLimited(response, MAX_MANIFEST_BYTES);
    if (text === null) return { status: 'error', reason: 'manifest is too large' };
    const parsed = parseManifest(text, slug);
    return parsed.ok ? parsed.release : { status: 'error', reason: parsed.reason };
  } catch (error) {
    const name = error instanceof Error ? error.name : 'Error';
    return { status: 'error', reason: name === 'TimeoutError' ? 'timed out' : 'request failed' };
  }
}

function readEnv(name: string): string | undefined {
  const astroEnv = (import.meta as { env?: Record<string, string | undefined> }).env;
  return astroEnv?.[name] ?? process.env[name];
}

/** Tuyen Drive app slug from PUBLIC_DRIVE_APP_SLUG, defaulting to DEFAULT_APP_SLUG. */
export function configuredSlug(): string {
  return readEnv('PUBLIC_DRIVE_APP_SLUG') || DEFAULT_APP_SLUG;
}

let cached: Promise<Release> | undefined;

/**
 * Memoized for the whole build so every page shows the same release and the feed
 * is requested once. Environment:
 * - PUBLIC_DRIVE_APP_SLUG: Tuyen Drive app slug (default "regressify-desktop").
 * - RELEASE_OFFLINE=1: skip the request and render the fallback.
 * - RELEASE_REQUIRED=1: fail the build unless a valid release is found.
 * - RELEASE_MANIFEST_FILE=path: validate a local manifest instead of fetching,
 *   for previewing the available state. It passes the same validator.
 */
export function getRelease(): Promise<Release> {
  cached ??= resolveRelease();
  return cached;
}

async function resolveRelease(): Promise<Release> {
  const slug = configuredSlug();
  const manifestFile = readEnv('RELEASE_MANIFEST_FILE');
  const release: Release = manifestFile
    ? await readManifestFile(manifestFile, slug)
    : readEnv('RELEASE_OFFLINE') === '1'
      ? { status: 'error', reason: 'RELEASE_OFFLINE=1' }
      : await fetchRelease({ slug });

  if (release.status !== 'available') {
    const detail = release.status === 'error' ? release.reason : 'no release published (HTTP 404)';
    if (readEnv('RELEASE_REQUIRED') === '1') {
      throw new Error(`[release] RELEASE_REQUIRED=1 but no valid release: ${detail}`);
    }
    console.warn(`[release] Release details unavailable: ${detail}`);
  }
  return release;
}

async function readManifestFile(path: string, slug: string): Promise<Release> {
  try {
    const { readFile } = await import('node:fs/promises');
    const parsed = parseManifest(await readFile(path, 'utf8'), slug);
    return parsed.ok ? parsed.release : { status: 'error', reason: parsed.reason };
  } catch {
    return { status: 'error', reason: 'cannot read RELEASE_MANIFEST_FILE' };
  }
}

export function formatSize(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
