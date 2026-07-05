import { describe, expect, it } from 'vitest';
import { topKeywords } from './fingerprint-keywords.js';
import type { FingerprintResponse } from './types.js';

function response(
	clusters: Array<{ label: string; keywords: string[]; weight: number }>
): FingerprintResponse {
	return {
		fingerprint: { clusters, post_count: 100 },
		post_count: 100,
		updated_at: '2026-07-05T12:00:00Z'
	};
}

describe('topKeywords', () => {
	it('returns keywords from the heaviest cluster first', () => {
		const fp = response([
			{ label: 'light', keywords: ['minor'], weight: 0.2 },
			{ label: 'heavy', keywords: ['major', 'big'], weight: 0.9 }
		]);
		expect(topKeywords(fp, 12)).toEqual(['major', 'big', 'minor']);
	});

	it('deduplicates keywords that appear in multiple clusters', () => {
		const fp = response([
			{ label: 'a', keywords: ['shared', 'one'], weight: 0.9 },
			{ label: 'b', keywords: ['shared', 'two'], weight: 0.5 }
		]);
		expect(topKeywords(fp, 12)).toEqual(['shared', 'one', 'two']);
	});

	it('limits the number of keywords returned', () => {
		const fp = response([{ label: 'a', keywords: ['k1', 'k2', 'k3', 'k4', 'k5'], weight: 1.0 }]);
		expect(topKeywords(fp, 3)).toEqual(['k1', 'k2', 'k3']);
	});

	it('returns empty for a null response or null fingerprint', () => {
		expect(topKeywords(null, 12)).toEqual([]);
		expect(
			topKeywords({ fingerprint: null, post_count: 0, updated_at: '2026-07-05T12:00:00Z' }, 12)
		).toEqual([]);
	});

	it('does not mutate the cluster order on the input object', () => {
		const fp = response([
			{ label: 'light', keywords: ['minor'], weight: 0.2 },
			{ label: 'heavy', keywords: ['major'], weight: 0.9 }
		]);
		topKeywords(fp, 12);
		expect(fp.fingerprint?.clusters[0].label).toBe('light');
	});
});
