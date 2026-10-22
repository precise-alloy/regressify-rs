import { getCollection, type CollectionEntry } from 'astro:content';

// Drafts render in `astro dev` for previewing but never ship in production builds.
const includeEntry = ({ data }: { data: { draft: boolean } }) => import.meta.env.DEV || !data.draft;

export async function getDocs(): Promise<CollectionEntry<'docs'>[]> {
  const entries = await getCollection('docs', includeEntry);
  return entries.sort((a, b) => a.data.order - b.data.order || a.data.title.localeCompare(b.data.title));
}

export async function getPosts(): Promise<CollectionEntry<'blog'>[]> {
  const entries = await getCollection('blog', includeEntry);
  return entries.sort((a, b) => b.data.date.getTime() - a.data.date.getTime());
}

export function formatDate(date: Date): string {
  return date.toLocaleDateString('en-US', { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' });
}
