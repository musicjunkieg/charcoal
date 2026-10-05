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
		NotFoundError,
		AuthError,
		AccessRevokedError
	} from '$lib/api.js';
	import {
		batchHeadline,
		driftNote,
		isRunning,
		isParked,
		canRetry,
		canUndo,
		bannerSummary,
		returnPath
	} from '$lib/action-status';
	import { POLL_INTERVAL_MS } from '$lib/action-progress';
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
	let timer: ReturnType<typeof setTimeout> | null = null;
	let inflight: Promise<void> | null = null;
	let inflightFor: number | null = null;
	let destroyed = false;

	/** The banner replaces the header controls once the runner is done and
	 *  nobody is waiting on a reconnect (spec §4). */
	let finished = $derived(detail ? !isRunning(detail.batch) && !isParked(detail.batch) : false);

	const STATUS_LABEL: Record<ActionRowView['status'], string> = {
		pending: 'Pending',
		applied: 'Done',
		skipped_already_done: 'Already done',
		failed: 'Failed',
		undone: 'Undone'
	};

	/** One read of the batch, then — if it is still running — schedule the
	 *  next one. Only ever one request in flight: a caller that arrives
	 *  while a read is pending (the poll timer, or `run()` after an undo)
	 *  joins that read instead of starting a second, so an older response
	 *  can never land after a newer one and flip a finished batch back to
	 *  running. */
	function load(): Promise<void> {
		const id = Number($page.params.id);
		if (inflight && inflightFor === id) return inflight;
		// A read for a different batch is still pending (Undo all / Retry
		// just navigated here): let it land, then read this one.
		if (inflight) return inflight.then(load);
		inflightFor = id;
		inflight = loadOnce(id).finally(() => (inflight = null));
		return inflight;
	}

	async function loadOnce(id: number) {
		if (timer) {
			clearTimeout(timer);
			timer = null;
		}
		// Reset both first: this runs every second while a batch is in
		// flight, and a single dropped poll must not latch the page into an
		// error state while the runner is still applying blocks behind it.
		notFound = false;
		error = '';
		try {
			detail = await getActionBatch(id);
		} catch (err) {
			if (err instanceof AuthError) return goto('/login');
			if (err instanceof AccessRevokedError) return goto('/waitlist');
			if (err instanceof NotFoundError) notFound = true;
			// Anything else is a blip, not a missing batch — say so and keep
			// the last-good detail on screen.
			else error = err instanceof Error ? err.message : 'Something went wrong';
		} finally {
			loading = false;
		}
		const running = detail ? isRunning(detail.batch) : false;
		// 1 s: one small JSON read; the old 3 s made a 1 s action read as 3 s+ (#332).
		// setTimeout, not setInterval: the next poll is armed only after this
		// one has finished, so a slow response can't stack requests.
		if (running && !destroyed) timer = setTimeout(load, POLL_INTERVAL_MS);
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
		destroyed = true;
		if (timer) clearTimeout(timer);
	});
</script>

<svelte:head>
	<title>Action — Charcoal</title>
</svelte:head>

