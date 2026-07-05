// Derives the keyword chips for the "Your topic fingerprint" card:
// keywords from the heaviest clusters first, deduplicated, limited.
// Extracted from the dashboard page so it's unit-testable.

import type { FingerprintResponse } from './types.js';

export function topKeywords(fp: FingerprintResponse | null, limit: number): string[] {
	if (!fp?.fingerprint) return [];
	// Copy before sorting — never reorder the caller's cluster list.
	const ordered = [...fp.fingerprint.clusters].sort((a, b) => b.weight - a.weight);
	return [...new Set(ordered.flatMap((c) => c.keywords))].slice(0, limit);
}
