<script lang="ts">
	import { labelAccount } from '$lib/api.js';

	interface Props {
		targetDid: string;
		currentLabel?: string | null;
		predictedTier?: string | null;
		onlabeled?: (label: string) => void;
	}

	let { targetDid, currentLabel = null, predictedTier = null, onlabeled }: Props = $props();

	let saving = $state(false);
	let activeLabel = $state(currentLabel);
	let error = $state('');

	const TIERS = [
		{ value: 'high', display: 'High', color: '#fca5a5', bg: 'rgba(252, 165, 165, 0.12)', border: 'rgba(252, 165, 165, 0.25)' },
		{ value: 'elevated', display: 'Elevated', color: '#fdba74', bg: 'rgba(253, 186, 116, 0.12)', border: 'rgba(253, 186, 116, 0.25)' },
		{ value: 'watch', display: 'Watch', color: '#fcd34d', bg: 'rgba(252, 211, 77, 0.12)', border: 'rgba(252, 211, 77, 0.25)' },
		{ value: 'safe', display: 'Safe', color: '#86efac', bg: 'rgba(134, 239, 172, 0.12)', border: 'rgba(134, 239, 172, 0.25)' },
	] as const;

	function tierMatches(): boolean {
		if (!activeLabel || !predictedTier) return true;
		return activeLabel.toLowerCase() === predictedTier.toLowerCase();
	}

	async function handleLabel(tier: string) {
		if (saving) return;
		error = '';
		saving = true;
		try {
			await labelAccount(targetDid, tier);
			activeLabel = tier;
			onlabeled?.(tier);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to save label';
		} finally {
			saving = false;
		}
	}
</script>

<div class="label-group">
	<div class="label-buttons">
		{#each TIERS as tier}
			<button
				class="label-btn"
				class:active={activeLabel === tier.value}
				style="--tier-color: {tier.color}; --tier-bg: {tier.bg}; --tier-border: {tier.border}"
				onclick={() => handleLabel(tier.value)}
				disabled={saving}
			>
				{tier.display}
			</button>
		{/each}
	</div>

	{#if activeLabel && !tierMatches()}
		<p class="discrepancy">
			You labeled this <strong>{activeLabel}</strong> — Charcoal predicted <strong>{predictedTier}</strong>
		</p>
	{/if}

	{#if error}
		<p class="label-error">{error}</p>
	{/if}
</div>

<style>
	.label-group {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.label-buttons {
		display: flex;
		gap: 0.375rem;
	}

	.label-btn {
		padding: 0.375rem 0.75rem;
		font-size: 0.8125rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--tier-color);
		background: transparent;
		border: 1px solid rgba(168, 162, 158, 0.15);
		border-radius: 8px;
		cursor: pointer;
		transition: all 0.2s;
	}

	.label-btn:hover:not(:disabled) {
		background: var(--tier-bg);
		border-color: var(--tier-border);
	}

	.label-btn.active {
		background: var(--tier-bg);
		border-color: var(--tier-border);
		box-shadow: 0 0 0 1px var(--tier-border);
	}

	.label-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.discrepancy {
		font-size: 0.75rem;
		color: #78716c;
		line-height: 1.4;
	}

	.discrepancy strong {
		color: #a8a29e;
		text-transform: capitalize;
	}

	.label-error {
		font-size: 0.75rem;
		color: #f87171;
	}
</style>
