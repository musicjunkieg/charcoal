<script lang="ts">
	// Renders the toast store (#332, spec §3.2). Mounted once in the
	// protected layout; every page raises through `$lib/toast`. Newest is
	// last in the store and sits nearest the viewport edge (column-reverse).
	import { fly } from 'svelte/transition';
	import { toasts, dismiss } from '$lib/toast';
	import '$lib/website/styles/tokens.css';

	// Respect reduced motion by zeroing the transition rather than branching
	// the markup — one code path, same DOM either way.
	const reduced =
		typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
	const flyOpts = { y: reduced ? 0 : 16, duration: reduced ? 0 : 320, easing: easeOutExpo };

	function easeOutExpo(t: number): number {
		// cubic-bezier(0.16, 1, 0.3, 1) ≈ 1 - 2^(-10t); `--ease-out-expo` in
		// tokens.css is the CSS twin for hover/transform transitions.
		return t === 1 ? 1 : 1 - Math.pow(2, -10 * t);
	}
</script>

<div class="stack" aria-live="polite">
	{#each $toasts as t (t.id)}
		<div
			class="toast"
			data-tone={t.tone}
			role={t.tone === 'error' ? 'alert' : 'status'}
			transition:fly={flyOpts}
		>
			{#if t.tone === 'working'}
				<span class="spinner" aria-hidden="true"></span>
			{/if}
			<span class="text">{t.text}</span>
			{#each t.actions as a (a.label)}
				<button class="action" onclick={a.onclick}>{a.label}</button>
			{/each}
			{#if t.href}
				<a class="link" href={t.href.url}>{t.href.label}</a>
			{/if}
			<button class="close" aria-label="Dismiss" onclick={() => dismiss(t.id)}>×</button>
		</div>
	{/each}
</div>

<style>
	.stack {
		position: fixed;
		bottom: 1rem;
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		width: min(28rem, calc(100vw - 2rem));
		z-index: 60;
		pointer-events: none;
	}
	@media (min-width: 720px) {
		.stack {
			left: 1.5rem;
			transform: none;
		}
	}
	.toast {
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 0.625rem;
		padding: 0.625rem 0.75rem 0.625rem 0.875rem;
		background: var(--charcoal-800);
		color: var(--cream-50);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15);
		border-left: 3px solid var(--charcoal-500);
		border-radius: 8px;
		font-size: 0.8125rem;
		box-shadow: 0 8px 24px -8px rgb(var(--charcoal-950-rgb) / 0.6);
	}
	.toast[data-tone='ok'] {
		border-left-color: var(--status-ok);
	}
	.toast[data-tone='error'] {
		border-left-color: var(--status-error);
	}
	.text {
		flex: 1;
		min-width: 0;
	}
	.action,
	.link {
		font: inherit;
		font-size: 0.8125rem;
		font-weight: 500;
		color: var(--copper);
		background: none;
		border: 0;
		padding: 0.125rem 0.25rem;
		cursor: pointer;
		text-decoration: none;
	}
	.action:hover,
	.link:hover {
		text-decoration: underline;
	}
	.close {
		font: inherit;
		font-size: 1rem;
		line-height: 1;
		color: var(--charcoal-500);
		background: none;
		border: 0;
		padding: 0 0.25rem;
		cursor: pointer;
	}
	.close:hover {
		color: var(--cream-50);
	}
	.spinner {
		width: 0.875rem;
		height: 0.875rem;
		flex: none;
		border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2);
		border-top-color: var(--charcoal-400);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.spinner {
			animation: none;
		}
	}
</style>
