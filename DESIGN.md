---
name: Charcoal
description: Warm, watchful threat detection for Bluesky — a lit room that keeps watch in the dark.
colors:
  reading-copper: "#c9956c"
  alert-amber: "#f59e0b"
  alert-amber-deep: "#d97706"
  night-ground: "#0c0a09"
  dim-ground: "#1c1917"
  surface: "#292524"
  surface-raised: "#44403c"
  muted-deep: "#57534e"
  muted: "#78716c"
  body-text: "#a8a29e"
  body-text-bright: "#d6d3d1"
  instrument-cream: "#fef3c7"
  lit-cream: "#fffbeb"
  tier-high: "#fca5a5"
  tier-elevated: "#fdba74"
  tier-watch: "#fcd34d"
  tier-low: "#a8a29e"
typography:
  display:
    fontFamily: "Libre Baskerville, Georgia, serif"
    fontSize: "clamp(2.5rem, 8vw, 4.5rem)"
    fontWeight: 400
    lineHeight: 1.1
  headline:
    fontFamily: "Libre Baskerville, Georgia, serif"
    fontSize: "clamp(1.75rem, 4vw, 2.5rem)"
    fontWeight: 400
    lineHeight: 1.1
  title:
    fontFamily: "Libre Baskerville, Georgia, serif"
    fontSize: "1.25rem"
    fontWeight: 400
    lineHeight: 1.3
  body:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "1rem"
    fontWeight: 300
    lineHeight: 1.65
  label:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "0.8125rem"
    fontWeight: 500
    letterSpacing: "0.03em"
  eyebrow:
    fontFamily: "Outfit, system-ui, sans-serif"
    fontSize: "0.8125rem"
    fontWeight: 500
    letterSpacing: "0.15em"
rounded:
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
  2xl: "24px"
spacing:
  xs: "0.625rem"
  sm: "1rem"
  md: "1.5rem"
  lg: "2rem"
  xl: "3rem"
  section: "8rem"
components:
  button-primary:
    backgroundColor: "{colors.alert-amber}"
    textColor: "{colors.night-ground}"
    rounded: "{rounded.md}"
    padding: "1rem 2rem"
  button-primary-hover:
    backgroundColor: "{colors.alert-amber}"
    textColor: "{colors.night-ground}"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.body-text-bright}"
    rounded: "{rounded.sm}"
    padding: "0.625rem 1.25rem"
  button-ghost-hover:
    backgroundColor: "rgba(201, 149, 108, 0.1)"
    textColor: "{colors.lit-cream}"
  card-standard:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.instrument-cream}"
    rounded: "{rounded.lg}"
    padding: "2rem"
  card-featured:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.instrument-cream}"
    rounded: "{rounded.2xl}"
    padding: "3rem"
  input-field:
    backgroundColor: "rgba(12, 10, 9, 0.6)"
    textColor: "{colors.instrument-cream}"
    rounded: "{rounded.md}"
    padding: "1rem"
  input-field-focus:
    backgroundColor: "rgba(12, 10, 9, 0.8)"
    textColor: "{colors.instrument-cream}"
  tier-badge-high:
    backgroundColor: "transparent"
    textColor: "{colors.tier-high}"
    typography: "{typography.label}"
  tier-badge-abstained:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    typography: "{typography.label}"
---

# Design System: Charcoal

## Overview

**Creative North Star: "The Hearth Watch"**

Someone is awake, watching the door, and the fire is still lit. Charcoal names
accounts likely to harass the person using it — subject matter that pulls hard
toward the security-console idiom of neon-on-black, red alert bars and dense
telemetry. This system refuses that pull deliberately. The ground is a warm
near-black, the text is cream rather than white, the accents are copper and
amber, and the threat tiers are desaturated so that even a High finding is
legible rather than alarming.

The warmth is not decoration; it is the argument. The product name evokes
activated charcoal absorbing toxins, and the concentric-ring mark reads as
absorption and as a sweep at once. What remains after the harmful content is
filtered is supposed to feel like calm, so the interface behaves like a lit room
rather than an operations center. Depth is atmospheric and layered — a base
gradient, warmth radials, and slow drifting orbs, all of it barely perceptible
and none of it competing with the reading.

Components behave as **calm instruments**: precise, unhurried, responsive
without demanding. The interface never raises its voice, even when the finding
is High. That restraint is what makes the findings credible — a system that
shouts about everything cannot be trusted when something is genuinely wrong.

