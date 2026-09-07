<script lang="ts">
	// Per-account Mute / Block with confirm sheet (#315, spec §5.2). Hidden
	// entirely when the server has the feature off or the viewer is
	// impersonating — nobody acts with someone else's credentials.
	//
	// A single action finishes in place (#332): no navigation to the batch
	// page. The button shows `Muting…` while a toast polls the batch, then
	// both settle from the server's answer.
	import '$lib/website/styles/tokens.css';
	import { onMount } from 'svelte';
	import {
		getActionsStatus,
		getAccountActions,
		getActionBatch,
		createActionBatch,
		undoAction,
		retryBatch,
		startConsent,
		NotConnectedError
	} from '$lib/api.js';
	import { buttonState, type ActiveRow } from '$lib/action-selection';
	import { pollUntilSettled, toastCopy, type ToastKind } from '$lib/action-progress';
	import { raise, update, dismiss } from '$lib/toast';
	import ConfirmSheet from '$lib/components/ConfirmSheet.svelte';
	import type { ActionKind, ActionsStatus } from '$lib/types.js';

	interface Props {
		handle: string;
		did: string;
		tier: string | null;
		/** `?resume=mute|block|undo` from a consent round-trip. `mute`/`block`
		 *  reopen the confirm sheet; `undo` cannot (the round-trip doesn't carry
		 *  which action was being undone), so it surfaces a notice instead. */
		resume?: string | null;
		/** `?actions_error=` from a failed consent round-trip. */
		actionsError?: string | null;
		impersonating?: boolean;
	}

	let { handle, did, tier, resume = null, actionsError = null, impersonating = false }: Props = $props();

	let status = $state<ActionsStatus | null>(null);
	let active = $state<ActiveRow[]>([]);
	let busy = $state(false);
	/** Which button is mid-flight, so it can read `Muting…` and the other
	 *  can sit disabled. `'undo'` disables both without relabelling. */
	let inflight = $state<ActionKind | 'undo' | null>(null);
	let error = $state('');
	let notice = $state('');
	let sheet = $state<ActionKind | null>(null);

	const KINDS: ActionKind[] = ['mute', 'block'];
	let states = $derived(Object.fromEntries(KINDS.map((k) => [k, buttonState(active, k)])));

	const WORKING_LABEL: Record<ActionKind, string> = { mute: 'Muting…', block: 'Blocking…' };
	/** Settled ok toasts hold this long, then go (spec §3.1). */
	const OK_TTL_MS = 6000;

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
		else if (resume === 'undo') notice = 'Connected to Bluesky. Click Undo again to finish.';
	});

	/** Raise the working toast for `batchId`, poll it to a verdict, refresh
	 *  the buttons, and settle the toast. `toastKind` picks the verb;
	 *  `buttonKind` is the button that can offer Undo (null for an undo
	 *  batch — redoing is the original button, which is back to `Mute`). */
	async function track(batchId: number, toastKind: ToastKind, buttonKind: ActionKind | null) {
		const record = { label: 'Record', url: `/actions/${batchId}` };
		const id = raise({ tone: 'working', text: toastCopy(toastKind, handle, 'working'), actions: [] });
		const settled = await pollUntilSettled(() => getActionBatch(batchId));
		try {
			await refresh();
		} catch {
			// The toast still tells the truth; the buttons catch up next load.
		}
		inflight = null;
		if (settled === 'timeout') {
			update(id, { tone: 'error', text: toastCopy(toastKind, handle, 'timeout'), actions: [], href: record });
			return;
		}
		switch (settled.kind) {
			case 'applied': {
				// Captured as a const so the narrowing survives into the closure.
				const bk = buttonKind;
				const actions =
					bk !== null
						? [{ label: 'Undo', onclick: () => { dismiss(id); void undo(bk); } }]
						: [];
				update(id, { tone: 'ok', text: toastCopy(toastKind, handle, 'applied'), actions, href: record, ttlMs: OK_TTL_MS });
				return;
			}
			case 'skipped':
				update(id, { tone: 'ok', text: toastCopy(toastKind, handle, 'skipped'), actions: [], href: record, ttlMs: OK_TTL_MS });
				return;
			case 'failed':
				update(id, {
					tone: 'error',
					text: toastCopy(toastKind, handle, 'failed'),
					actions: [{ label: 'Retry', onclick: () => { dismiss(id); void retry(batchId, toastKind, buttonKind); } }],
					href: record
				});
				return;
			case 'parked':
				update(id, {
					tone: 'error',
					text: toastCopy(toastKind, handle, 'parked'),
					actions: [{ label: 'Reconnect', onclick: () => { void startConsent(buttonKind ?? 'undo', { handle }); } }],
					href: record
				});
				return;
		}
	}

	async function retry(batchId: number, toastKind: ToastKind, buttonKind: ActionKind | null) {
		inflight = buttonKind ?? 'undo';
		error = '';
		try {
			const res = await retryBatch(batchId);
			await track(res.batch_id, toastKind, buttonKind);
		} catch (e) {
			inflight = null;
			if (e instanceof NotConnectedError) {
				await startConsent(buttonKind ?? 'undo', { handle });
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		}
	}

	async function confirm(kind: ActionKind) {
		sheet = null;
		busy = true;
		error = '';
		notice = '';
		try {
			const res = await createActionBatch(kind, `account:${handle}`, [did]);
			if (res.batch_id === null) {
				// The server returns batch_id: null when every target is already
				// in force (is_in_force: Charcoal applied it, or the user already
				// held it themselves) — never for "in progress" work.
				notice = 'That action is already in place.';
				await refresh();
				return;
			}
			inflight = kind;
			busy = false;
			await track(res.batch_id, kind, kind);
		} catch (e) {
			inflight = null;
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
		notice = '';
		try {
			const res = await undoAction(id);
			inflight = 'undo';
			busy = false;
			await track(res.batch_id, kind === 'mute' ? 'unmute' : 'unblock', null);
		} catch (e) {
			inflight = null;
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
			{#if inflight === kind}
				<button class="act working" data-kind={kind} disabled>
					<span class="spinner" aria-hidden="true"></span>
					{WORKING_LABEL[kind]}
				</button>
			{:else if s.state === 'done'}
				<span class="done">{s.label}</span>
				<!-- No Undo when `actionId` is null: that mute or block is the
				     person's own, and Charcoal does not remove it (#261). -->
				{#if s.actionId !== null}
					<button class="undo" onclick={() => undo(kind)} disabled={busy || inflight !== null}>Undo</button>
				{/if}
			{:else}
				<button class="act" data-kind={kind} onclick={() => (sheet = kind)} disabled={busy || inflight !== null}>
					{s.label}
				</button>
			{/if}
		{/each}
		{#if error}
			<p class="error">{error}</p>
		{:else if notice}
			<p class="notice">{notice}</p>
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
	.act.working {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		opacity: 1;
		cursor: progress;
	}
	.spinner {
		width: 0.75rem;
		height: 0.75rem;
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
	.done {
		font-size: 0.8125rem;
		color: var(--status-ok);
	}
	.error {
		width: 100%;
		font-size: 0.75rem;
		color: var(--status-error);
	}
	.notice {
		width: 100%;
		font-size: 0.75rem;
		color: var(--charcoal-400);
	}
</style>
