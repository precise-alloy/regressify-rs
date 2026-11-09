import type { SiteDetailValue, SiteRecord } from './admin-api';

export const siteColumns = [
  ['url', 'Site'], ['datetime', 'Deployed'], ['file_count', 'Files'],
  ['total_bytes', 'Size (MiB)'], ['duration_ms', 'Duration'],
] as const;
export type SiteColumn = typeof siteColumns[number][0];
const counts = new Intl.NumberFormat('de-DE');
const sizes = new Intl.NumberFormat('en-US', { useGrouping: false, minimumFractionDigits: 1, maximumFractionDigits: 1 });

export function safeSiteUrl(value: string): URL | null {
  try {
    const url = new URL(value);
    return ['https:', 'http:'].includes(url.protocol) && !url.username && !url.password ? url : null;
  } catch {
    return null;
  }
}

export function formatSiteValue(key: SiteColumn, value: string | number | null, now = Date.now()): string {
  if (value === null) return '-';
  if (key === 'datetime' && typeof value === 'string') {
    const timestamp = Date.parse(value);
    if (!Number.isFinite(timestamp)) return value;
    const seconds = Math.floor(Math.abs(now - timestamp) / 1000);
    for (const [unit, size] of [['y', 31_536_000], ['M', 2_592_000], ['d', 86_400], ['h', 3600], ['m', 60]] as const) {
      if (seconds >= size) return `${Math.floor(seconds / size)}${unit}`;
    }
    return `${seconds}s`;
  }
  if (typeof value === 'number') {
    if (key === 'file_count') return counts.format(value);
    if (key === 'total_bytes') return sizes.format(value / 1024 ** 2);
    if (key === 'duration_ms') {
      const total = Math.round(value / 1000);
      const hours = Math.floor(total / 3600);
      const minutes = Math.floor(total / 60) % 60;
      const seconds = String(total % 60).padStart(2, '0');
      return hours ? `${hours}:${String(minutes).padStart(2, '0')}:${seconds}` : `${minutes}:${seconds}`;
    }
  }
  return String(value);
}

export function formatSiteDetailValue(key: string, value: SiteDetailValue): string {
  if (value === null) return 'Not available';
  if (key === 'datetime' && typeof value === 'string') {
    const timestamp = new Date(value);
    return Number.isFinite(timestamp.getTime())
      ? new Intl.DateTimeFormat(undefined, { dateStyle: 'full', timeStyle: 'long' }).format(timestamp)
      : value;
  }
  if (/(?:^|_)bytes$/.test(key) && typeof value === 'number') return `${formatSiteValue('total_bytes', value)} MiB (${counts.format(value)} bytes)`;
  if (/(?:^|_)ms$/.test(key) && typeof value === 'number') return `${formatSiteValue('duration_ms', value)} (${counts.format(value)} ms)`;
  if (typeof value === 'object') return JSON.stringify(value, null, 2);
  if (siteColumns.some(([column]) => column === key) && typeof value !== 'boolean') return formatSiteValue(key as SiteColumn, value);
  return String(value);
}

export function siteDetailEntries(site: SiteRecord): Array<readonly [string, string, SiteDetailValue]> {
  const labels = new Map([
    ['original_bytes', 'Original Size'], ['os', 'OS'], ['os_version', 'OS Version'], ['provider', 'Provider'],
  ]);
  return [
    ...siteColumns.map(([key, label]) => [key, key === 'total_bytes' ? 'Size' : label, site[key]] as const),
    ...Object.entries(site.details ?? {}).map(([key, value]) => [
      key,
      labels.get(key) ?? key.replace(/[_\s.-]+/g, ' ').replace(/\b\w/g, character => character.toUpperCase()),
      value,
    ] as const),
  ];
}

export function selectSites(sites: SiteRecord[], search: string, key: SiteColumn, descending: boolean): SiteRecord[] {
  const query = search.trim().toLowerCase();
  return sites.filter(site => site.url.toLowerCase().includes(query)).sort((left, right) => {
    const first = left[key];
    const second = right[key];
    if (first === null) return second === null ? 0 : 1;
    if (second === null) return -1;
    const order = key === 'datetime' ? Date.parse(String(first)) - Date.parse(String(second)) :
      typeof first === 'number' && typeof second === 'number' ? first - second : String(first).localeCompare(String(second));
    return descending ? -order : order;
  });
}

