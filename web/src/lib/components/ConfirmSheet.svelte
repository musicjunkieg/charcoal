<script lang="ts">
	// Confirm sheet for mute/block (#315, spec §5.2). One component for the
	// single-account and tier-wide cases; the consent interstitial is the same
	// sheet with a different footer when there is no write session yet.
	import type { ActionKind, SheetRow } from '$lib/types.js';
	import { tierClass } from '$lib/tier-class';
	import '$lib/website/styles/tokens.css';
	import '$lib/website/styles/tiers.css';

	interface Props {
		kind: ActionKind;
		/** How many accounts the action covers (after removing already-done). */
		count: number;
		/** `@handle` for one account, or the tier name for a bulk action. */
		label: string;
		connected: boolean;
		/** Accounts left out because Charcoal already holds this action on them. */
		alreadyDone?: number;
		/** Bulk only: the account list with checkboxes (spec §5.1). When present,
		 *  `count`/`alreadyDone` are derived from the rows and the props are ignored. */
		rows?: SheetRow[];
		onconfirm: (dids: string[]) => void;
		oncancel: () => void;
	}

	let { kind, count, label, connected, alreadyDone = 0, rows, onconfirm, oncancel }: Props = $props();

	const VERB: Record<ActionKind, string> = { mute: 'Mute', block: 'Block' };
	const BODY: Record<ActionKind, string> = {
		mute: "You stop seeing them. They won't know.",
		block: "They can't see, reply to, or quote you. Blocks are visible to anyone who looks."
	};
	const DONE: Record<ActionKind, string> = { mute: 'already muted', block: 'already blocked' };

	let selected = $state<Set<string>>(new Set());
	$effect(() => {
		// Reset the selection whenever a new list arrives; done rows start unchecked.
		selected = new Set((rows ?? []).filter((r) => !r.done).map((r) => r.did));
	});
	let liveCount = $derived(rows ? selected.size : count);
	let liveAlreadyDone = $derived(rows ? rows.filter((r) => r.done).length : alreadyDone);

	function toggle(did: string) {
		const next = new Set(selected);
		if (next.has(did)) next.delete(did);
		else next.add(did);
		selected = next;
	}

	let title = $derived(
		liveCount === 1 && label.startsWith('@')
			? `${VERB[kind]} ${label}?`
			: `${VERB[kind]} ${liveCount} ${liveCount === 1 ? 'account' : 'accounts'} in ${label}?`
	);

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') oncancel();
	}

	// Cancel only when the backdrop itself is clicked, not a click that
	// bubbled up from inside the sheet — avoids needing a click handler (and
	// therefore a matching keyboard handler) on the non-interactive dialog
	// container itself.
	function onBackdropClick(e: MouseEvent) {
		if (e.target === e.currentTarget) oncancel();
	}

	let sheetEl = $state<HTMLDivElement | null>(null);
	$effect(() => {
		sheetEl?.focus();
	});
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" onclick={onBackdropClick}>
	<div
		class="sheet"
		role="dialog"
		aria-modal="true"
		aria-labelledby="confirm-title"
		tabindex="-1"
		bind:this={sheetEl}
	>
		<h2 id="confirm-title">{title}</h2>
		<p class="body">{BODY[kind]}</p>
		{#if rows}
			<ul class="rows" aria-label="Accounts">
				{#each rows as r (r.did)}
					<li class="row" class:done={r.done}>
						<label>
							<input
								type="checkbox"
								checked={selected.has(r.did)}
								disabled={r.done}
								onchange={() => toggle(r.did)}
							/>
							<span class="handle">@{r.handle}</span>
							{#if r.tier}<span class="tier-badge {tierClass(r.tier)}">{r.tier}</span>{/if}
							<span class="signal">{r.done ? DONE[kind] : r.signal}</span>
						</label>
					</li>
				{/each}
			</ul>
		{/if}
		{#if !rows && liveAlreadyDone > 0}
			<p class="already">{liveAlreadyDone} {DONE[kind]}</p>
		{/if}
		{#if !connected}
			<p class="consent">
				Charcoal needs permission to mute or block on your behalf. You'll approve exactly these two abilities on Bluesky — nothing else.
			</p>
		{/if}
		<div class="footer">
			<button class="cancel" onclick={oncancel}>Cancel</button>
			<button
				class="confirm"
				data-kind={kind}
				onclick={() => onconfirm(rows ? Array.from(selected) : [])}
				disabled={liveCount === 0}
			>
				{connected ? VERB[kind] : 'Continue to Bluesky'}
			</button>
		</div>
	</div>
</div>

<style>
	.backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.4); display: flex; align-items: flex-end; justify-content: center; z-index: 50; }
	@media (min-width: 40rem) { .backdrop { align-items: center; } }
	.sheet { width: 100%; max-width: 26rem; background: var(--charcoal-900, #1c1917); color: var(--charcoal-100, #f5f5f4); border-radius: 16px 16px 0 0; padding: 1.25rem 1.25rem 1.5rem; display: flex; flex-direction: column; gap: 0.75rem; }
	@media (min-width: 40rem) { .sheet { border-radius: 16px; } }
	h2 { font-family: 'Outfit', system-ui, sans-serif; font-size: 1.125rem; margin: 0; }
	.body { margin: 0; line-height: 1.5; }
	.rows { list-style: none; margin: 0; padding: 0; max-height: 40vh; overflow-y: auto; display: flex; flex-direction: column; gap: 0.25rem; border-top: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); padding: 0.5rem 0; }
	.row label { display: grid; grid-template-columns: auto auto auto 1fr; align-items: center; gap: 0.5rem; font-size: 0.8125rem; cursor: pointer; }
	.row.done { opacity: 0.5; }
	.row.done label { cursor: default; }
	.handle { font-weight: 500; }
	.signal { color: var(--charcoal-400); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.already { margin: 0; font-size: 0.8125rem; color: var(--charcoal-500); }
	.consent { margin: 0; font-size: 0.875rem; line-height: 1.5; color: var(--charcoal-400); border-left: 2px solid rgb(var(--charcoal-400-rgb) / 0.3); padding-left: 0.75rem; }
	.footer { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }
	.footer button { padding: 0.5rem 0.9rem; font: inherit; font-size: 0.875rem; border-radius: 8px; cursor: pointer; }
	.cancel { background: transparent; color: inherit; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.25); }
	.confirm { background: var(--charcoal-100, #f5f5f4); color: var(--charcoal-900, #1c1917); border: 0; }
	.confirm[data-kind='block'] { background: var(--tier-high); color: white; }
	.confirm:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
