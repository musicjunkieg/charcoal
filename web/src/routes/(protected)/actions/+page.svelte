<script lang="ts">
	// Action log (#315, spec §5.3): connection line + newest-first batches.
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		getActionsStatus,
		listActionBatches,
		disconnectActions,
		startConsent,
		AuthError,
		AccessRevokedError
	} from '$lib/api.js';
	import { batchHeadline, isRunning, isParked } from '$lib/action-status';
	import type { ActionsStatus, ActionBatchSummary } from '$lib/types.js';
	import '$lib/website/styles/tokens.css';

	let asUser = $derived($page.url.searchParams.get('as_user'));
	let asUserSuffix = $derived(asUser ? `?as_user=${encodeURIComponent(asUser)}` : '');
	let actionsError = $derived($page.url.searchParams.get('actions_error'));

	let status = $state<ActionsStatus | null>(null);
	let batches = $state<ActionBatchSummary[]>([]);
	let loading = $state(true);
	let error = $state('');
	let timer: ReturnType<typeof setInterval> | null = null;

	const ERROR_COPY: Record<string, string> = {
		denied: "Bluesky didn't grant permission. Nothing was changed.",
		invalid_scope: 'Bluesky granted different permissions than Charcoal asked for. Nothing was changed.',
		failed: 'Something went wrong while connecting. Nothing was changed.',
		disabled: 'Mute and block actions are not enabled on this server.'
	};

	async function load() {
		try {
			status = await getActionsStatus();
			batches = (await listActionBatches(50, 0)).batches;
		} catch (err) {
			if (err instanceof AuthError) return goto('/login');
			if (err instanceof AccessRevokedError) return goto('/waitlist');
			error = err instanceof Error ? err.message : 'Something went wrong';
		} finally {
			loading = false;
		}
		// Poll while anything is in flight; stop as soon as nothing is.
		const active = batches.some((b) => isRunning(b));
		if (active && !timer) timer = setInterval(load, 3000);
		if (!active && timer) {
			clearInterval(timer);
			timer = null;
		}
	}

	async function disconnect() {
		if (!confirm('Disconnect Charcoal from your Bluesky account? Existing mutes and blocks stay in place.')) return;
		await disconnectActions();
		await load();
	}

	function when(iso: string): string {
		return new Date(iso).toLocaleString();
	}

	onMount(() => {
		if (actionsError) error = ERROR_COPY[actionsError] ?? ERROR_COPY.failed;
		load();
	});
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});
</script>

<svelte:head>
	<title>Actions — Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<h1 class="page-title">Actions</h1>
	</div>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else}
		{#if status && !status.enabled}
			<p class="connection muted">Mute and block actions are not enabled on this server.</p>
		{:else if status?.connected}
			<p class="connection">
				Connected to your Bluesky account for mute and block (fine-grained permissions)
				{#if !asUser}
					· <button class="link" onclick={disconnect}>Disconnect</button>
				{/if}
			</p>
		{:else if batches.some(isParked)}
			<p class="connection warn">
				Not connected — reconnect to continue
				{#if !asUser}
					· <button class="link" onclick={() => startConsent('undo')}>Reconnect</button>
				{/if}
			</p>
		{:else}
			<p class="connection muted">Not connected</p>
		{/if}

		{#if error}
			<p class="error">{error}</p>
		{/if}

		{#if batches.length === 0}
			<p class="empty">Nothing yet. Actions you take from the Accounts page will appear here.</p>
		{:else}
			<ul class="batches">
				{#each batches as b (b.id)}
					<li>
						<a href="/actions/{b.id}{asUserSuffix}" class="batch" class:running={isRunning(b)}>
							<span class="headline">{batchHeadline(b)}</span>
							<span class="meta">{b.source} · {when(b.created_at)}{b.drifted ? ' · some scores have changed' : ''}</span>
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>

<style>
	.page { max-width: 48rem; margin: 0 auto; padding: 2rem 1rem; }
	.page-title { font-family: 'Outfit', system-ui, sans-serif; font-size: 1.5rem; margin: 0 0 1rem; }
	.connection { font-size: 0.875rem; color: var(--charcoal-400); margin: 0 0 1.5rem; }
	.connection.muted { color: var(--charcoal-500); }
	.connection.warn { color: var(--tier-elevated); }
	.link { background: none; border: 0; padding: 0; color: inherit; text-decoration: underline; cursor: pointer; font: inherit; }
	.empty { color: var(--charcoal-500); }
	.error { color: var(--status-error); font-size: 0.875rem; }
	.batches { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.5rem; }
	.batch { display: flex; flex-direction: column; gap: 0.25rem; padding: 0.75rem 1rem; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: 10px; text-decoration: none; color: inherit; }
	.batch:hover { background: rgb(var(--charcoal-400-rgb) / 0.06); }
	.batch.running .headline::after { content: ''; display: inline-block; width: 0.5rem; height: 0.5rem; margin-left: 0.5rem; border-radius: 50%; background: var(--tier-watch); animation: pulse 1.2s infinite; }
	.headline { font-weight: 500; }
	.meta { font-size: 0.75rem; color: var(--charcoal-500); }
	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }
	.spinner { width: 1.5rem; height: 1.5rem; border: 2px solid rgb(var(--charcoal-400-rgb) / 0.2); border-top-color: var(--charcoal-400); border-radius: 50%; animation: spin 0.8s linear infinite; }
	@keyframes spin { to { transform: rotate(360deg); } }
	@keyframes pulse { 50% { opacity: 0.3; } }
</style>