export function metricStyle(value: number, min: number, max: number): { color: string; bold: boolean } {
  const position = max > min ? (value - min) / (max - min) : 0.5;
  return {
    color: position < 0.5
      ? `color-mix(in srgb, var(--success), var(--foreground) ${position * 200}%)`
      : `color-mix(in srgb, var(--foreground), var(--danger) ${(position - 0.5) * 200}%)`,
    bold: value >= max * 0.75,
  };
}

export function mountAdmin(): void {
  const root = document.querySelector<HTMLElement>('[data-admin]');
  if (!root) return;
  const search = root.querySelector<HTMLInputElement>('#site-search')!;
  const refresh = root.querySelector<HTMLButtonElement>('#refresh-sites')!;
  const automatic = root.querySelector<HTMLInputElement>('#auto-refresh')!;
  const status = root.querySelector<HTMLElement>('#feed-status')!;
  const count = root.querySelector<HTMLElement>('#site-count')!;
  const table = root.querySelector<HTMLTableElement>('table')!;
  const body = table.tBodies[0]!;
  const empty = root.querySelector<HTMLElement>('#empty-sites')!;
  const partial = root.querySelector<HTMLElement>('#partial-sites')!;
  const detailsTemplate = root.querySelector<HTMLTemplateElement>('#site-details-button')!;
  const dialog = root.querySelector<HTMLDialogElement>('#site-details')!;
  const detailFields = root.querySelector<HTMLElement>('#site-detail-fields')!;
  let sites: SiteRecord[] = [];
  let selectedSites: SiteRecord[] = [];
  let detailsSite: SiteRecord | null = null;
  let detailsTrigger: HTMLButtonElement | null = null;
  let sortKey: SiteColumn = 'datetime';
  let descending = true;
  let loading = false;
  let loaded = false;
  let lastUpdated: string | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function restoreDetailsFocus(site: SiteRecord): void {
    const index = selectedSites.findIndex(candidate => candidate.url === site.url && candidate.datetime === site.datetime);
    const button = body.querySelector<HTMLButtonElement>(`[data-site-details="${index}"]`);
    (button ?? search).focus({ preventScroll: true });
  }

  function render(): void {
    const focusedButton = document.activeElement?.closest<HTMLButtonElement>('[data-site-details]');
    const focusedSite = focusedButton ? selectedSites[Number(focusedButton.dataset.siteDetails)] : undefined;
    const selected = selectSites(sites, search.value, sortKey, descending);
    selectedSites = selected;
    count.textContent = search.value.trim() ? `${selected.length} of ${sites.length} sites` : `${sites.length} sites`;
    const ranges = new Map<SiteColumn, { min: number; max: number }>();
    for (const key of ['file_count', 'total_bytes', 'duration_ms'] as const) {
      let min = Infinity;
      let max = -Infinity;
      for (const site of sites) {
        const value = site[key];
        if (value !== null) { min = Math.min(min, value); max = Math.max(max, value); }
      }
      ranges.set(key, { min, max });
    }
    const fragment = document.createDocumentFragment();
    for (const [index, site] of selected.entries()) {
      const row = document.createElement('tr');
      const number = row.insertCell();
      number.className = 'row-index metric';
      number.textContent = String(index + 1);
      for (const [key] of siteColumns) {
        const cell = row.insertCell();
        cell.dataset.column = key;
        const value = site[key];
        cell.textContent = formatSiteValue(key, value);
        if (key === 'url') {
          const url = safeSiteUrl(site.url);
          if (url) {
            const link = document.createElement('a');
            link.href = url.href;
            link.target = '_blank';
            link.rel = 'noopener noreferrer';
            link.textContent = `${url.host}${url.pathname === '/' ? '' : url.pathname}${url.search}${url.hash}`;
            link.title = url.href;
            cell.replaceChildren(link);
          }
          cell.dir = 'ltr';
        } else {
          cell.className = 'metric';
          if (key === 'datetime' && typeof value === 'string') cell.title = new Date(value).toLocaleString();
          const range = ranges.get(key);
          if (range && typeof value === 'number') {
            const style = metricStyle(value, range.min, range.max);
            cell.style.color = style.color;
            if (style.bold) {
              const strong = document.createElement('strong');
              strong.textContent = cell.textContent;
              cell.replaceChildren(strong);
            }
          }
        }
      }
      const actions = row.insertCell();
      actions.className = 'details-column';
      const button = detailsTemplate.content.firstElementChild!.cloneNode(true) as HTMLButtonElement;
      button.dataset.siteDetails = String(index);
      button.setAttribute('aria-label', `Deployment details for ${site.url}`);
      actions.append(button);
      fragment.append(row);
    }
    body.replaceChildren(fragment);
    if (focusedSite && !dialog.open) restoreDetailsFocus(focusedSite);
    empty.hidden = !loaded || selected.length !== 0;
    empty.textContent = sites.length ? 'No sites match your search.' : 'No sites available.';
    for (const button of root!.querySelectorAll<HTMLButtonElement>('[data-sort]')) {
      const active = button.dataset.sort === sortKey;
      button.closest('th')!.setAttribute('aria-sort', active ? (descending ? 'descending' : 'ascending') : 'none');
    }
  }

  body.addEventListener('click', event => {
    const button = event.target instanceof Element ? event.target.closest<HTMLButtonElement>('[data-site-details]') : null;
    if (!button) return;
    const site = selectedSites[Number(button.dataset.siteDetails)];
    if (!site) return;
    detailsTrigger = button;
    detailsSite = site;
    const fragment = document.createDocumentFragment();
    for (const [key, label, value] of siteDetailEntries(site)) {
      const term = document.createElement('dt');
      term.textContent = label;
      const description = document.createElement('dd');
      description.textContent = formatSiteDetailValue(key, value);
      if (value !== null && typeof value === 'object') description.className = 'detail-json';
      if (key === 'url') {
        description.dir = 'ltr';
        const url = safeSiteUrl(site.url);
        if (url) {
          const link = document.createElement('a');
          link.href = url.href;
          link.target = '_blank';
          link.rel = 'noopener noreferrer';
          link.textContent = site.url;
          description.replaceChildren(link);
        }
      }
      fragment.append(term, description);
    }
    detailFields.replaceChildren(fragment);
    dialog.showModal();
  });
  root.querySelector<HTMLButtonElement>('#close-site-details')!.addEventListener('click', () => dialog.close());
  dialog.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      event.preventDefault();
      dialog.close();
    }
  });
  dialog.addEventListener('close', () => {
    if (detailsTrigger?.isConnected) detailsTrigger.focus({ preventScroll: true });
    else if (detailsSite) restoreDetailsFocus(detailsSite);
    detailsTrigger = null;
    detailsSite = null;
  });

  function schedule(): void {
    clearTimeout(timer);
    if (automatic.checked && !document.hidden) timer = setTimeout(() => void load(), 60_000);
  }

  async function load(): Promise<void> {
    if (loading) return;
    loading = true;
    refresh.disabled = true;
    table.setAttribute('aria-busy', 'true');
    status.textContent = loaded ? 'Refreshing...' : 'Loading sites...';
    status.dataset.error = 'false';
    try {
      const response = await fetch('/admin/api/sites', {
        credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: AbortSignal.timeout(20_000),
      });
      if (!response.ok) throw new Error('Feed failed');
      const data = await response.json();
      if (!Array.isArray(data.sites) || !Number.isFinite(Date.parse(data.fetchedAt)) || typeof data.hasMore !== 'boolean') {
        throw new Error('Invalid feed');
      }
      sites = data.sites;
      loaded = true;
      lastUpdated = new Date(data.fetchedAt).toLocaleTimeString();
      partial.hidden = !data.hasMore;
      status.textContent = `Updated ${lastUpdated}`;
      render();
    } catch {
      status.dataset.error = 'true';
      status.textContent = loaded
        ? `Refresh failed. Showing results from ${lastUpdated}.`
        : 'Unable to load sites. Refresh to retry. Your access session may have expired.';
    } finally {
      loading = false;
      refresh.disabled = false;
      table.removeAttribute('aria-busy');
      schedule();
    }
  }

  search.addEventListener('input', render);
  refresh.addEventListener('click', () => void load());
  automatic.addEventListener('change', schedule);
  document.addEventListener('visibilitychange', schedule);
  for (const button of root.querySelectorAll<HTMLButtonElement>('[data-sort]')) {
    button.addEventListener('click', () => {
      const key = button.dataset.sort as SiteColumn;
      descending = key === sortKey ? !descending : key !== 'url';
      sortKey = key;
      render();
    });
  }
  render();
  void load();
}