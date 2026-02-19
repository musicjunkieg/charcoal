# Charcoal Design Language

A self-contained specification for implementing the Charcoal visual identity across any project, page, or component. This document provides everything needed to produce visually consistent work — no access to the original landing page source is required.

## Brand Philosophy

The name **Charcoal** evokes activated charcoal absorbing toxins. The visual language should feel **warm, grounded, and safe** — never cold, clinical, or aggressive. The concentric-circles logo represents absorption. The warm palette represents the calm that remains after harmful content is filtered away.

---

## Design Tokens (CSS Custom Properties)

Copy-paste this block into your global stylesheet or component `<style>` block:

```css
:root {
  /* Warm charcoal palette */
  --charcoal-950: #0c0a09;
  --charcoal-900: #1c1917;
  --charcoal-800: #292524;
  --charcoal-700: #44403c;
  --charcoal-600: #57534e;
  --charcoal-500: #78716c;
  --charcoal-400: #a8a29e;
  --charcoal-300: #d6d3d1;

  /* Warm cream tones */
  --cream-50: #fffbeb;
  --cream-100: #fef3c7;

  /* Amber accent */
  --amber-500: #f59e0b;
  --amber-600: #d97706;

  /* Copper accent */
  --copper: #c9956c;
  --copper-glow: rgba(201, 149, 108, 0.3);

  /* Typography */
  --font-display: 'Libre Baskerville', Georgia, serif;
  --font-body: 'Outfit', system-ui, sans-serif;

  /* Easing */
  --ease-out-expo: cubic-bezier(0.16, 1, 0.3, 1);
  --ease-in-out: cubic-bezier(0.4, 0, 0.2, 1);
}
```

---

## Color Palette

The palette is built around **warm charcoal** tones with **cream** text and **copper/amber** accents.

### Charcoal (backgrounds, surfaces, muted text)

| Token              | Hex       | Usage                                    |
|--------------------|-----------|------------------------------------------|
| `--charcoal-950`   | `#0c0a09` | Deepest background, page base            |
| `--charcoal-900`   | `#1c1917` | Primary background gradient stop         |
| `--charcoal-800`   | `#292524` | Card/surface backgrounds (with opacity)  |
| `--charcoal-700`   | `#44403c` | Rarely used directly                     |
| `--charcoal-600`   | `#57534e` | Muted footer text                        |
| `--charcoal-500`   | `#78716c` | Secondary muted text, hints              |
| `--charcoal-400`   | `#a8a29e` | Body text, descriptions                  |
| `--charcoal-300`   | `#d6d3d1` | Brighter body text, nav links            |

### Cream (primary text)

| Token          | Hex       | Usage                          |
|----------------|-----------|--------------------------------|
| `--cream-50`   | `#fffbeb` | Headings, primary text, hovers |
| `--cream-100`  | `#fef3c7` | Default page text color        |

### Accent Colors

| Token          | Hex                        | Usage                                    |
|----------------|----------------------------|------------------------------------------|
| `--copper`     | `#c9956c`                  | Primary accent — logo, icons, eyebrow text, borders on hover |
| `--copper-glow`| `rgba(201, 149, 108, 0.3)` | Glow/shadow effects on cards             |
| `--amber-500`  | `#f59e0b`                  | CTA buttons (gradient start)             |
| `--amber-600`  | `#d97706`                  | Background warmth radials                |

### Hard Rules
- **Never use pure white** (`#fff`) for text. Use `--cream-50` or `--cream-100`.
- **Never use pure black** (`#000`) for backgrounds. Use `--charcoal-950`.
- Borders are always `rgba(168, 162, 158, ...)` at low opacity (0.08–0.2).
- Hover states shift border color toward copper: `rgba(201, 149, 108, 0.2–0.4)`.

---

## Typography

Two font families, loaded from Google Fonts:

```html
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
<link href="https://fonts.googleapis.com/css2?family=Libre+Baskerville:ital,wght@0,400;0,700;1,400&family=Outfit:wght@300;400;500;600&display=swap" rel="stylesheet" />
```

### Display Font: Libre Baskerville
- **Usage**: All headings (`h1`–`h3`), section titles, testimonial quotes, brand name
- **Weights**: 400 (regular) is the primary weight; 700 (bold) sparingly; italic for accent words
- **Character**: Elegant serif that conveys trust and sophistication
- **Sizing**: Use `clamp()` for fluid scaling
  - Hero title: `clamp(2.5rem, 8vw, 4.5rem)`
  - Section titles: `clamp(1.75rem, 4vw, 2.5rem)`
  - Card titles: `1.125rem` – `1.25rem`
  - Brand name (e.g. login header): `2.25rem`

