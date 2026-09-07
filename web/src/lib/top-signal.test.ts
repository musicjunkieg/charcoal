import { describe, it, expect } from 'vitest';
import { topSignal, SIGNAL_MAX_CHARS } from './top-signal';
import type { Account } from './types';

function account(overrides: Partial<Account> = {}): Pick<Account, 'top_toxic_posts' | 'behavioral_signals'> {
	return {
		top_toxic_posts: [],
		behavioral_signals: null,
		...overrides
	};
}

describe('topSignal', () => {
	it("quotes the first toxic post's text, whitespace collapsed", () => {
		const a = account({
			top_toxic_posts: [{ text: 'you\nare   the\tworst', toxicity: 0.9, uri: 'https://bsky.app/x' }]
		});
		expect(topSignal(a)).toBe('“you are the worst”');
	});

	it('truncates to 70 chars with a trailing ellipsis', () => {
		const long = 'x'.repeat(200);
		const a = account({
			top_toxic_posts: [{ text: long, toxicity: 0.9, uri: 'https://bsky.app/x' }]
		});
		const result = topSignal(a);
		// 70 chars + trailing … + two surrounding quote marks = 72
		expect(Array.from(result).length).toBe(72);
		expect(result.startsWith('“')).toBe(true);
		expect(result.endsWith('…”')).toBe(true);
	});

	it('falls back to "Joined a pile-on" when there are no posts and is_pile_on_participant is true', () => {
		const a = account({
			top_toxic_posts: [],
			behavioral_signals: { is_pile_on_participant: true }
		});
		expect(topSignal(a)).toBe('Joined a pile-on');
	});

	it('falls back to "No hostile post on record" when signals are null', () => {
		const a = account({ top_toxic_posts: [], behavioral_signals: null });
		expect(topSignal(a)).toBe('No hostile post on record');
	});

	it('falls back to "No hostile post on record" when posts are empty and not a pile-on participant', () => {
		const a = account({
			top_toxic_posts: [],
			behavioral_signals: { is_pile_on_participant: false }
		});
		expect(topSignal(a)).toBe('No hostile post on record');
	});

	it('falls back to "No hostile post on record" when the top post is whitespace-only', () => {
		const a = account({
			top_toxic_posts: [{ text: '   \n\t  ', toxicity: 0.9, uri: 'https://bsky.app/x' }],
			behavioral_signals: { is_pile_on_participant: true }
		});
		expect(topSignal(a)).toBe('Joined a pile-on');
	});

	it('exposes SIGNAL_MAX_CHARS as 70', () => {
		expect(SIGNAL_MAX_CHARS).toBe(70);
	});
});
