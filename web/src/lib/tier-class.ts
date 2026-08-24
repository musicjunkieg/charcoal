/** Threat tiers that have their own colour. Anything else is unscored. */
const SCORED = new Set(['high', 'elevated', 'watch', 'low']);

/**
 * CSS class for a threat tier.
 *
 * Unrecognised values return `tier-low` on purpose: the inline styles this
 * replaced ended in `?? '#a8a29e'`, which is exactly `--tier-low`. Abstained
 * accounts (NotAssessed, InsufficientData) rely on that fallback, so changing
 * it here would recolour them.
 */
export function tierClass(tier: string | null | undefined): string {
	const t = (tier ?? '').trim().toLowerCase();
	return SCORED.has(t) ? `tier-${t}` : 'tier-low';
}
