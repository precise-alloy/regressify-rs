# Website Styling

Verified: 2026-11-17 against [tailwind.css](../../website/src/styles/tailwind.css),
[global.css](../../website/src/styles/global.css),
[starlight.css](../../website/src/styles/starlight.css), and the
[styling notes](../../website/README.md#styling).

## Facts

- Tailwind CSS v4 runs through `@tailwindcss/vite`. Only `tailwindcss/utilities.css`
  is imported (no default theme, no Preflight). The theme is
  `@theme inline reference`, mapping Tailwind namespaces onto the token variables
  in tokens.css without emitting variables. `.astro` files have no `<style>`.
- Without `reference`, entries whose name matches a token (`--text-sm`,
  `--radius-md`, `--font-sans`) are emitted as self-referencing `:root` variables
  in the theme layer. tokens.css masks them, but `reference` avoids the cycle.
- Tailwind's `container` utility merges into a same-named site class: a custom
  `@utility container` adds declarations but keeps the per-breakpoint
  `max-width`. The site class is therefore `.page-container`.
- Tailwind sorts arbitrary properties such as `[font:inherit]` after `text-*`,
  `font-*`, and `leading-*`, so a shorthand there resets them; `font-inherit`
  sets only the other longhands.
- `group-open:rotate-90` sets the `rotate` property, not `transform`; use
  `[transform:rotate(90deg)]` where a `transition: transform` must animate it.
- Unlayered rules in starlight.css beat utilities, so its `:focus-visible` ring
  sits in `@layer base`.
- `@astrojs/starlight-tailwind` 5.0.0 is deliberately unused (see the styling notes).
- Scanning is limited to `src/components`, `src/layouts`, `src/pages`, and
  `src/lib`. Starlight's own `print:hidden` class is the only built class that
  matched a generated utility, with the same meaning.

## Verifying a Conversion

- Compare `getComputedStyle` for every element from `<body>` down (plus
  pseudo-elements) per route, theme (`prefers-color-scheme` emulation), and
  width over CDP, for both the old and new build. Reload before each state:
  resting, forced `:hover`, forced `:focus-visible`, and open (`details`, the
  docs menu, the admin dialog with a mocked `/admin/api/sites`). Also build with
  `RELEASE_MANIFEST_FILE` (archives must be `.zip` or `.7z`) to render the
  download page's installer and archive table.
- Ignore `--tw-*` (registered by `@property`), `0px 0px` versus `0% 0%`
  positions, and transition lists that repeat one value.
- Capture the old build twice: forced states are flaky on a few elements, and
  pixel diffs vary inside the desktop screenshots. Computed styles catch invisible
  changes, such as `border-border` coloring all four sides.
- In Git Bash, an environment value starting with `/` is rewritten to a Windows
  path; pass route lists without leading slashes.
