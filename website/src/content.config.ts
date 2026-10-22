import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';

// Markdown only: no MDX integration is installed, so .mdx files would be
// skipped with just a warning.
//
// Docs: ordered by `order`, then title. The file path (without extension) is the
// URL slug, so `src/content/docs/guides/suites.md` becomes /docs/guides/suites/.
const docs = defineCollection({
  loader: glob({ pattern: '**/[^_]*.md', base: './src/content/docs' }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    order: z.number().int().default(100),
    updated: z.coerce.date().optional(),
    draft: z.boolean().default(false),
  }),
});

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