### Body Font: Outfit
- **Usage**: All body text, navigation, buttons, descriptions, form labels
- **Weights**: 300 (light) for descriptions and body; 400 for default; 500 for nav/buttons/labels; 600 sparingly
- **Character**: Clean geometric sans-serif that balances the serif display font
- **Sizing**:
  - Body text: `0.9375rem` – `1.125rem`
  - Small text (hints, footer): `0.8125rem` – `0.875rem`
  - Buttons/CTAs: `1.0625rem`

### Typography Rules
- Line height for body text: `1.6` – `1.7`
- Line height for headings: `1.1`
- Letter spacing on eyebrow/label text: `0.15em` with `text-transform: uppercase`
- Form labels: `0.03em` letter-spacing, `text-transform: uppercase`, `font-weight: 500`
- Always set `-webkit-font-smoothing: antialiased` and `-moz-osx-font-smoothing: grayscale` on the page container
- The **accent word** in hero headings uses `font-style: italic` and `color: var(--copper)`

---

## Background Treatment

Every page uses a layered background system. All layers are `position: fixed` with negative `z-index`:

### Layer 1: Base Gradient (`z-index: -3`)
```css
background: linear-gradient(165deg, var(--charcoal-900) 0%, var(--charcoal-950) 50%, #0a0705 100%);
```

### Layer 2: Warmth Radials (`z-index: -2`)
Three overlapping radial gradients that add subtle warm color:
```css
background:
  radial-gradient(ellipse 100% 80% at 50% 20%, rgba(201, 149, 108, 0.08) 0%, transparent 60%),
  radial-gradient(ellipse 80% 60% at 20% 80%, rgba(245, 158, 11, 0.05) 0%, transparent 50%),
  radial-gradient(ellipse 60% 50% at 90% 50%, rgba(217, 119, 6, 0.04) 0%, transparent 40%);
pointer-events: none;
```

### Layer 3: Ambient Orbs (`z-index: -1`)
Two to three large, blurred circles that drift slowly:
- 400–600px diameter
- `filter: blur(80px–100px)`
- Radial gradients using copper/amber at 0.06–0.15 opacity
- Animated with a 25–30s `ease-in-out` drift keyframe (translate ±30-40px + scale 0.95–1.05)
- Use `aria-hidden="true"` on the container

**Rule**: Background warmth should be barely perceptible — it creates atmosphere without drawing attention.

---

## Surface & Card Patterns

All cards and surfaces use this formula:

```css
background: linear-gradient(145deg, rgba(41, 37, 36, 0.4–0.8) 0%, rgba(28, 25, 23, 0.5–0.9) 100%);
border: 1px solid rgba(168, 162, 158, 0.08–0.15);
border-radius: 16px–24px;
```

### Card Tiers
| Context             | Border radius | Border opacity | Background opacity | Extra                        |
|---------------------|---------------|----------------|--------------------|------------------------------|
| Standard card       | `16px`        | `0.08`         | `0.4` / `0.5`     | —                            |
| Pipeline/step card  | `20px`        | `0.1`          | `0.5` / `0.6`     | —                            |
| Featured/CTA card   | `24px`        | `0.15`         | `0.7` / `0.8`     | copper glow box-shadow       |
| Login/form card     | `20px`        | `0.1`          | `0.8` / `0.9`     | `backdrop-filter: blur(20px)`, copper glow |

### Featured Card Box Shadow
```css
box-shadow:
  0 0 0 1px rgba(0, 0, 0, 0.2–0.3),
  0 20px 50px -10px rgba(0, 0, 0, 0.5),
  0 0 80px -20px var(--copper-glow);
```

### Hover States
```css
border-color: rgba(201, 149, 108, 0.15–0.2);
transform: translateY(-2px to -4px);
box-shadow: 0 20px 40px -10px rgba(0, 0, 0, 0.4);  /* for deeper cards */
transition: all 0.4s var(--ease-out-expo);
```

---

## Buttons & CTAs

### Primary CTA (amber gradient)
```css
color: var(--charcoal-950);  /* dark text on bright background */
background: linear-gradient(135deg, var(--amber-500) 0%, var(--copper) 100%);
border: none;
border-radius: 12px;
padding: 1rem 2rem;
font-family: var(--font-body);
font-size: 1.0625rem;
font-weight: 500;
cursor: pointer;
box-shadow: 0 4px 20px -4px rgba(245, 158, 11, 0.4);
transition: all 0.4s var(--ease-out-expo);
```

**Hover**: `transform: translateY(-3px)`, shadow intensifies to `0 8px 30px -4px rgba(245, 158, 11, 0.5)`.

**Active**: `transform: translateY(0)`.

**Disabled**: `opacity: 0.4; cursor: not-allowed; transform: none`.

### Secondary / Ghost Button
```css
color: var(--charcoal-300);
background: transparent;
border: 1px solid rgba(168, 162, 158, 0.2);
border-radius: 8px;
padding: 0.625rem 1.25rem;
font-size: 0.9375rem;
font-weight: 500;
transition: all 0.3s var(--ease-in-out);
```

