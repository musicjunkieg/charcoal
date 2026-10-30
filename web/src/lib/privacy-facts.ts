// Figures the privacy page (/privacy, #259) states. Each one is enforced by
// backend code, and privacy-facts.test.ts checks it against that code, so the
// page cannot quietly drift from what Charcoal actually does.
export const PRIVACY_FACTS = {
	contactEmail: 'bryan@makeworkbetter.io',
	lastUpdated: 'October 29, 2026',
	/** Recent public posts read per scored account (feed cache). */
	postsCachedPerAccount: 50,
	/** How long that post cache is kept before deletion. */
	postCacheDays: 7,
	/** How long text-free model results (keyed by a hash) are kept. */
	modelResultDays: 90,
	/** Example posts kept with each score, as the evidence behind it. */
	examplePostsPerAccount: 3,
	/** Sign-in cookie lifetime. */
	cookieHours: 24,
	/** A score is treated as current for this long, then refreshed. */
	scoreFreshDays: { min: 3, max: 14 }
} as const;
