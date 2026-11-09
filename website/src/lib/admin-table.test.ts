import { expect, test } from 'bun:test';
import { formatSiteDetailValue, formatSiteValue, metricStyle, safeSiteUrl, selectSites, siteDetailEntries } from './admin-table';
import type { SiteRecord } from './admin-api';

test('formats count grouping, binary MiB, elapsed times and duration carries', () => {
  expect(formatSiteValue('file_count', 1234567)).toBe('1.234.567');
  expect(formatSiteValue('total_bytes', 1048576)).toBe('1.0');
  expect(formatSiteValue('total_bytes', 0)).toBe('0.0');
  expect(formatSiteValue('duration_ms', 59_500)).toBe('1:00');
  expect(formatSiteValue('duration_ms', 3_599_500)).toBe('1:00:00');
  expect(formatSiteValue('duration_ms', 0)).toBe('0:00');
  expect(formatSiteValue('duration_ms', null)).toBe('-');
  const now = Date.parse('2026-10-23T12:00:00Z');
  expect(formatSiteValue('datetime', '2026-10-23T11:00:00Z', now)).toBe('1h');
  expect(formatSiteValue('datetime', '2026-10-23T13:00:00Z', now)).toBe('1h');
  expect(formatSiteValue('datetime', 'invalid', now)).toBe('invalid');
});

test('allows only HTTP(S) destinations without embedded credentials', () => {
  expect(safeSiteUrl('https://demo.example/report/?page=2#image')?.href).toBe('https://demo.example/report/?page=2#image');
  for (const value of ['javascript:alert(1)', 'data:text/html,<script>', '/relative', 'https://user:pass@example.com']) {
    expect(safeSiteUrl(value)).toBeNull();
  }
});

test('formats deployment details with exact measurements, explicit missing values and a zoned timestamp', () => {
  expect(formatSiteDetailValue('url', 'https://example.com/report/?id=1#result')).toBe('https://example.com/report/?id=1#result');
  expect(formatSiteDetailValue('file_count', 1234)).toBe('1.234');
  expect(formatSiteDetailValue('total_bytes', 1048576)).toBe('1.0 MiB (1.048.576 bytes)');
  expect(formatSiteDetailValue('total_bytes', 0)).toBe('0.0 MiB (0 bytes)');
  expect(formatSiteDetailValue('duration_ms', 59_500)).toBe('1:00 (59.500 ms)');
  expect(formatSiteDetailValue('duration_ms', 0)).toBe('0:00 (0 ms)');
  for (const key of ['datetime', 'file_count', 'total_bytes', 'duration_ms'] as const) {
    expect(formatSiteDetailValue(key, null)).toBe('Not available');
  }
  const timestamp = '2026-10-23T12:34:56Z';
  expect(formatSiteDetailValue('datetime', timestamp)).toBe(new Intl.DateTimeFormat(undefined, {
    dateStyle: 'full', timeStyle: 'long',
  }).format(new Date(timestamp)));
  expect(formatSiteDetailValue('datetime', 'invalid')).toBe('invalid');
});

test('lists all additional fields without changing the table columns or requiring new field definitions', () => {
  const site: SiteRecord = {
    url: 'https://example.com/', datetime: null, file_count: 1, total_bytes: 0, duration_ms: 0,
    details: {
      original_bytes: 2097152, os: 'windows', os_version: '11', provider: 'cloudflare',
      future_field: { enabled: false }, constructor: 'value',
    },
  };
  expect(siteDetailEntries(site).map(([key, label]) => [key, label])).toEqual([
    ['url', 'Site'], ['datetime', 'Deployed'], ['file_count', 'Files'], ['total_bytes', 'Size'], ['duration_ms', 'Duration'],
    ['original_bytes', 'Original Size'], ['os', 'OS'], ['os_version', 'OS Version'], ['provider', 'Provider'],
    ['future_field', 'Future Field'], ['constructor', 'Constructor'],
  ]);
  expect(siteDetailEntries({ ...site, details: undefined })).toHaveLength(5);
  expect(siteDetailEntries(site).at(-2)?.[2]).toEqual({ enabled: false });
});

test('formats new and future detail types without hiding falsy values or interpreting markup', () => {
  expect(formatSiteDetailValue('original_bytes', 2097152)).toBe('2.0 MiB (2.097.152 bytes)');
  expect(formatSiteDetailValue('upload_ms', 1000)).toBe('0:01 (1.000 ms)');
  expect(formatSiteDetailValue('os_version', '11')).toBe('11');
  expect(formatSiteDetailValue('provider', '<img src=x onerror=alert(1)>')).toBe('<img src=x onerror=alert(1)>');
  expect(formatSiteDetailValue('enabled', false)).toBe('false');
  expect(formatSiteDetailValue('retry_count', 0)).toBe('0');
  expect(formatSiteDetailValue('ratio', 0.123456789)).toBe('0.123456789');
  expect(formatSiteDetailValue('missing', null)).toBe('Not available');
  expect(formatSiteDetailValue('empty', '')).toBe('');
  expect(formatSiteDetailValue('metadata', { tags: ['preview', 1], enabled: true })).toBe(JSON.stringify({ tags: ['preview', 1], enabled: true }, null, 2));
  expect(formatSiteDetailValue('tags', [])).toBe('[]');
});

test('filters full URLs, sorts raw values, keeps missing values last and does not mutate the feed', () => {
  const base: SiteRecord = { url: 'https://beta.example/report', datetime: '2026-10-23T10:00:00Z', file_count: 10, total_bytes: 0, duration_ms: null };
  const sites = [base, { ...base, url: 'https://alpha.example/', file_count: 2, datetime: '2026-10-23T12:00:00Z' }, { ...base, file_count: null }];
  expect(selectSites(sites, '', 'file_count', false).map(site => site.file_count)).toEqual([2, 10, null]);
  expect(selectSites(sites, '', 'file_count', true).map(site => site.file_count)).toEqual([10, 2, null]);
  expect(selectSites(sites, ' ALPHA.EXAMPLE ', 'url', false)).toEqual([sites[1]!]);
  expect(selectSites(sites, '/report', 'url', false)).toHaveLength(2);
  expect(selectSites(sites, '', 'datetime', true)[0]).toBe(sites[1]!);
  expect(sites[0]).toBe(base);
});

test('blends metric endpoints, keeps constant columns neutral and includes exact bold boundary', () => {
  expect(metricStyle(0, 0, 100).color).toContain('var(--success)');
  expect(metricStyle(100, 0, 100).color).toContain('var(--danger) 100%');
  expect(metricStyle(75, 0, 100).bold).toBe(true);
  expect(metricStyle(74.99, 0, 100).bold).toBe(false);
  expect(metricStyle(10, 10, 10).color).toContain('var(--danger) 0%');
});