**Hover**: text to `--cream-50`, border to `rgba(201, 149, 108, 0.4)`, background `rgba(201, 149, 108, 0.1)`.

---

## Form Elements

### Text Inputs
```css
/* Container wrapping the input */
display: flex;
align-items: center;
background: rgba(12, 10, 9, 0.6);
border: 1px solid rgba(168, 162, 158, 0.15);
border-radius: 12px;
padding: 0 1rem;
transition: all 0.3s var(--ease-in-out);

/* Focused state */
border-color: var(--copper);
background: rgba(12, 10, 9, 0.8);
box-shadow: 0 0 0 3px rgba(201, 149, 108, 0.15);
```

### Input Text
```css
border: none;
background: transparent;
padding: 1rem 0;
font-size: 1rem;
font-family: var(--font-body);
font-weight: 400;
color: var(--cream-100);
outline: none;

/* Placeholder */
color: var(--charcoal-600);
```

### Labels
```css
font-size: 0.8125rem;
font-weight: 500;
color: var(--charcoal-300);
letter-spacing: 0.03em;
text-transform: uppercase;
margin-bottom: 0.625rem;
```

### Prefix Symbols (e.g., "@" for handles)
- Color: `--charcoal-500`, transitions to `--copper` on focus
- Font size: `1rem`, weight 400

---

## Logo / Icon System

The Charcoal logo is a set of concentric circles rendered as inline SVG:

```svg
<svg viewBox="0 0 64 64" fill="none">
  <circle cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1" opacity="0.15" />
  <circle cx="32" cy="32" r="26" stroke="currentColor" stroke-width="1" opacity="0.25" />
  <circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.35" />
  <circle cx="32" cy="32" r="18" stroke="currentColor" stroke-width="1.5" opacity="0.5" />
  <circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.7" />
  <circle cx="32" cy="32" r="6" fill="currentColor" />
</svg>
```

The number of rings can be reduced for smaller contexts. Minimum representation is 2 stroked rings + filled core.

### Icon Rules
- All icons use `currentColor` so they inherit the parent's `color` property (usually `--copper`)
- Icons are SVG with `viewBox`, no fixed width/height attributes — sized via CSS
- Stroke icons use `stroke-linecap="round"` and `stroke-linejoin="round"`
- Icon containers are sized explicitly with `width` + `height` in CSS
- The concentric-circles motif should appear wherever the brand mark is needed

### Icon Sizing Reference
| Context       | Size   | Mobile    |
|---------------|--------|-----------|
| Hero logo     | `120px`| `90px`    |
| CTA logo      | `72px` | —         |
| Login logo    | `72px` | `64px`    |
| Step icons    | `64px` | —         |
| Benefit icons | `48px` | —         |
| Nav logo      | `36px` | —         |
| Footer logo   | `32px` | —         |

---

## Animation & Motion

All animations use two custom easing curves:
- `--ease-out-expo: cubic-bezier(0.16, 1, 0.3, 1)` — entrances, transforms, card hovers
- `--ease-in-out: cubic-bezier(0.4, 0, 0.2, 1)` — subtle hover transitions (color, border)

### Entrance: `emerge`
```css
@keyframes emerge {
  from { opacity: 0; transform: translateY(24px–40px); }
  to   { opacity: 1; transform: translateY(0); }
}
/* Usage: animation: emerge 1s–1.2s var(--ease-out-expo) forwards; */
/* Stagger child elements with animation-delay increments of 0.05s–0.1s */
/* Use 'backwards' fill mode for delayed elements: animation: emerge 1s var(--ease-out-expo) 0.2s backwards; */
```

### Entrance: Staggered Words
Each word in a hero heading gets its own `<span>` with incrementing `animation-delay` (0.1s apart).

### Scroll Reveal (sections below the fold)
- Initial state: `opacity: 0; transform: translateY(60px)`
- Visible state: `opacity: 1; transform: translateY(0)`
- Transition: `opacity 0.8s var(--ease-out-expo), transform 0.8s var(--ease-out-expo)`
- Trigger: IntersectionObserver with `threshold: 0.15`, `rootMargin: '0px 0px -50px 0px'`

### Ambient: Logo Rings
```css
@keyframes ring-pulse {
  0%, 100% { transform: scale(1); opacity: <base>; }
  50%      { transform: scale(1.02–1.03); opacity: <base + 0.2>; }
}
/* 4–5s cycle, stagger each ring by 0.3–0.5s */
```

### Ambient: Logo Core Breathing
```css
@keyframes core-breathe {
  0%, 100% { transform: scale(1); }
  50%      { transform: scale(0.9–0.95); }
}
/* 4–5s cycle */
```

