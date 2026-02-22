# Charcoal Landing Page

The public-facing landing page for Charcoal — a tool that absorbs harmful engagement on Bluesky so you can engage with confidence.

## Getting Started

```bash
nvm use              # Switch to Node v24
npm install          # Install dependencies
npm run dev          # Start dev server at http://localhost:5173
```

## Building

### Standard build (uses adapter-auto)

```bash
npm run build
npm run preview      # Preview the build locally
```

### Static export (for static hosting)

```bash
npm run build:static
```

Output goes to `build-static/` with pre-compressed `.gz` and `.br` files, ready for deployment to any static host (Cloudflare Pages, Netlify, Vercel, etc.).

## Design Language

See [docs/design-language.md](docs/design-language.md) for the complete Charcoal design system specification.

## Structure

```
src/
  routes/
    +page.svelte          # Landing page
    +page.ts              # Prerender config
    +layout.svelte        # Root layout
    auth/login/
      +page.svelte        # Login page (design reference)
      +page.server.ts     # Stub login action (replace with real OAuth)
  lib/
    assets/               # Brand assets (favicon, profile, banner)
    website/styles/
      tokens.css          # Extracted CSS design tokens
```
