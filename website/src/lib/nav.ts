/**
 * Whether a header link points at the current section: the exact page or any
 * page below it, ignoring trailing slashes. `/docs/` is active on every docs
 * page, while `/` would be active everywhere, so callers only pass section links.
 */
export function isActivePath(pathname: string, href: string): boolean {
  const base = href.replace(/\/$/, '');
  return pathname.replace(/\/$/, '') === base || pathname.startsWith(`${base}/`);
}