### Ambient: Drifting Orbs
```css
@keyframes drift {
  0%, 100% { transform: translate(0, 0) scale(1); }
  33%      { transform: translate(30px–40px, -20px–-30px) scale(1.05); }
  66%      { transform: translate(-20px–-30px, 30px–40px) scale(0.95); }
}
/* 25–30s cycle, stagger with negative animation-delay */
```

### Transition Defaults
| Context            | Transition                            |
|--------------------|---------------------------------------|
| Card/surface hover | `all 0.4s var(--ease-out-expo)`       |
| Nav link hover     | `0.3s var(--ease-in-out)`             |
| Color-only         | `color 0.3s var(--ease-in-out)`       |
| Input focus        | `all 0.3s var(--ease-in-out)`         |
| Button hover       | `all 0.3s–0.4s var(--ease-out-expo)`  |

### Reduced Motion (mandatory)
Every page must include:
```css
@media (prefers-reduced-motion: reduce) {
  /* Kill all animations */
  .orb, [data-animate], .ring, .core, .hero-content, .scroll-line,
  .particle, .cta-arrow, .title-word, .content, .logo, .card, .footer {
    animation: none;
  }
  /* Make scroll-reveal sections visible */
  .section, [data-animate] {
    opacity: 1;
    transform: none;
  }
  /* Kill hover transitions */
  .card, .btn, .input-container, button, a {
    transition: none;
  }
}
```

---

## Layout & Spacing

### Page Structure
- Max content width: `1100px`, centered with `margin: 0 auto`
- Section padding: `8rem 2rem` (mobile: `5rem 1.5rem`)
- Hero: Full viewport height (`min-height: 100dvh`), flexbox centered
- Navigation: Fixed top, `padding: 1.5rem 2rem`, gradient fade background:
  ```css
  background: linear-gradient(to bottom, rgba(12, 10, 9, 0.8) 0%, transparent 100%);
  ```
- Centered single-column pages (login): `max-width: 400px`, viewport centered

### Grid Patterns
- 3-column content: `grid-template-columns: repeat(auto-fit, minmax(280px, 1fr))`, gap `2rem`
- Card grids: Same pattern, `minmax(300px, 1fr)` for wider cards
- Pipeline/steps: Flexbox row with SVG connectors between items, wraps to column on mobile

### Card Padding
| Context            | Padding                    |
|--------------------|----------------------------|
| Standard card      | `2rem`                     |
| Featured/CTA card  | `3rem` (mobile: `2rem 1.5rem`) |
| Pipeline step      | `2rem 1.5rem`              |
| Login form card    | `2rem` (mobile: `1.5rem`)  |

---

## Responsive Breakpoints

Two breakpoints:

### `max-width: 768px`
- Reduce nav/hero padding
- Hero logo shrinks (120px → 90px)
- Sections reduce to `5rem 1.5rem` padding
- Pipeline/step layouts go vertical (flex-direction column, connectors rotate 90deg)
- CTA card padding reduces

### `max-width: 480px`
- CTA buttons and submit buttons go full width
- Login logo shrinks (72px → 64px)
- Further padding reductions on cards and containers

---

## Accessibility

- All decorative elements (background layers, orbs, logo decorations) use `aria-hidden="true"`
- Interactive elements have visible focus states (copper-tinted outlines or browser defaults)
- Navigation uses semantic `<nav>`
- Sections use semantic `<section>` with `id` attributes for anchor links
- SVG logos on links include `aria-label` on the link element
- `prefers-reduced-motion` is always respected (see Animation section)
- Form inputs include proper `<label>` elements with `for` attributes
- Disabled states use `opacity: 0.4–0.5` and `cursor: not-allowed`

---

## Quick Reference: Implementing a New Page

1. **Declare tokens**: Paste the CSS custom properties block into your component or global stylesheet.
2. **Load fonts**: Include the Google Fonts `<link>` tags (with `preconnect`) in the document head.
3. **Set the background**: Apply the three-layer background system (base gradient, warmth radials, ambient orbs).
4. **Set the page container**: `font-family: var(--font-body); color: var(--cream-100); -webkit-font-smoothing: antialiased;`
5. **Use the surface formula** for any cards or elevated content areas.
6. **Use Libre Baskerville** (`--font-display`) for all headings; **Outfit** (`--font-body`) for everything else.
7. **Color text** with cream tones (`--cream-50` for headings, `--cream-100` for body); use `--charcoal-300` to `--charcoal-400` for descriptions.
8. **Accent with copper** — icons, eyebrow text, hover borders, the logo.
9. **CTA buttons** use the amber-to-copper gradient with dark (`--charcoal-950`) text.
10. **Add the `emerge` entrance** animation for above-fold content; use scroll-reveal for below-fold sections.
11. **Always include** the `prefers-reduced-motion` media query.
12. **Form elements** use the input container pattern with copper focus ring.
