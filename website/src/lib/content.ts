import { getCollection, type CollectionEntry } from 'astro:content';

// Drafts render in `astro dev` for previewing but never ship in production builds.
// Docs are listed and routed by Starlight, which applies the same draft rule.
const includeEntry = ({ data }: { data: { draft: boolean } }) => import.meta.env.DEV || !data.draft;

export async function getPosts(): Promise<CollectionEntry<'blog'>[]> {
  const entries = await getCollection('blog', includeEntry);
  return entries.sort((a, b) => b.data.date.getTime() - a.data.date.getTime());
}

export function formatDate(date: Date): string {
  return date.toLocaleDateString('en-US', { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' });
}
