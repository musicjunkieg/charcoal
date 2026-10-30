// The privacy page (#259) states numbers the backend enforces. A promise the
// code does not keep is worse than no promise, so each figure is checked
// against the Rust source it comes from: change the backend and this fails
// until the page is updated to match.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { PRIVACY_FACTS } from './privacy-facts';

// Resolved against this file, not the working directory: `npm --prefix web`
// and a bare `vitest` start in different places.
const repo = new URL('../../../', import.meta.url);
const src = (path: string) => readFileSync(new URL(path, repo), 'utf-8');

/** The single capture group of `pattern` in `text`, as a number. */
function grab(text: string, pattern: RegExp): number {
	const m = text.match(pattern);
	if (!m) throw new Error(`pattern not found: ${pattern}`);
	return Number(m[1].replace(/_/g, ''));
}

describe('privacy page facts match the backend', () => {
	it('post cache retention', () => {
		const rs = src('src/db/cache_retention.rs');
		expect(PRIVACY_FACTS.postCacheDays).toBe(
			grab(rs, /FEED_SNAPSHOT_RETENTION[^=]*=\s*chrono::Duration::days\((\d+)\)/)
		);
	});

	it('model result retention', () => {
		const rs = src('src/db/cache_retention.rs');
		expect(PRIVACY_FACTS.modelResultDays).toBe(
			grab(rs, /SCORE_RETENTION[^=]*=\s*chrono::Duration::days\((\d+)\)/)
		);
	});

	it('posts cached per account', () => {
		const rs = src('src/pipeline/scan_phases/feed_cache.rs');
		expect(PRIVACY_FACTS.postsCachedPerAccount).toBe(
			grab(rs, /SNAPSHOT_FETCH_LIMIT:\s*usize\s*=\s*(\d+)/)
		);
	});

	it('example posts kept per scored account', () => {
		const rs = src('src/scoring/profile.rs');
		expect(PRIVACY_FACTS.examplePostsPerAccount).toBe(
			grab(rs, /let top_toxic_posts[\s\S]*?\.take\((\d+)\)/)
		);
	});

	it('sign-in cookie lifetime', () => {
		const rs = src('src/web/auth.rs');
		expect(PRIVACY_FACTS.cookieHours * 3600).toBe(
			grab(rs, /SESSION_TTL_SECS:\s*u64\s*=\s*([\d_]+)/)
		);
	});

	it('score freshness window', () => {
		const rs = src('src/db/models.rs');
		const days = [...rs.matchAll(/ScoringConfidence::\w+ => (\d+),/g)].map((m) => Number(m[1]));
		expect(days.length).toBeGreaterThan(0);
		expect(PRIVACY_FACTS.scoreFreshDays).toEqual({
			min: Math.min(...days),
			max: Math.max(...days)
		});
	});
});

describe('landing page footer', () => {
	const landing = src('web/src/routes/+page.svelte');

	it('links to the privacy page, which exists', () => {
		expect(landing).toContain('href="/privacy"');
		expect(() => src('web/src/routes/privacy/+page.svelte')).not.toThrow();
	});

	it('has no link to a terms page that does not exist', () => {
		expect(landing).not.toContain('href="/terms"');
	});
});
