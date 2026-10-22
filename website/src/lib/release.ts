// Build-time resolution of the latest release downloads.
//
// Release binaries are stored in versioned GitHub Releases. The website serves
// /downloads/latest.txt, selecting the version supported by this deployment.
// The desktop updater (src/desktop/updates.rs,
// UpdateFeed::parse_manifest) is the authority for the `latest.txt` manifest
// format. This module mirrors its validation so the site never links to anything
// the app itself would reject. The manifest is untrusted data: only the validated,
// reconstructed installer URL and the validated scalar fields ever reach the page,
// and nothing is rendered as HTML.
//
// Release deployments pass the staged RELEASE_MANIFEST_FILE. Other builds fetch the deployed
// manifest once and never throw unless RELEASE_REQUIRED=1, so offline or
// unpublished builds still succeed.

export const DEFAULT_DOWNLOAD_ORIGIN = 'https://regressify.eshn.dev';
export const DOWNLOADS_PATH = '/downloads';
export const RELEASE_REPO = 'precise-alloy/regressify-rs';
export const RELEASE_DOWNLOAD_PREFIX = `https://github.com/${RELEASE_REPO}/releases/download`;
export const MAX_MANIFEST_BYTES = 16 * 1024;
export const MAX_INSTALLER_BYTES = 1024 * 1024 * 1024;
export const FETCH_TIMEOUT_MS = 10_000;

const SHA256_PATTERN = /^[0-9a-fA-F]{64}$/;
const TAG_PATTERN = /^v[0-9]{1,4}(?:\.[0-9]{1,4}){2,3}$/;

export interface AvailableRelease {
  status: 'available';
  /** Four-part version exactly as the desktop app formats it, e.g. 1.10.0.0. */
  version: string;
  url: string;
  fileName: string;
  /** Lowercase hexadecimal SHA-256 of the installer. */
  sha256: string;
  size: number;
  archives: ReleaseArchive[];
}

export type Release =
  | AvailableRelease
  | { status: 'unpublished' }
  | { status: 'error'; reason: string };

export type ManifestResult =
  | { ok: true; release: AvailableRelease }
  | { ok: false; reason: string };

/** `https://host` with a lowercase DNS name and no port, path, or credentials. */
export function isValidOrigin(origin: string): boolean {
  const host = origin.startsWith('https://') ? origin.slice('https://'.length) : '';
  return (
    /^[a-z0-9.-]{1,253}$/.test(host) &&
    !/^[.-]|[.-]$|\.\./.test(host) &&
    URL.canParse(origin) &&
    new URL(origin).origin === origin
  );
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
  return `Regressify-Setup-${version}-x64.exe`;
}

export function expectedInstallerUrl(tag: string, version: string): string {
  return `${RELEASE_DOWNLOAD_PREFIX}/${tag}/${installerFileName(version)}`;
}

export function releaseTagFromInstallerUrl(installerUrl: string, version: string): string | null {
  const prefix = `${RELEASE_DOWNLOAD_PREFIX}/`;
  const suffix = `/${installerFileName(version)}`;
  if (!installerUrl.startsWith(prefix) || !installerUrl.endsWith(suffix)) return null;
  const tag = installerUrl.slice(prefix.length, installerUrl.length - suffix.length);
  if (!TAG_PATTERN.test(tag)) return null;
  if (normalizeVersion(tag.slice(1)) !== version) return null;
  return tag;
}

export function manifestUrl(origin: string): string {
  return `${origin}${DOWNLOADS_PATH}/latest.txt`;
}

/**
 * Href for download buttons, or null when the caller should link to the download
 * page instead (no verified release at build time).
 */