**Key Characteristics:**
- Warm near-black ground (`#0c0a09`); never pure black, never pure white text
- Serif display (Libre Baskerville) against geometric sans body (Outfit)
- Copper as the single recurring accent; amber reserved for action
- Desaturated threat tiers — soft red, not alarm red
- Atmosphere from layered gradients and 25–30s ambient drift, never from chrome
- Motion that reveals, never bounces; `prefers-reduced-motion` honored throughout

## Colors

A warm charcoal ground carrying cream text, with copper for identity and amber
reserved for action — an observational palette, named for a room where readings
are taken.

### Primary
- **Reading Copper** (`#c9956c`): The brand's single recurring accent. Logo,
  icons, eyebrow text, focus rings, and the border color surfaces shift toward
  on hover. It marks *identity and attention*, never state or severity.
- **Alert Amber** (`#f59e0b`): Reserved for action. The primary CTA gradient
  starts here. Amber appearing anywhere other than a call to action dilutes it.
- **Alert Amber Deep** (`#d97706`): Background warmth radials only — never text,
  never a border.

### Neutral
- **Night Ground** (`#0c0a09`): The deepest background and page base.
- **Dim Ground** (`#1c1917`): The primary background gradient stop.
- **Surface** (`#292524`): Card and panel backgrounds, always at partial opacity
  over the ground rather than as a flat fill.
- **Surface Raised** (`#44403c`): Present in the scale; rarely used directly.
- **Muted Deep** (`#57534e`): Footer text and input placeholders.
- **Muted** (`#78716c`): Secondary text and hints.
- **Body Text** (`#a8a29e`): Descriptions and running body copy.
- **Body Text Bright** (`#d6d3d1`): Navigation links and emphasized body copy.
- **Instrument Cream** (`#fef3c7`): The default page text color.
- **Lit Cream** (`#fffbeb`): Headings, primary text, and hover states.

### Tertiary — Threat Tiers
Deliberately desaturated. These encode severity without ever reading as an
alarm.
- **Tier High** (`#fca5a5`): A soft red. The most severe finding the system
  reports.
- **Tier Elevated** (`#fdba74`): Soft orange.
- **Tier Watch** (`#fcd34d`): Soft yellow.
- **Tier Low** (`#a8a29e`): Neutral. A real reading that found little.

### Named Rules

**The No-Pure Rule.** Never `#fff` for text and never `#000` for background.
Cream (`#fffbeb` / `#fef3c7`) and Night Ground (`#0c0a09`) are the extremes.
Pure values break the warmth that the entire identity rests on.

**The Copper-Is-Not-Status Rule.** Copper marks identity and attention. Amber
marks action. Neither ever encodes severity — severity belongs exclusively to
the tier scale. A copper badge implying danger is a defect.

**The Indoor Voice Rule.** Threat tiers stay desaturated at all times. A High
finding is `#fca5a5`, not `#ef4444`. The system reports; it does not shout.
Saturating a tier to increase urgency is the single fastest way to make this
product feel like the thing it was built not to be.

**The Border Rule.** Borders are `rgba(168, 162, 158, …)` at 0.08–0.2 opacity at
rest, shifting toward `rgba(201, 149, 108, 0.2–0.4)` on hover. Borders are how
surfaces separate; contrast is not.

## Typography

**Display Font:** Libre Baskerville (with Georgia, serif)
**Body Font:** Outfit (with system-ui, sans-serif)

**Character:** An elegant serif carrying trust and composure, set against a
clean geometric sans that keeps the dense operational screens legible. The
pairing is what keeps the product from reading as either a security tool or a
consumer app — the serif says considered, the sans says usable.

### Hierarchy
- **Display** (400, `clamp(2.5rem, 8vw, 4.5rem)`, 1.1): Hero titles only.
- **Headline** (400, `clamp(1.75rem, 4vw, 2.5rem)`, 1.1): Section titles.
- **Title** (400, `1.125rem`–`1.25rem`, 1.3): Card titles, panel headers.
- **Body** (300–400, `0.9375rem`–`1.125rem`, 1.6–1.7): All running text.
  Weight 300 for descriptions, 400 for default.
- **Label** (500, `0.8125rem`, `0.03em`, uppercase): Form labels.
- **Eyebrow** (500, `0.8125rem`, `0.15em`, uppercase): Section kickers, in
  copper.

### Named Rules

**The Serif-Heads-Only Rule.** Libre Baskerville is for headings, section
titles, quotes, and the brand name. It never sets body copy, navigation,
buttons, or form fields. Outfit never sets a heading.

**The Italic Accent Rule.** The emphasized word in a hero heading takes
`font-style: italic` and Reading Copper. One word per heading — the device stops
working the moment it repeats.

## Layout

