<script lang="ts">
	// One batch (#315, spec §5.4): rows with status, drift note, undo/retry.
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		getActionBatch,
		undoBatch,
		retryBatch,
		undoAction,
		startConsent,
		NotConnectedError,
		AuthError,
		AccessRevokedError
	} from '$lib/api.js';
	import { batchHeadline, driftNote, isRunning, isParked, canRetry, canUndo } from '$lib/action-status';
	import { tierClass } from '$lib/tier-class';
	import type { ActionBatchDetail, ActionRowView } from '$lib/types.js';
	import '$lib/website/styles/tokens.css';
	import '$lib/website/styles/tiers.css';

	let asUser = $derived($page.url.searchParams.get('as_user'));
	let asUserSuffix = $derived(asUser ? `?as_user=${encodeURIComponent(asUser)}` : '');

	let detail = $state<ActionBatchDetail | null>(null);
	let loading = $state(true);
	let notFound = $state(false);
	let busy = $state(false);
	let error = $state('');
	let timer: ReturnType<typeof setInterval> | null = null;

	const STATUS_LABEL: Record<ActionRowView['status'], string> = {
		pending: 'Pending',
		applied: 'Done',
		skipped_already_done: 'Already done',
		failed: 'Failed',
		undone: 'Undone'
	};

	async function load() {
		try {
			detail = await getActionBatch(Number($page.params.id));
		} catch (err) {
			if (err instanceof AuthError) return goto('/login');
			if (err instanceof AccessRevokedError) return goto('/waitlist');
			notFound = true;
		} finally {
			loading = false;
		}
		const running = detail ? isRunning(detail.batch) : false;
		if (running && !timer) timer = setInterval(load, 3000);
		if (!running && timer) {
			clearInterval(timer);
			timer = null;
		}
	}

	async function run(op: () => Promise<{ batch_id: number }>, kind: 'undo' | 'mute' | 'block') {
		busy = true;
		error = '';
		try {
			const res = await op();
			await goto(`/actions/${res.batch_id}`);
			loading = true;
			await load();
		} catch (e) {
			if (e instanceof NotConnectedError) {
				await startConsent(kind);
				return;
			}
			error = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			busy = false;
		}
	}

	onMount(load);
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});
</script>

<svelte:head>
	<title>Action — Charcoal</title>
</svelte:head>

<div class="page">
	<a href="/actions{asUserSuffix}" class="back-link">← All actions</a>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if notFound || !detail}
		<div class="not-found"><h2>Action not found</h2></div>
	{:else}
		{@const b = detail.batch}
		<div class="header">
			<h1 class="headline">{batchHeadline(b)}</h1>
			<p class="meta">{b.source} · {new Date(b.created_at).toLocaleString()}</p>
			{#if !asUser}
				<div class="controls">
					{#if canUndo(b)}
						<button onclick={() => run(() => undoBatch(b.id), 'undo')} disabled={busy}>Undo all</button>
					{/if}
					{#if canRetry(b)}
						<button onclick={() => run(() => retryBatch(b.id), b.kind)} disabled={busy}>Retry failed</button>
					{/if}
					{#if isParked(b)}
						<button onclick={() => startConsent('undo')} disabled={busy}>Reconnect</button>
					{/if}
				</div>
			{/if}
			{#if error}
				<p class="error">{error}</p>
			{/if}
		</div>

		<table class="rows">
			<thead>
				<tr><th>Account</th><th>Tier then</th><th>Status</th><th></th></tr>
			</thead>
			<tbody>
				{#each detail.actions as r (r.id)}
					<tr>
						<td>
							{#if r.handle}
								<a href="/accounts/{r.handle}{asUserSuffix}">@{r.handle}</a>
							{:else}
								<span class="did">{r.target_did}</span>
							{/if}
						</td>
						<td>
							{#if r.tier_at_action}
								<span class={tierClass(r.tier_at_action)}>{r.tier_at_action}</span>
							{:else}
								—
							{/if}
							{#if driftNote(r)}
								<span class="note">{driftNote(r)}</span>
							{/if}
						</td>
						<td>
							{STATUS_LABEL[r.status]}
							{#if r.status === 'failed' && r.error}
								<span class="note">{r.error}</span>
							{/if}
						</td>
						<td>
							{#if !asUser && b.kind !== 'undo' && r.undo_of === null && (r.status === 'applied' || r.status === 'skipped_already_done')}
								<button class="link" onclick={() => run(() => undoAction(r.id), 'undo')} disabled={busy}>Undo</button>
							{/if}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
</div>

<style>
	.page { max-width: 56rem; margin: 0 auto; padding: 2rem 1rem; }
	.back-link { font-size: 0.875rem; color: var(--charcoal-500); text-decoration: none; }
	.header { margin: 1rem 0 1.5rem; }
	.headline { font-family: 'Outfit', system-ui, sans-serif; font-size: 1.375rem; margin: 0; }
	.meta { font-size: 0.8125rem; color: var(--charcoal-500); margin: 0.25rem 0 0.75rem; }
	.controls { display: flex; gap: 0.5rem; }
	.controls button { padding: 0.375rem 0.75rem; font: inherit; font-size: 0.8125rem; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: 8px; background: transparent; color: var(--charcoal-400); cursor: pointer; }
	.controls button:disabled, .link:disabled { opacity: 0.5; cursor: not-allowed; }
	.link { background: none; border: 0; padding: 0; color: var(--charcoal-400); text-decoration: underline; cursor: pointer; font: inherit; font-size: 0.8125rem; }
	.rows { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
	.rows th { text-align: left; font-weight: 500; color: var(--charcoal-500); padding: 0.5rem 0.75rem; border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); }
	.rows td { padding: 0.5rem 0.75rem; border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.08); vertical-align: top; }
	.rows a { color: inherit; }
	.did { font-family: ui-monospace, monospace; font-size: 0.75rem; color: var(--charcoal-500); }
	.note { display: block; font-size: 0.75rem; color: var(--charcoal-500); }
	.error { color: var(--status-error); font-size: 0.875rem; }
	.not-found { padding: 3rem 0; text-align: center; color: var(--charcoal-500); }
	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }
	.spinner { width: 1.5rem; height: 1.5rem; border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2); border-top-color: var(--charcoal-400); border-radius: 50%; animation: spin 0.8s linear infinite; }
	@keyframes spin { to { transform: rotate(360deg); } }
</style>
