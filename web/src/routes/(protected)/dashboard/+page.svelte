<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { getStatus, getEvents, triggerScan } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import type { ScanStatus, AmplificationEvent } from '$lib/types.js';

	let status = $state<ScanStatus | null>(null);
	let events = $state<AmplificationEvent[]>([]);
	let loading = $state(true);
	let scanError = $state('');
	let searchQuery = $state('');

	let pollTimer: ReturnType<typeof setInterval> | null = null;

	async function loadData() {
		try {
			const [s, e] = await Promise.all([getStatus(), getEvents(10)]);
			status = s;
			events = e.events;
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
			}
		} finally {
			loading = false;
		}
	}

	function startPolling() {
		if (pollTimer) clearInterval(pollTimer);
		pollTimer = setInterval(async () => {
			if (status?.scan_running) {
				try {
					status = await getStatus();
				} catch {}
			}
		}, 5000);
	}

	async function handleScan() {
		scanError = '';
		try {
			await triggerScan();
			status = await getStatus();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			scanError = err instanceof Error ? err.message : 'Scan failed to start';
		}
	}

	function handleSearch(e: KeyboardEvent | MouseEvent) {
		if (e instanceof KeyboardEvent && e.key !== 'Enter') return;
		if (searchQuery.trim()) {
			goto(`/accounts?q=${encodeURIComponent(searchQuery.trim())}`);
		}
	}

	function formatDate(iso: string): string {
		try {
			return new Intl.DateTimeFormat('en-US', {
				month: 'short',
				day: 'numeric',
				hour: '2-digit',
				minute: '2-digit'
			}).format(new Date(iso));
		} catch {
			return iso;
		}
	}

	function timeAgo(iso: string): string {
		try {
			const diff = Date.now() - new Date(iso).getTime();
			const hours = Math.floor(diff / 3600000);
			if (hours < 1) return 'just now';
			if (hours < 24) return `${hours}h ago`;
			return `${Math.floor(hours / 24)}d ago`;
		} catch {
			return '';
		}
	}

	onMount(() => {
		loadData();
		startPolling();
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<svelte:head>
	<title>Dashboard — Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<div>
			<h1 class="page-title">Threat Intelligence</h1>
			{#if status?.started_at}
				<p class="page-subtitle">Last scan: {timeAgo(status.started_at)}</p>
			{/if}
		</div>

		<div class="scan-area">
			{#if status?.scan_running}
				<div class="scan-running">
					<div class="spinner"></div>
					<span>{status.progress_message || 'Scanning…'}</span>
				</div>
			{:else}
				<button class="btn-scan" onclick={handleScan}>Trigger Scan</button>
			{/if}
			{#if scanError}
				<p class="scan-error">{scanError}</p>
			{/if}
		</div>
	</div>

	{#if loading}
		<div class="loading-state">
			<div class="spinner large"></div>
		</div>
	{:else if status}
		<!-- Tier Summary Cards -->
		<div class="tier-grid">
			<a href="/accounts?tier=High" class="tier-card tier-high">
				<span class="tier-count">{status.tier_counts.high}</span>
				<span class="tier-label">High</span>
			</a>
			<a href="/accounts?tier=Elevated" class="tier-card tier-elevated">
				<span class="tier-count">{status.tier_counts.elevated}</span>
				<span class="tier-label">Elevated</span>
			</a>
			<a href="/accounts?tier=Watch" class="tier-card tier-watch">
				<span class="tier-count">{status.tier_counts.watch}</span>
				<span class="tier-label">Watch</span>
			</a>
			<a href="/accounts?tier=Low" class="tier-card tier-low">
				<span class="tier-count">{status.tier_counts.low}</span>
				<span class="tier-label">Low</span>
			</a>
		</div>

		<!-- Handle Search -->
		<div class="search-box">
			<span class="search-at">@</span>
			<input
				type="text"
				class="search-input"
				placeholder="Search handle..."
				bind:value={searchQuery}
				onkeydown={handleSearch}
			/>
			<button class="search-btn" onclick={handleSearch}>Search</button>
		</div>

		<!-- Recent Events -->
		{#if events.length > 0}
			<section class="events-section">
				<div class="section-header">
					<h2 class="section-title">Recent Amplification Events</h2>
					<a href="/accounts" class="section-link">View all accounts →</a>
				</div>

				<div class="events-list">
					{#each events as event}
						<div class="event-row">
							<div class="event-info">
								<a
									href="/accounts/{event.amplifier_handle}"
									class="event-handle"
								>@{event.amplifier_handle}</a>
								<span class="event-type">{event.event_type.replace('_', ' ')}</span>
								{#if event.amplifier_text}
									<p class="event-text">"{event.amplifier_text}"</p>
								{/if}
							</div>
							<div class="event-meta">
								<span class="event-time">{timeAgo(event.detected_at)}</span>
								{#if event.amplifier_post_uri}
									<a
										href={event.amplifier_post_uri}
										target="_blank"
										rel="noopener noreferrer"
										class="event-link"
									>View post ↗</a>
								{/if}
							</div>
						</div>
					{/each}
				</div>
			</section>
		{:else}
			<div class="empty-state">
				<p>No amplification events yet. Run a scan to detect quotes and reposts.</p>
			</div>
		{/if}
	{/if}
</div>

<style>
	.page { max-width: 900px; }

	.page-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1.5rem;
		margin-bottom: 2rem;
		flex-wrap: wrap;
	}

	.page-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.75rem;
		font-weight: 400;
		color: #fffbeb;
		letter-spacing: -0.01em;
	}

	.page-subtitle {
		font-size: 0.875rem;
		color: #78716c;
		margin-top: 0.25rem;
	}

	.scan-area {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.5rem;
	}

	.btn-scan {
		padding: 0.625rem 1.25rem;
		font-size: 0.9375rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #0c0a09;
		background: linear-gradient(135deg, #f59e0b 0%, #c9956c 100%);
		border: none;
		border-radius: 10px;
		cursor: pointer;
		transition: transform 0.2s, box-shadow 0.2s;
		box-shadow: 0 4px 12px -2px rgba(245, 158, 11, 0.35);
	}

	.btn-scan:hover { transform: translateY(-1px); box-shadow: 0 6px 16px -2px rgba(245, 158, 11, 0.45); }

	.scan-running {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		color: #c9956c;
		font-size: 0.875rem;
	}

	.scan-error {
		font-size: 0.8125rem;
		color: #f87171;
		text-align: right;
	}

	.loading-state {
		display: flex;
		justify-content: center;
		padding: 4rem 0;
	}

	.spinner {
		width: 24px;
		height: 24px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	.spinner.large { width: 40px; height: 40px; }
	@keyframes spin { to { transform: rotate(360deg); } }

	/* Tier Cards */
	.tier-grid {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 1rem;
		margin-bottom: 2rem;
	}

	.tier-card {
		display: flex;
		flex-direction: column;
		align-items: center;
		padding: 1.5rem 1rem;
		border-radius: 14px;
		border: 1px solid rgba(168, 162, 158, 0.1);
		text-decoration: none;
		transition: transform 0.2s, box-shadow 0.2s, border-color 0.2s;
		background: rgba(28, 25, 23, 0.6);
	}

	.tier-card:hover {
		transform: translateY(-2px);
		border-color: rgba(201, 149, 108, 0.3);
		box-shadow: 0 8px 24px -4px rgba(0, 0, 0, 0.4);
	}

	.tier-count {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 2.5rem;
		font-weight: 400;
		line-height: 1;
		margin-bottom: 0.5rem;
	}

	.tier-label {
		font-size: 0.8125rem;
		font-weight: 500;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		opacity: 0.7;
	}

	.tier-high { color: #fca5a5; }
	.tier-elevated { color: #fdba74; }
	.tier-watch { color: #fcd34d; }
	.tier-low { color: #a8a29e; }

	/* Search */
	.search-box {
		display: flex;
		align-items: center;
		background: rgba(12, 10, 9, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.15);
		border-radius: 12px;
		padding: 0 1rem;
		margin-bottom: 2.5rem;
		transition: border-color 0.2s;
	}

	.search-box:focus-within {
		border-color: #c9956c;
		box-shadow: 0 0 0 3px rgba(201, 149, 108, 0.12);
	}

	.search-at { color: #57534e; font-size: 1rem; margin-right: 0.25rem; }

	.search-input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 0.875rem 0;
		font-size: 0.9375rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #fef3c7;
		outline: none;
	}

	.search-input::placeholder { color: #44403c; }

	.search-btn {
		padding: 0.5rem 1rem;
		font-size: 0.875rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 8px;
		cursor: pointer;
		transition: background 0.2s;
	}

	.search-btn:hover { background: rgba(201, 149, 108, 0.18); }

	/* Events */
	.events-section { margin-top: 1rem; }

	.section-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 1rem;
	}

	.section-title {
		font-size: 1rem;
		font-weight: 500;
		color: #d6d3d1;
		letter-spacing: 0.01em;
	}

	.section-link {
		font-size: 0.8125rem;
		color: #c9956c;
		text-decoration: none;
	}

	.section-link:hover { color: #e8b48a; }

	.events-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.event-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.875rem 1rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.07);
		border-radius: 10px;
	}

	.event-info { flex: 1; min-width: 0; }

	.event-handle {
		font-weight: 500;
		color: #c9956c;
		text-decoration: none;
		font-size: 0.9375rem;
	}

	.event-handle:hover { color: #e8b48a; }

	.event-type {
		font-size: 0.8125rem;
		color: #78716c;
		margin-left: 0.5rem;
	}

	.event-text {
		font-size: 0.8125rem;
		color: #a8a29e;
		margin-top: 0.25rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.event-meta {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.25rem;
		flex-shrink: 0;
	}

	.event-time { font-size: 0.8125rem; color: #57534e; }

	.event-link {
		font-size: 0.75rem;
		color: #78716c;
		text-decoration: none;
	}

	.event-link:hover { color: #a8a29e; }

	.empty-state {
		padding: 3rem 0;
		text-align: center;
		color: #57534e;
		font-size: 0.9375rem;
	}

	@media (max-width: 640px) {
		.tier-grid { grid-template-columns: repeat(2, 1fr); }
		.page-header { flex-direction: column; }
		.scan-area { align-items: flex-start; }
	}
</style>
