export interface AdminEnv {
  POSTHOG_API_ENDPOINT?: string;
  POSTHOG_API_TOKEN?: string;
}

export type AdminFetch = (input: string, init?: RequestInit) => Promise<Response>;

export type SiteDetailValue = string | number | boolean | null | SiteDetailValue[] | { [key: string]: SiteDetailValue };

export interface SiteRecord {
  url: string;
  datetime: string | null;
  file_count: number | null;
  total_bytes: number | null;
  duration_ms: number | null;
  details?: Record<string, SiteDetailValue>;
}

const fields = ['url', 'datetime', 'file_count', 'total_bytes', 'duration_ms'] as const;
const maxBytes = 2 * 1024 * 1024;

function isDetailValue(value: unknown, depth = 0): value is SiteDetailValue {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
  if (typeof value === 'number') return Number.isFinite(value);
  if (depth >= 16 || typeof value !== 'object') return false;
  if (Array.isArray(value)) return value.every(item => isDetailValue(item, depth + 1));
  return (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null) &&
    Object.values(value).every(item => isDetailValue(item, depth + 1));
}

function reply(body: unknown, status = 200, extra: Record<string, string> = {}): Response {
  return Response.json(body, {
    status,
    headers: {
      'Cache-Control': 'private, no-store',
      'X-Content-Type-Options': 'nosniff',
      'X-Robots-Tag': 'noindex, nofollow',
      ...extra,
    },
  });
}

export function parseSites(data: unknown): { sites: SiteRecord[]; hasMore: boolean } {
  if (!data || typeof data !== 'object') throw new Error('Invalid result');
  const { columns, results, error, hasMore } = data as Record<string, unknown>;
  if (error || !Array.isArray(columns) || !columns.every(column => typeof column === 'string') ||
    !Array.isArray(results) || results.length > 10_000 ||
    (hasMore !== undefined && typeof hasMore !== 'boolean')) throw new Error('Invalid result');
  const normalized = columns.map(column => column.trim().toLowerCase());
  if (normalized.some(column => !column) || new Set(normalized).size !== normalized.length) throw new Error('Invalid columns');
  const indexes = fields.map(field => {
    const index = normalized.indexOf(field);
    if (index === -1 || normalized.lastIndexOf(field) !== index) throw new Error('Invalid columns');
    return index;
  });
  const additional = normalized.map((key, index) => ({ key, index })).filter(({ key }) => !fields.some(field => field === key));
  const sites = results.map((row: unknown): SiteRecord => {
    if (!Array.isArray(row) || row.length !== columns.length) throw new Error('Invalid row');
    const [url, datetime, fileCount, totalBytes, durationMs] = indexes.map(index => row[index]);
    if (typeof url !== 'string' || url.length > 8192 ||
      (datetime !== null && (typeof datetime !== 'string' || !Number.isFinite(Date.parse(datetime))))) {
      throw new Error('Invalid site');
    }
    for (const value of [fileCount, totalBytes, durationMs]) {
      if (value !== null && (typeof value !== 'number' || !Number.isFinite(value) || value < 0)) {
        throw new Error('Invalid measurement');
      }
    }
    const details = Object.fromEntries(additional.map(({ key, index }) => {
      const value = row[index];
      if (!isDetailValue(value)) throw new Error('Invalid detail');
      return [key, value];
    }));
    return {
      url, datetime, file_count: fileCount, total_bytes: totalBytes, duration_ms: durationMs,
      ...(additional.length ? { details } : {}),
    };
  });
  return { sites, hasMore: hasMore === true };
}

async function readResult(response: Response): Promise<unknown> {
  if (!response.body) throw new Error('Missing body');
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > maxBytes) throw new Error('Response too large');
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return JSON.parse(new TextDecoder().decode(bytes));
}

export async function serveSites(request: Request, env: AdminEnv, fetcher: AdminFetch = (input, init) => fetch(input, init)): Promise<Response> {
  if (request.method !== 'GET') return reply({ error: 'Method not allowed.' }, 405, { Allow: 'GET' });
  const origin = request.headers.get('Origin');
  if ((origin && origin !== new URL(request.url).origin) || request.headers.get('Sec-Fetch-Site') === 'cross-site') {
    return reply({ error: 'Forbidden.' }, 403);
  }
  if (!env.POSTHOG_API_ENDPOINT?.trim() || !env.POSTHOG_API_TOKEN?.trim()) {
    return reply({ error: 'Statistics are not configured.' }, 503);
  }
  let stage = 'configuration';
  try {
    const endpoint = new URL(env.POSTHOG_API_ENDPOINT.trim());
    if (endpoint.protocol !== 'https:' || endpoint.username || endpoint.password) throw new Error('Invalid endpoint');
    stage = 'request';
    const response = await fetcher(endpoint.href, {
      method: 'POST',
      headers: { Authorization: `Bearer ${env.POSTHOG_API_TOKEN.trim()}`, 'Content-Type': 'application/json' },
      body: '{}',
      // Workers supports manual/follow; reject redirects below without forwarding credentials.
      redirect: 'manual',
      signal: AbortSignal.timeout(15_000),
    });
    stage = 'response';
    if (!response.ok) {
      await response.body?.cancel();
      throw new Error('Upstream failure');
    }
    stage = 'validation';
    return reply({ ...parseSites(await readResult(response)), fetchedAt: new Date().toISOString() });
  } catch (error) {
    console.warn('Admin feed unavailable', { stage, kind: error instanceof Error ? error.name : 'unknown' });
    return reply({ error: 'Unable to load statistics. Please try again later.' }, 502);
  }
}