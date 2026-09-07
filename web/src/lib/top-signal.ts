import type { Account } from './types.js';

export const SIGNAL_MAX_CHARS = 70;

/** The single most useful thing to show beside a checkbox so that unchecking
 *  is an informed choice (spec §5.1). Order: the worst post Charcoal saw,
 *  then pile-on participation, then an honest "nothing on record". Plain
 *  words only — no scores. */
export function topSignal(a: Pick<Account, 'top_toxic_posts' | 'behavioral_signals'>): string {
	const post = a.top_toxic_posts?.[0];
	if (post && post.text.trim()) {
		const t = post.text.replace(/\s+/g, ' ').trim();
		const chars = Array.from(t);
		const short = chars.length > SIGNAL_MAX_CHARS ? chars.slice(0, SIGNAL_MAX_CHARS - 1).join('') + '…' : t;
		return `“${short}”`;
	}
	if (a.behavioral_signals?.is_pile_on_participant) return 'Joined a pile-on';
	return 'No hostile post on record';
}