<div class="page">
	<a href="/actions{asUserSuffix}" class="back-link">← All actions</a>

	<!-- Outside the branch below so a load failure is readable whether or not
	     there is a last-good batch to keep showing. -->
	{#if error}
		<p class="error">{error}</p>
	{/if}

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if notFound}
		<div class="not-found"><h2>Action not found</h2></div>
	{:else if detail}
		{@const b = detail.batch}
		<div class="header">
			<h1 class="headline">{batchHeadline(b)}</h1>
			<p class="meta">{b.source} · {new Date(b.created_at).toLocaleString()}</p>
			{#if !asUser && !finished}
				<div class="controls">
					{#if isParked(b)}
						<button onclick={() => startConsent('undo')} disabled={busy}>Reconnect</button>
					{/if}
				</div>
			{/if}
		</div>

		{#if finished}
			{@const s = bannerSummary(b, detail.actions)}
			{@const back = returnPath(b.source, asUserSuffix)}
			<div class="banner" data-tone={s.tone} role="status">
				<div class="banner-text">
					<strong>{s.title}</strong>
					<span class="banner-detail">· {s.detail}</span>
				</div>
				<div class="banner-actions">
					{#if !asUser && canRetry(b)}
						<button onclick={() => run(() => retryBatch(b.id), b.kind)} disabled={busy}>Retry failed</button>
					{/if}
					{#if !asUser && canUndo(b)}
						<button onclick={() => run(() => undoBatch(b.id), 'undo')} disabled={busy}>Undo all</button>
					{/if}
					<a class="banner-back" href={back.href}>{back.label}</a>
				</div>
			</div>
		{/if}

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
							<!-- Undo only what Charcoal applied. A `skipped_already_done`
							     row is the user's own mute or block (#261). -->
							{#if !asUser && b.kind !== 'undo' && r.undo_of === null && r.status === 'applied'}
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
	.page { max-width: 56rem; margin: 0 auto; padding: var(--space-32) var(--space-16); }
	.back-link { font-size: var(--text-small); color: var(--charcoal-500); text-decoration: none; }
	.header { margin: var(--space-16) 0 var(--space-24); }
	.headline { font-family: 'Outfit', system-ui, sans-serif; font-size: var(--text-title); margin: 0; }
	.meta { font-size: var(--text-label); color: var(--charcoal-500); margin: var(--space-4) 0 var(--space-12); }
	.controls { display: flex; gap: var(--space-8); }
	.controls button { padding: var(--space-6) var(--space-12); font: inherit; font-size: var(--text-label); border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: var(--radius-8); background: transparent; color: var(--charcoal-400); cursor: pointer; }
	.controls button:disabled, .link:disabled { opacity: 0.5; cursor: not-allowed; }
	.banner { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-12); margin: 0 0 var(--space-20); padding: var(--space-12) var(--space-16); border-radius: var(--radius-8); border: 1px solid rgb(var(--status-ok-rgb) / 0.3); background: rgb(var(--status-ok-rgb) / 0.06); font-size: var(--text-small); }
	.banner[data-tone='error'] { border-color: rgb(var(--status-error-rgb) / 0.3); background: rgb(var(--status-error-rgb) / 0.06); }
	.banner-text strong { font-weight: 500; }
	.banner-detail { color: var(--charcoal-400); }
	.banner-actions { display: flex; align-items: center; gap: var(--space-8); }
	.banner-actions button { padding: var(--space-6) var(--space-12); font: inherit; font-size: var(--text-label); border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: var(--radius-8); background: transparent; color: var(--charcoal-400); cursor: pointer; }
	.banner-actions button:disabled { opacity: 0.5; cursor: not-allowed; }
	.banner-back { font-size: var(--text-label); color: var(--copper); text-decoration: none; }
	.banner-back:hover { text-decoration: underline; }
	.link { background: none; border: 0; padding: 0; color: var(--charcoal-400); text-decoration: underline; cursor: pointer; font: inherit; font-size: var(--text-label); }
	.rows { width: 100%; border-collapse: collapse; font-size: var(--text-small); }
	.rows th { text-align: left; font-weight: 500; color: var(--charcoal-500); padding: var(--space-8) var(--space-12); border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); }
	.rows td { padding: var(--space-8) var(--space-12); border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.08); vertical-align: top; }
	.rows a { color: inherit; }
	.did { font-family: ui-monospace, monospace; font-size: var(--text-caption); color: var(--charcoal-500); }
	.note { display: block; font-size: var(--text-caption); color: var(--charcoal-500); }
	.error { color: var(--status-error); font-size: var(--text-small); }
	.not-found { padding: var(--space-48) 0; text-align: center; color: var(--charcoal-500); }
	.loading-state { display: flex; justify-content: center; padding: var(--space-64) 0; }
	.spinner { width: 1.5rem; height: 1.5rem; border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2); border-top-color: var(--charcoal-400); border-radius: var(--radius-circle); animation: spin 0.8s linear infinite; }
	@keyframes spin { to { transform: rotate(360deg); } }
</style>
