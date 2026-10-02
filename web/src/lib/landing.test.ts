import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

// The landing page is the one surface a stranger reads before trusting us, and
// PRODUCT.md is specific about what it may and may not say. These are the
// promises a redesign is most likely to break quietly: the page that shipped
// before #395 carried three invented testimonials, a button labelled "waitlist"
// that went somewhere else, and a claim of auto-muting that was never built.
const SRC = readFileSync('src/routes/+page.svelte', 'utf8');
// Markup only: the rules below are about what a visitor reads, not class names.
const MARKUP = SRC.slice(SRC.indexOf('</script>'), SRC.indexOf('<style>'));

describe('landing page: one honest action', () => {
	it('the primary action is signing in with Bluesky', () => {
		expect(MARKUP).toMatch(/href="\/login"[^>]*>[\s\S]{0,200}Sign in with Bluesky/);
	});

	it('nothing is labelled a waitlist button that goes somewhere else', () => {
		expect(MARKUP).not.toContain('href="#early-access"');
		expect(MARKUP).not.toMatch(/Join the waitlist/i);
	});

	it('links nowhere that does not exist', () => {
		// /privacy and /terms are not written yet (#259). A dead link on the page
		// that asks for trust is worse than no link.
		expect(MARKUP).not.toContain('href="/privacy"');
		expect(MARKUP).not.toContain('href="/terms"');
	});

	it('still links the companion publication', () => {
		expect(SRC).toContain('https://charcoal.leaflet.pub');
	});
});

describe('landing page: nothing invented', () => {
	it('carries no testimonials', () => {
		// PRODUCT.md, Evidence on Hand: no testimonials exist; do not fabricate.
		expect(SRC).not.toMatch(/testimonial/i);
		expect(SRC).not.toMatch(/Early tester|Beta user|Waitlist member/);
		expect(SRC).not.toMatch(/@(community|earlyaccess|waiting)\.bsky\.social/);
	});

	it('does not promise automation while it is unbuilt', () => {
		// The automatic wording lives in the file, ready for launch. What is
		// SHOWN is chosen by one constant, and it stays manual until the feature
		// ships (PRODUCT.md, launch gate 2026-10-02).
		expect(SRC).toMatch(/const ACTIONS_MODE: ActionsMode = 'manual';/);
		expect(MARKUP).not.toMatch(/auto-mute|automatically/i);
		expect(MARKUP).not.toMatch(/digest of flagged/i);
		expect(MARKUP).not.toMatch(/in the background/i);
	});

	it('is not stamped with a stale year', () => {
		expect(MARKUP).not.toContain('2024');
	});
});

describe('landing page: the voice PRODUCT.md binds', () => {
	it('says joy, never enjoyable or comfortable', () => {
		expect(MARKUP).toMatch(/\bjoy\b/i);
		expect(MARKUP).not.toMatch(/enjoyable|comfortable/i);
	});

	it('does not open on fear', () => {
		const title = SRC.match(/<title>([^<]*)<\/title>/)?.[1] ?? '';
		const h1 = MARKUP.match(/<h1[\s\S]*?<\/h1>/)?.[0] ?? '';
		expect(title).not.toBe('');
		expect(h1).not.toBe('');
		for (const text of [title, h1]) {
			expect(text).not.toMatch(/threat|harass|toxic|harm|danger|attack/i);
		}
	});

	it('never says the pressure stops', () => {
		// The name's argument: Charcoal removes the people, never the pressure.
		expect(MARKUP).not.toMatch(/stress-free|frictionless|calmer way/i);
	});
});
