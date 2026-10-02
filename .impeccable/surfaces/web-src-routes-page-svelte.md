---
version: 1
slug: "web-src-routes-page-svelte"
primary_target: "web/src/routes/+page.svelte"
related_targets: []
---

Scope: the public landing page at `/`. Mode: Persuade.

Audience: someone who posts about hard things on Bluesky and braces every time. They arrive cold or from a personal invitation, often on a phone.

Job: make them believe posting can be joyful again, and get them to sign in with Bluesky. People not yet admitted land on the waitlist, so the one action is honest.

Proof (confirmed by Bryan 2026-10-02): the before/after story, not testimonials — on edge with every post, then looking forward to who replies because people with a pattern of bad faith, worst-possible readings and personal attacks are gone first. The four "what a blocklist cannot do" claims back it. No testimonials, user counts or benchmarks exist; none may be invented.

Direction (pinned by Bryan after two re-rolls: "I want wakanda, I want joyful but grounded and powerful... I can handle hard things but I am choosing not to deal with bullshit"): daylight Afrofuturism, seen from inside a border the visitor controls. Whole fields of sun gold, royal purple, laterite red, leaf green and aubergine ink. Concentric rings built from dashed strokes; triangle-and-diamond band dividers; Tac One poster capitals, Ojuju headings, Atkinson Hyperlegible Next text.

Memorable moment: the quarter dome in the hero. Posts travel toward it; friends pass through, the others break into the same beads the outer band is made of. With reduced motion it is a still plate.

Boundaries: no Marvel names, marks or lettering ("Wakanda" is an internal reference only). Original geometry; no borrowed sacred symbols. Display faces from Black or African type designers. Never opens on fear. Daylight, not neon on black.

Who decides: the section has a manual and an automatic wording behind `ACTIONS_MODE`. It stays `manual` until automated actions ship (PRODUCT.md launch gate); `web/src/lib/landing.test.ts` enforces that and the other promises above.

Unresolved:
- The signed-in app, /login and /waitlist still wear the Hearth Watch look. Bryan chose to replace the whole look; those surfaces follow, and DESIGN.md is rewritten when they do.
- Whether a Black illustrator or designer should redraw or review the pattern work before public launch.
- /privacy and /terms are not written (#259), so the footer does not link them.
