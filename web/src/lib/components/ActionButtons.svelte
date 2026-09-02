<script lang="ts">
	// Per-account Mute / Block with confirm sheet (#315, spec §5.2). Hidden
	// entirely when the server has the feature off or the viewer is
	// impersonating — nobody acts with someone else's credentials.
	import '$lib/website/styles/tokens.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import {
		getActionsStatus,
		getAccountActions,
		createActionBatch,
		undoAction,
		startConsent,
		NotConnectedError
	} from '$lib/api.js';
	import { buttonState, type ActiveRow } from '$lib/action-selection';
	import ConfirmSheet from '$lib/components/ConfirmSheet.svelte';
	import type { ActionKind, ActionsStatus } from '$lib/types.js';

	interface Props {
		handle: string;
		did: string;
		tier: string | null;
		/** `?resume=mute|block|undo` from a consent round-trip. */
		resume?: string | null;
		/** `?actions_error=` from a failed consent round-trip. */
		actionsError?: string | null;
		impersonating?: boolean;
	}

	let { handle, did, tier, resume = null, actionsError = null, impersonating = false }: Props = $props();

	let status = $state<ActionsStatus | null>(null);
	let active = $state<ActiveRow[]>([]);
	let busy = $state(false);
	let error = $state('');
	let sheet = $state<ActionKind | null>(null);

	const KINDS: ActionKind[] = ['mute', 'block'];
	let states = $derived(Object.fromEntries(KINDS.map((k) => [k, buttonState(active, k)])));

	const ERROR_COPY: Record<string, string> = {
		denied: "Bluesky didn't grant permission. Nothing was changed.",
		invalid_scope: "Bluesky granted different permissions than Charcoal asked for. Nothing was changed.",
		failed: "Something went wrong while connecting. Nothing was changed.",
		disabled: 'Mute and block actions are not enabled on this server.'
	};

	async function refresh() {
		const res = await getAccountActions(handle);
		active = res.actions.map((r) => ({ id: r.id, kind: r.kind, status: r.status }));
	}

	onMount(async () => {
		try {
			status = await getActionsStatus();
			if (!status.enabled) return;
			await refresh();
		} catch {
			status = null;
			return;
		}
		if (actionsError) error = ERROR_COPY[actionsError] ?? ERROR_COPY.failed;
		else if (resume === 'mute' || resume === 'block') sheet = resume;
	});

	async function confirm(kind: ActionKind) {
		sheet = null;
		busy = true;
		error = '';
		try {
			const res = await createActionBatch(kind, `account:${handle}`, [did]);
			if (res.batch_id !== null) await goto(`/actions/${res.batch_id}`);
			else await refresh();
		} catch (e) {
			if (e instanceof NotConnectedError) {
				await startConsent(kind, { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			busy = false;
		}
	}

	async function undo(kind: ActionKind) {
		const id = states[kind].actionId;
		if (id === null) return;
		busy = true;
		error = '';
		try {
			const res = await undoAction(id);
			await goto(`/actions/${res.batch_id}`);
		} catch (e) {
			if (e instanceof NotConnectedError) {
				await startConsent('undo', { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			busy = false;
		}
	}
</script>

{#if status?.enabled && !impersonating}
	<div class="actions">
		{#each KINDS as kind (kind)}
			{@const s = states[kind]}
			{#if s.state === 'done'}
				<span class="done">{s.label}</span>
				<button class="undo" onclick={() => undo(kind)} disabled={busy}>Undo</button>
			{:else}
				<button class="act" data-kind={kind} onclick={() => (sheet = kind)} disabled={busy}>
					{s.label}
				</button>
			{/if}
		{/each}
		{#if error}
			<p class="error">{error}</p>
		{/if}
	</div>

	{#if sheet}
		<ConfirmSheet
			kind={sheet}
			count={1}
			label={`@${handle}`}
			connected={status.connected}
			onconfirm={() => confirm(sheet!)}
			oncancel={() => (sheet = null)}
		/>
	{/if}
{/if}

<style>
	.actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
	.act,
	.undo {
		padding: 0.375rem 0.75rem;
		font-size: 0.8125rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15);
		border-radius: 8px;
		background: transparent;
		color: var(--charcoal-400);
		cursor: pointer;
	}
	.act[data-kind='block'] {
		color: var(--tier-high);
	}
	.act:hover:not(:disabled),
	.undo:hover:not(:disabled) {
		background: rgb(var(--charcoal-400-rgb) / 0.08);
	}
	.act:disabled,
	.undo:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.done {
		font-size: 0.8125rem;
		color: var(--status-ok);
	}
	.error {
		width: 100%;
		font-size: 0.75rem;
		color: var(--status-error);
	}
</style>