Content is centered at a `1100px` maximum width. Sections breathe at `8rem 2rem`,
tightening to `5rem 1.5rem` below 768px. Hero areas fill the viewport
(`min-height: 100dvh`) and center their content with flexbox. Single-purpose
pages — login above all — narrow to a `400px` column centered in the viewport.

Navigation is fixed to the top at `1.5rem 2rem`, over a gradient fade
(`rgba(12, 10, 9, 0.8)` to transparent) rather than a solid bar, so the ground
stays continuous behind it.

Grids are auto-fitting rather than fixed: `repeat(auto-fit, minmax(280px, 1fr))`
at `2rem` gaps for content columns, `minmax(300px, 1fr)` for card grids. Pipeline
and step sequences lay out as a flex row with SVG connectors between items and
wrap to a column on mobile, the connectors rotating 90 degrees.

Card padding scales with the card's rank: `2rem` standard, `3rem` featured
(dropping to `2rem 1.5rem` on mobile), `2rem 1.5rem` for pipeline steps.

**Breakpoints.** Two, and only two. At `768px` navigation and hero padding
reduce, the hero logo steps 120px → 90px, and step layouts go vertical. At
`480px` calls to action and submit buttons go full width and the login logo
steps 72px → 64px.

## Elevation & Depth

Depth is **hybrid: tone ranks, shadow answers**. A surface's importance is
carried by tonal layering — background opacity climbs `0.4` → `0.8` and border
opacity `0.08` → `0.15` as a card grows in rank — while shadows are reserved
almost entirely for interaction. A card does not sit higher because it matters;
it sits higher because you touched it.

Beneath everything sits a three-layer atmosphere, all fixed and negatively
z-indexed: a base gradient at `-3`, warmth radials at `-2`, and two or three
blurred ambient orbs at `-1` drifting on a 25–30s cycle. This layer is
`aria-hidden` and must stay barely perceptible.

### Shadow Vocabulary
- **Featured glow** (`0 0 0 1px rgba(0,0,0,0.25), 0 20px 50px -10px rgba(0,0,0,0.5), 0 0 80px -20px var(--copper-glow)`):
  The copper halo under featured and CTA cards. Atmosphere, not rank.
- **Hover lift** (`0 20px 40px -10px rgba(0,0,0,0.4)`): Paired with
  `translateY(-2px … -4px)` on interactive surfaces.
- **Action shadow** (`0 4px 20px -4px rgba(245,158,11,0.4)`, deepening to
  `0 8px 30px -4px rgba(245,158,11,0.5)` on hover): Primary CTAs only.
- **Focus ring** (`0 0 0 3px rgba(201,149,108,0.15)`): Input focus. A ring, not
  a flash.

### Named Rules

**The Tone-Ranks Rule.** If a surface needs to read as more important, raise its
background and border opacity — do not give it a shadow. Shadows that encode
rank collide with shadows that encode state, and the interface stops being
readable at a glance.

**The Barely-There Rule.** Background warmth exists to create atmosphere, never
to attract attention. If a reviewer notices the orbs before the content, the
layer is too strong.

## Shapes

Corners are generously rounded and scale with the surface's weight: `8px` for
ghost buttons and chips, `12px` for primary buttons and input containers, `16px`
for standard cards, `20px` for pipeline and form cards, `24px` for featured and
CTA cards. Nothing in the system is square-cornered.

Surfaces are defined by a `1px` low-opacity border plus a `145deg` gradient
fill, never by a hard edge or a drop shadow at rest. The recurring silhouette is
the **concentric ring** — the brand mark is five stroked circles at ascending
opacity around a filled core, and it may reduce to two rings plus a core at
small sizes but never below that.

Icons are inline SVG using `currentColor`, with no fixed width or height
attributes — they inherit color from the parent (usually copper) and are sized
in CSS. Stroke icons use round caps and joins.

## Components

### Buttons
- **Shape:** Softly rounded (`12px` primary, `8px` ghost).
- **Primary:** An amber-to-copper gradient (`135deg`) carrying *dark* text
  (`#0c0a09`) — the one place in the system where the ground color becomes
  foreground. Padding `1rem 2rem`, weight 500, `1.0625rem`.
- **Hover / Focus:** Lifts `translateY(-3px)` with the action shadow deepening,
  over `0.4s` on the expo curve. Active returns to `0`. Disabled drops to
  `opacity: 0.4` with `cursor: not-allowed` and no transform.
- **Ghost:** Transparent on a `rgba(168,162,158,0.2)` border, text in Body Text
  Bright. On hover the text goes Lit Cream, the border goes copper at `0.4`, and
  a `rgba(201,149,108,0.1)` wash fills in.