export function downloadHref(release: Release): string | null {
  return release.status === 'available' ? release.url : null;
}
interface RawFields {
  version: unknown;
  tag?: unknown;
  installer: unknown;
  sha256: unknown;
  size: unknown;
  archives: Array<[string, unknown]>;
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
      tag: record.tag,
      installer: record.installer,
      sha256: record.sha256,
      size: record.size,
      archives: Object.entries(record).filter(([key]) => key.startsWith('archive.')),
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
  // Notes remain optional; every archive link comes from this same validated feed.
  return {
    version: fields.get('version'),
    tag: fields.get('tag'),
    installer: fields.get('installer'),
    sha256: fields.get('sha256'),
    size: fields.get('size'),
    archives: [...fields].filter(([key]) => key.startsWith('archive.')),
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
export function parseManifest(text: string, origin: string): ManifestResult {
  if (!isValidOrigin(origin)) return { ok: false, reason: 'invalid download origin' };
  if (new TextEncoder().encode(text).length > MAX_MANIFEST_BYTES) {
    return { ok: false, reason: 'manifest is too large' };
  }
  const fields = readFields(text.replace(/^\uFEFF+/, '').trim());
  if (typeof fields === 'string') return { ok: false, reason: fields };

  const { version: rawVersion, tag: rawTag, installer, sha256, size: rawSize } = fields;
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

  const tag = releaseTagFromInstallerUrl(installer, version);
  if (tag === null) {
    return { ok: false, reason: 'unexpected installer URL' };
  }
  if (rawTag !== undefined && rawTag !== tag) {
    return { ok: false, reason: 'mismatched release tag' };
  }
  const expected = expectedInstallerUrl(tag, version);
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

  const entries: Array<{ name: string; size: number; checksum: string }> = [];
  for (const [key, value] of fields.archives) {
    const name = key.slice('archive.'.length);
    const match = typeof value === 'string' ? /^([0-9]+):([0-9a-fA-F]{64})$/.exec(value) : null;
    const archiveSize = match ? Number(match[1]) : 0;
    if (!match || !Number.isSafeInteger(archiveSize) || archiveSize < 1 || archiveSize > MAX_INSTALLER_BYTES) {
      return { ok: false, reason: 'invalid archive metadata' };
    }
    entries.push({ name, size: archiveSize, checksum: match[2] as string });
  }
  const archives = selectArchives(entries, tag);
  if (archives.length !== entries.length || !['cli', 'desktop'].every((application) =>
    archives.some((archive) => archive.fileName === `regressify-${application}-x86_64-pc-windows-msvc.zip`))) {
    return { ok: false, reason: 'missing or unsupported release archives' };
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
      archives,
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
  origin: string;
  fetchImpl?: FetchLike;
  timeoutMs?: number;
}

export async function fetchRelease({
  origin,
  fetchImpl = fetch,
  timeoutMs = FETCH_TIMEOUT_MS,
}: FetchOptions): Promise<Release> {
  if (!isValidOrigin(origin)) return { status: 'error', reason: 'invalid download origin' };
  try {
    const response = await fetchImpl(manifestUrl(origin), {
      // The desktop updater and the release check refuse redirects; so does the site.
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
    const parsed = parseManifest(text, origin);
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

/** Site origin hosting /downloads/, from PUBLIC_DOWNLOAD_ORIGIN or DEFAULT_DOWNLOAD_ORIGIN. */
export function configuredOrigin(): string {
  return readEnv('PUBLIC_DOWNLOAD_ORIGIN') || DEFAULT_DOWNLOAD_ORIGIN;
}

let cached: Promise<Release> | undefined;

/**
 * Memoized for the whole build so every page shows the same release and the feed
 * is requested once. Environment:
 * - PUBLIC_DOWNLOAD_ORIGIN: site origin hosting /downloads/ (default "https://regressify.eshn.dev").
 * - RELEASE_OFFLINE=1: skip the request and render the fallback.
 * - RELEASE_REQUIRED=1: fail the build unless a valid release is found.
 * - RELEASE_MANIFEST_FILE=path: validate a local manifest instead of fetching. Release
 *   deployments use this with the staged manifest. It passes the same validator.
 */
export function getRelease(): Promise<Release> {
  cached ??= resolveRelease();
  return cached;
}

async function resolveRelease(): Promise<Release> {
  const origin = configuredOrigin();
  const manifestFile = readEnv('RELEASE_MANIFEST_FILE');
  const release: Release = manifestFile
    ? await readManifestFile(manifestFile, origin)
    : readEnv('RELEASE_OFFLINE') === '1'
      ? { status: 'error', reason: 'RELEASE_OFFLINE=1' }
      : await fetchRelease({ origin });

  if (release.status !== 'available') {
    const detail = release.status === 'error' ? release.reason : 'no release published (HTTP 404)';
    if (readEnv('RELEASE_REQUIRED') === '1') {
      throw new Error(`[release] RELEASE_REQUIRED=1 but no valid release: ${detail}`);
    }
    console.warn(`[release] Release details unavailable: ${detail}`);
  }
  return release;
}

async function readManifestFile(path: string, origin: string): Promise<Release> {
  try {
    const { readFile } = await import('node:fs/promises');
    const parsed = parseManifest(await readFile(path, 'utf8'), origin);
    return parsed.ok ? parsed.release : { status: 'error', reason: parsed.reason };
  } catch {
    return { status: 'error', reason: 'cannot read RELEASE_MANIFEST_FILE' };
  }
}

export const TARGETS: Readonly<Record<string, string>> = {
  'x86_64-pc-windows-msvc': 'Windows x64',
  'x86_64-apple-darwin': 'macOS Intel',
  'aarch64-apple-darwin': 'macOS Apple silicon',
  'x86_64-unknown-linux-gnu': 'Linux x64 (glibc)',
};

export interface ReleaseArchive {
  application: 'cli' | 'desktop';
  target: string;
  platform: string;
  format: 'zip' | '7z';
  fileName: string;
  /** Immutable tag-specific GitHub Release asset URL. */
  url: string;
  size: number;
  sha256: string;
}

const ARCHIVE_PATTERN = new RegExp(
  `^regressify-(cli|desktop)-(${Object.keys(TARGETS)
    .map((target) => target.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
    .join('|')})\\.(zip|7z)$`,
);

/**
 * Parses `<file>.sha256` sidecars written by sha256sum/shasum ("<hash>  <name>").
 * Returns the lowercase hash only when it names the expected file.
 */
export function parseChecksum(text: string, fileName: string): string | null {
  const match = /^([0-9a-fA-F]{64})[ \t]+\*?(\S+)\s*$/.exec(text.trim());
  return match && match[2] === fileName ? (match[1] as string).toLowerCase() : null;
}

/** Recognised release archives from a directory listing or manifest, in a stable display order. */
export function selectArchives(
  entries: ReadonlyArray<{ name: string; size: number; checksum: string | null }>,
  tag: string,
): ReleaseArchive[] {
  if (!TAG_PATTERN.test(tag)) return [];
  const archives: ReleaseArchive[] = [];
  for (const { name, size, checksum } of entries) {
    const match = ARCHIVE_PATTERN.exec(name);
    if (!match || checksum === null || !SHA256_PATTERN.test(checksum) || !Number.isSafeInteger(size) || size < 1 || size > MAX_INSTALLER_BYTES) {
      continue;
    }
    const target = match[2] as string;
    archives.push({
      application: match[1] as 'cli' | 'desktop',
      target,
      platform: TARGETS[target] as string,
      format: match[3] as 'zip' | '7z',
      fileName: name,
      url: `${RELEASE_DOWNLOAD_PREFIX}/${tag}/${name}`,
      size,
      sha256: checksum.toLowerCase(),
    });
  }
  const order = Object.keys(TARGETS);
  return archives.sort(
    (a, b) =>
      a.application.localeCompare(b.application) ||
      order.indexOf(a.target) - order.indexOf(b.target) ||
      a.format.localeCompare(b.format),
  );
}

/** Extracts `archive.<name>=<size>:<sha256>` entries from a validated `latest.txt` manifest. */
export function parseManifestArchives(text: string, origin = DEFAULT_DOWNLOAD_ORIGIN): ReleaseArchive[] {
  const parsed = parseManifest(text, origin);
  return parsed.ok ? parsed.release.archives : [];
}

export async function getReleaseArchives(): Promise<ReleaseArchive[]> {
  const release = await getRelease();
  return release.status === 'available' ? release.archives : [];
}
export function formatSize(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
