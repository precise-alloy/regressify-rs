import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

// Docs: rendered by Starlight, which requires this collection to be named `docs`
// and loads every Markdown/MDX file under src/content/docs. Starlight routes each
// entry at the site root by its file path, so pages live under an extra `docs/`
// folder to keep their /docs/... URLs: `src/content/docs/docs/desktop-app/running-tests.md`
// becomes /docs/desktop-app/running-tests/. Sidebar order and labels come from the
// `sidebar` front matter and the `sidebar` config in astro.config.mjs.
const docs = defineCollection({
  loader: docsLoader(),
  // Starlight makes `description` optional; the site requires one for every page
  // because it feeds the meta description and search snippets.
  schema: docsSchema({ extend: z.object({ description: z.string() }) }),
});

// Blog: Markdown only. The file path (without extension) is the URL slug.
const blog = defineCollection({
  loader: glob({ pattern: '**/[^_]*.md', base: './src/content/blog' }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    date: z.coerce.date(),
    updated: z.coerce.date().optional(),
    author: z.string().optional(),
    draft: z.boolean().default(false),
  }),
});

export const collections = { docs, blog };