### Cards / Containers
- **Corner Style:** `16px` standard, `20px` pipeline and form, `24px` featured.
- **Background:** A `145deg` gradient from Surface to Dim Ground, both at partial
  opacity (`0.4`/`0.5` standard, climbing to `0.8`/`0.9` for form cards).
- **Shadow Strategy:** None at rest — see Elevation & Depth. Featured cards carry
  the copper glow; all interactive cards lift on hover.
- **Border:** `1px` at `rgba(168,162,158,0.08–0.15)`, shifting copper on hover.
- **Internal Padding:** `2rem` standard, `3rem` featured.
- **Login card:** adds `backdrop-filter: blur(20px)` over the ambient layer.

### Inputs / Fields
- **Style:** A container holding the field — `rgba(12,10,9,0.6)` fill, `1px`
  border at `rgba(168,162,158,0.15)`, `12px` radius, `0 1rem` padding. The input
  itself is borderless and transparent.
- **Focus:** Border goes solid Reading Copper, the fill deepens to
  `rgba(12,10,9,0.8)`, and a `3px` copper ring at `0.15` appears. Calm, not a
  flash.
- **Labels:** Uppercase, `0.8125rem`, weight 500, `0.03em`, in Body Text Bright.
- **Prefix symbols** (the `@` before a handle) sit in Muted and transition to
  copper on focus.

### Navigation
Fixed to the top over a gradient fade rather than a solid bar. Links are Outfit
at weight 500 in Body Text Bright, going Lit Cream on hover over `0.3s`. The
32–36px ring mark anchors the left. Semantic `<nav>`, with `aria-label` on the
logo link.

### Threat Tier Badge (signature)
The component that carries the product's entire tone. Not a chip — **colored
text**: the tier name set in its own color at weight 500, `0.875rem`, with no
fill, border, or radius. Quiet by construction.

Severity is encoded **only** here, and only in these four desaturated values:
High `#fca5a5`, Elevated `#fdba74`, Watch `#fcd34d`, Low `#a8a29e`. As text on
the ground these measure 10.41 / 11.71 / 13.70 / 7.83:1 — the restraint costs
nothing in legibility.

**Abstention is not a tier.** `NotAssessed` and `Insufficient Data` mean no
reading was taken. They must render *visibly outside the scale* — the intended
treatment is Muted (`#78716c`) text with a dashed underline or an outlined
container, staying within the text idiom rather than introducing a filled chip
that exists nowhere else in the system.

> **Implementation gap (#245).** Today a null tier renders via
> `?? '#a8a29e'` — Low's exact color — so abstention is not merely unstyled, it
> is actively indistinguishable from a genuine Low verdict. PRODUCT.md treats
> abstention as first-class and explicitly not a variant of Low. The treatment
> above is the intended target, not the current state.

### Brand Mark
Five concentric stroked circles at ascending opacity (0.15 → 0.7) around a
filled core, in `currentColor`. Rings pulse on a 4–5s cycle staggered 0.3–0.5s
apart; the core breathes on the same cycle. Sizes run 120px (hero) down to 32px
(footer).

## Do's and Don'ts

### Do:
- **Do** use Cream for text and Night Ground for backgrounds. The No-Pure Rule
  is absolute.
- **Do** keep severity in the tier chip and nowhere else. Copper is identity,
  amber is action.
- **Do** rank surfaces with tone — background and border opacity — and reserve
  shadow for interaction.
- **Do** set every heading in Libre Baskerville and everything else in Outfit.
- **Do** ship `prefers-reduced-motion` handling with any animated surface. It is
  mandatory, not a nicety, for an audience often reading this while distressed.
- **Do** render abstention outside the tier scale, as an outlined chip.
- **Do** mark every decorative layer `aria-hidden="true"`.

### Don't:
- **Don't** saturate a threat tier to convey urgency. The Indoor Voice Rule is
  the difference between this product and the console it refuses to be.
- **Don't** build anything that reads as a **security operations console**
  (neon-on-black, red alert banners, monospace-everything, telemetry chrome), a
  **generic SaaS dashboard** (white cards, Inter everywhere, blue primary,
  purple gradient hero), **clinical or medical** (cool grays, hard edges,
  chart-first), or a **playful consumer app** (bounce easing, mascots, confetti,
  emoji-as-status). All four are confirmed anti-references.
- **Don't** use amber outside a call to action, or copper to indicate state.
- **Don't** give a surface a resting shadow to make it feel important.
- **Don't** let the ambient background become noticeable.
- **Don't** repeat the italic copper accent more than once per heading.
- **Don't** square a corner. Nothing in this system has a hard 90-degree edge.
