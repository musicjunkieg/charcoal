/** Cents → "$d.cc". `null` (no full scan yet) → an em dash. */
export function formatCents(cents: number | null): string {
	if (cents === null) return '—';
	return `$${(cents / 100).toFixed(2)}`;
}
