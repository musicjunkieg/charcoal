<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		getStatus,
		getEvents,
		getAccounts,
		getFingerprint,
		triggerScan,
		getAccuracy
	} from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import { TIER_DESCRIPTIONS } from '$lib/tiers.js';
	import { dashboardView } from '$lib/dashboard-state.js';
	import { topKeywords } from '$lib/fingerprint-keywords.js';
	import { pollActions } from '$lib/poll-actions.js';
	import ScanProgress from '$lib/components/ScanProgress.svelte';
	import type {
		ScanStatus,
		AmplificationEvent,
		AccuracyMetrics,
		Account,
		FingerprintResponse
	} from '$lib/types.js';

	let isImpersonating = $derived(!!$page.url.searchParams.get('as_user'));

	let status = $state<ScanStatus | null>(null);
	let events = $state<AmplificationEvent[]>([]);
	let topAccounts = $state<Account[]>([]);
	let fingerprint = $state<FingerprintResponse | null>(null);
	let accuracy = $state<AccuracyMetrics | null>(null);

	// Top keywords across clusters, heaviest cluster first, deduplicated.
	let keywords = $derived(topKeywords(fingerprint, 12));

	// Which top-level view to render (welcome / all-clear / results).
	let view = $derived(status ? dashboardView(status) : 'results');
	let loading = $state(true);
	let loadError = $state('');
	let scanError = $state('');
	let searchQuery = $state('');

	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let scanning = $state(false);
	let now = $state(Date.now());
	let elapsedTimer: ReturnType<typeof setInterval> | null = null;

	// Non-blocking: 404 (not built yet) and network errors leave it null.
	function loadFingerprint() {
		getFingerprint()
			.then((f) => {
				fingerprint = f;
			})
			.catch(() => {});
	}

	// Refresh the live result panels (events + top threats). Called while a
	// scan runs and once more when it finishes, so results appear without a
	// manual page reload. Non-blocking: failures leave the previous data.
	function refreshResults() {
		getEvents(10)
			.then((e) => {
				events = e.events;
			})
			.catch(() => {});
		getAccounts({ per_page: 5 })
			.then((r) => {
				topAccounts = r.accounts;
			})
			.catch(() => {});
		// The fingerprint is built early in a first scan — pick it up as soon
		// as it exists so the promised "topic fingerprint" becomes visible.
		if (!fingerprint) loadFingerprint();
	}

	async function loadData() {
		loadError = '';
		try {
			// allSettled so one flaky endpoint can't blank the whole dashboard:
			// status is required (rethrown below), but events and top threats
			// degrade independently — their panels just stay empty this load.
			const [s, e, a] = await Promise.allSettled([
				getStatus(),
				getEvents(10),
				getAccounts({ per_page: 5 })
			]);
			if (s.status === 'rejected') throw s.reason;
			status = s.value;
			if (e.status === 'fulfilled') events = e.value.events;
			if (a.status === 'fulfilled') topAccounts = a.value.accounts;
			loadFingerprint();
			// Load accuracy metrics in background (non-blocking)
			getAccuracy()
				.then((m) => {
					accuracy = m;
				})
				.catch(() => {});
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			loadError = err instanceof Error ? err.message : 'Failed to load dashboard';
		} finally {
			loading = false;
		}
	}

	function retryLoad() {
		loading = true;
		loadData();
	}

	function startPolling() {
		if (pollTimer) clearInterval(pollTimer);
		pollTimer = setInterval(async () => {
			if (!status?.scan_running) return;
			const prevRunning = status.scan_running;
			try {
				status = await getStatus();
			} catch {
				return;
			}
			// Refresh partial results while running; on the falling edge (scan
			// just finished) also refresh once more so nothing is stale.
			const actions = pollActions(prevRunning, status.scan_running);
			if (actions.refreshResults) refreshResults();
			if (actions.refreshAccuracy) {
				getAccuracy()
					.then((m) => {
						accuracy = m;
					})
					.catch(() => {});
			}
		}, 5000);

		if (elapsedTimer) clearInterval(elapsedTimer);
		elapsedTimer = setInterval(() => {
			if (status?.scan_running) now = Date.now();
		}, 1000);
	}

	async function handleScan() {
		scanError = '';
		scanning = true;
		try {
			await triggerScan();
			status = await getStatus();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			scanError = err instanceof Error ? err.message : 'Scan failed to start';
		} finally {
			scanning = false;
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

	function elapsedTime(iso: string): string {
		const ms = now - new Date(iso).getTime();
		const secs = Math.floor(ms / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	onMount(() => {
		loadData();
		startPolling();
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		if (elapsedTimer) clearInterval(elapsedTimer);
	});
</script>

<svelte:head>
	<title>Dashboard — Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<div>
			<h1 class="page-title">Threat Intelligence</h1>
			{#if status?.scan_running && status.started_at}
				<p class="page-subtitle scan-in-progress">
					Scan in progress — {elapsedTime(status.started_at)}
				</p>
			{:else if status?.started_at}
				<p class="page-subtitle">Last scan: {timeAgo(status.started_at)}</p>
			{/if}
		</div>

		<div class="scan-area">
			{#if status?.scan_running}
				<div class="scan-running">
					<div class="spinner"></div>
					<span>Scanning…</span>
				</div>
			{:else if !isImpersonating}
				<button class="btn-scan" onclick={handleScan} disabled={scanning}>
					{scanning ? 'Starting…' : 'Trigger Scan'}
				</button>
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
		{#if status.scan_running}
			<ScanProgress {status} elapsed={status.started_at ? elapsedTime(status.started_at) : ''} />
		{/if}

		{#if view === 'welcome'}
			<!-- First-run welcome screen -->
			<div class="welcome">
				<h2 class="welcome-title">Welcome to Charcoal</h2>
				<p class="welcome-text">
					Charcoal scans your Bluesky posting history to identify accounts that may engage with your
					content in hostile ways — before it happens.
				</p>
				<p class="welcome-text">
					Your first scan will build a topic fingerprint from your recent posts, then find who's
					amplifying them and score each account for toxicity and topic overlap. First scans usually
					take 5–15 minutes depending on how many accounts engage with your posts — results appear
					as they're scored, so you can start exploring right away.
				</p>
				{#if !isImpersonating}
					<button class="btn-scan btn-scan-welcome" onclick={handleScan} disabled={scanning}>
						{scanning ? 'Starting…' : 'Start your first scan'}
					</button>
				{/if}
				{#if scanError}
					<p class="scan-error">{scanError}</p>
				{/if}
				{#if status.last_error}
					<p class="scan-error">{status.last_error}</p>
				{/if}
			</div>
		{:else if view === 'all-clear'}
			<!-- Scan finished but found nothing to score — don't strand the user
			     on a grid of zeros with no explanation. -->
			<div class="welcome">
				<h2 class="welcome-title">Scan complete — all clear</h2>
				{#if status.last_error}
					<p class="welcome-text">
						Your last scan hit an error before it could score any accounts. You can safely run it
						again — scans resume where they left off.
					</p>
					<p class="scan-error">{status.last_error}</p>
				{:else}
					<p class="welcome-text">
						Your scan finished and found no accounts that need watching right now. It detected {events.length}
						recent amplification
						{events.length === 1 ? 'event' : 'events'} of your posts.
					</p>
					<p class="welcome-text">
						Re-scan any time — each scan picks up new quotes, reposts, and replies since the last
						one.
					</p>
				{/if}
				{#if !isImpersonating}
					<button class="btn-scan btn-scan-welcome" onclick={handleScan} disabled={scanning}>
						{scanning ? 'Starting…' : 'Scan again'}
					</button>
				{/if}
				{#if scanError}
					<p class="scan-error">{scanError}</p>
				{/if}
			</div>
		{:else}
			{#if status.scan_running && status.tier_counts.total > 0}
				<div class="partial-banner">Partial results — updating as the scan runs.</div>
			{/if}

			<!-- Tier Summary Cards -->
			<div class="tier-grid">
				<a href="/accounts?tier=High" class="tier-card tier-high" title={TIER_DESCRIPTIONS.High}>
					<span class="tier-count">{status.tier_counts.high}</span>
					<span class="tier-label">High</span>
				</a>
				<a
					href="/accounts?tier=Elevated"
					class="tier-card tier-elevated"
					title={TIER_DESCRIPTIONS.Elevated}
				>
					<span class="tier-count">{status.tier_counts.elevated}</span>
					<span class="tier-label">Elevated</span>
				</a>
				<a href="/accounts?tier=Watch" class="tier-card tier-watch" title={TIER_DESCRIPTIONS.Watch}>
					<span class="tier-count">{status.tier_counts.watch}</span>
					<span class="tier-label">Watch</span>
				</a>
				<a href="/accounts?tier=Low" class="tier-card tier-low" title={TIER_DESCRIPTIONS.Low}>
					<span class="tier-count">{status.tier_counts.low}</span>
					<span class="tier-label">Low</span>
				</a>
			</div>

			<!-- Tier legend -->
			<div class="tier-legend">
				{#each Object.entries(TIER_DESCRIPTIONS) as [tier, desc] (tier)}
					<span class="legend-item">
						<span class="legend-tier tier-{tier.toLowerCase()}">{tier}</span>
						<span class="legend-desc">{desc}</span>
					</span>
				{/each}
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

			<!-- Top threats — the direct answer to "who should I watch out for" -->
			{#if topAccounts.length > 0}
				<section class="top-threats">
					<div class="section-header">
						<h2 class="section-title">Top threats{status.scan_running ? ' so far' : ''}</h2>
						<a href="/accounts" class="section-link">View all accounts →</a>
					</div>
					<div class="threat-list">
						{#each topAccounts as acct (acct.did)}
							<a href="/accounts/{acct.handle}" class="threat-row">
								<span class="threat-handle">@{acct.handle}</span>
								<span class="threat-meta">
									{#if acct.threat_tier}
										<span
											class="legend-tier tier-{acct.threat_tier.toLowerCase()}"
											title={TIER_DESCRIPTIONS[acct.threat_tier] ?? ''}>{acct.threat_tier}</span
										>
									{/if}
									{#if acct.threat_score !== null}
										<span class="threat-score">{acct.threat_score.toFixed(1)}</span>
									{/if}
								</span>
							</a>
						{/each}
					</div>
				</section>
			{/if}

			<!-- Accuracy Metrics -->
			{#if accuracy && accuracy.total_labeled >= 5}
				<section class="accuracy-panel">
					<div class="accuracy-header">
						<h2 class="section-title">Scoring Accuracy</h2>
						<a href="/review" class="section-link">Review more →</a>
					</div>
					<div class="accuracy-grid">
						<div class="accuracy-stat">
							<span class="accuracy-num" style="color: #86efac"
								>{(accuracy.accuracy * 100).toFixed(0)}%</span
							>
							<span class="accuracy-label">Accuracy</span>
						</div>
						<div class="accuracy-stat">
							<span class="accuracy-num">{accuracy.total_labeled}</span>
							<span class="accuracy-label">Labeled</span>
						</div>
						<div class="accuracy-stat">
							<span class="accuracy-num" style="color: #fdba74">{accuracy.overscored}</span>
							<span class="accuracy-label">Overscored</span>
						</div>
						<div class="accuracy-stat">
							<span class="accuracy-num" style="color: #fcd34d">{accuracy.underscored}</span>
							<span class="accuracy-label">Underscored</span>
						</div>
					</div>
					{#if accuracy.overscored > accuracy.underscored}
						<p class="accuracy-hint">
							Charcoal is flagging more accounts than warranted. Your labels help calibrate.
						</p>
					{:else if accuracy.underscored > accuracy.overscored}
						<p class="accuracy-hint">
							Charcoal is missing some threats. Your labels help it learn.
						</p>
					{/if}
				</section>
			{/if}

			<!-- Recent Events -->
			{#if events.length > 0}
				<section class="events-section">
					<div class="section-header">
						<h2 class="section-title">Recent Amplification Events</h2>
						<a href="/accounts" class="section-link">View all accounts →</a>
					</div>

					<div class="events-list">
						{#each events as event, i (event.id || i)}
							<div class="event-row">
								<div class="event-info">
									<a href="/accounts/{event.amplifier_handle}" class="event-handle"
										>@{event.amplifier_handle}</a
									>
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
											class="event-link">View post ↗</a
										>
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

			<!-- Topic fingerprint — the topics Charcoal extracted from the user's posts -->
			{#if keywords.length > 0 && fingerprint?.fingerprint}
				<details class="fingerprint-card">
					<summary class="fingerprint-summary">Your topic fingerprint</summary>
					<p class="fingerprint-hint">
						Built from {fingerprint.fingerprint.post_count} of your recent posts. Charcoal measures how
						much a potential threat's posting overlaps with these topics — overlap plus hostility is what
						raises an account's tier.
					</p>
					<div class="fingerprint-chips">
						{#each keywords as keyword (keyword)}
							<span class="chip">{keyword}</span>
						{/each}
					</div>
				</details>
			{/if}
		{/if}
	{:else}
		<!-- Status failed to load (non-auth error) — offer a retry instead of a blank page -->
		<div class="load-error">
			<p class="load-error-text">
				Couldn't load the dashboard{loadError ? ` — ${loadError}` : ''}.
			</p>
			<button class="btn-scan" onclick={retryLoad}>Retry</button>
		</div>
	{/if}
</div>

<style>
	.page {
		max-width: 900px;
	}

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
		transition:
			transform 0.2s,
			box-shadow 0.2s;
		box-shadow: 0 4px 12px -2px rgba(245, 158, 11, 0.35);
	}

	.btn-scan:hover {
		transform: translateY(-1px);
		box-shadow: 0 6px 16px -2px rgba(245, 158, 11, 0.45);
	}

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

	.load-error {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1rem;
		padding: 4rem 2rem;
		text-align: center;
	}

	.load-error-text {
		font-size: 0.9375rem;
		color: #a8a29e;
	}

	.spinner {
		width: 24px;
		height: 24px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	.spinner.large {
		width: 40px;
		height: 40px;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

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
		transition:
			transform 0.2s,
			box-shadow 0.2s,
			border-color 0.2s;
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

	.tier-high {
		color: #fca5a5;
	}
	.tier-elevated {
		color: #fdba74;
	}
	.tier-watch {
		color: #fcd34d;
	}
	.tier-low {
		color: #a8a29e;
	}

	/* Partial-results banner */
	.partial-banner {
		padding: 0.5rem 1rem;
		margin-bottom: 1rem;
		font-size: 0.8125rem;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.08);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 10px;
		text-align: center;
	}

	/* Tier legend */
	.tier-legend {
		display: flex;
		flex-wrap: wrap;
		gap: 0.375rem 1.25rem;
		margin: -1rem 0 2rem 0;
		padding: 0 0.25rem;
	}

	.legend-item {
		display: inline-flex;
		align-items: baseline;
		gap: 0.375rem;
		font-size: 0.75rem;
	}

	.legend-tier {
		font-weight: 600;
		font-size: 0.6875rem;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.legend-desc {
		color: #57534e;
	}

	/* Top threats */
	.top-threats {
		margin-bottom: 2.5rem;
	}

	.threat-list {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.threat-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.625rem 1rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.07);
		border-radius: 10px;
		text-decoration: none;
		transition: border-color 0.2s;
	}

	.threat-row:hover {
		border-color: rgba(201, 149, 108, 0.3);
	}

	.threat-handle {
		font-weight: 500;
		color: #c9956c;
		font-size: 0.9375rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.threat-meta {
		display: flex;
		align-items: baseline;
		gap: 0.75rem;
		flex-shrink: 0;
	}

	.threat-score {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 0.9375rem;
		color: #d6d3d1;
		font-variant-numeric: tabular-nums;
	}

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

	.search-at {
		color: #57534e;
		font-size: 1rem;
		margin-right: 0.25rem;
	}

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

	.search-input::placeholder {
		color: #44403c;
	}

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

	.search-btn:hover {
		background: rgba(201, 149, 108, 0.18);
	}

	/* Events */
	.events-section {
		margin-top: 1rem;
	}

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

	.section-link:hover {
		color: #e8b48a;
	}

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

	.event-info {
		flex: 1;
		min-width: 0;
	}

	.event-handle {
		font-weight: 500;
		color: #c9956c;
		text-decoration: none;
		font-size: 0.9375rem;
	}

	.event-handle:hover {
		color: #e8b48a;
	}

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

	.event-time {
		font-size: 0.8125rem;
		color: #57534e;
	}

	.event-link {
		font-size: 0.75rem;
		color: #78716c;
		text-decoration: none;
	}

	.event-link:hover {
		color: #a8a29e;
	}

	.empty-state {
		padding: 3rem 0;
		text-align: center;
		color: #57534e;
		font-size: 0.9375rem;
	}

	/* Topic fingerprint card */
	.fingerprint-card {
		margin-top: 2rem;
		padding: 1rem 1.25rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 14px;
	}

	.fingerprint-summary {
		font-size: 1rem;
		font-weight: 500;
		color: #d6d3d1;
		cursor: pointer;
		letter-spacing: 0.01em;
	}

	.fingerprint-summary:hover {
		color: #e8b48a;
	}

	.fingerprint-hint {
		font-size: 0.8125rem;
		color: #78716c;
		line-height: 1.5;
		margin: 0.75rem 0;
	}

	.fingerprint-chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.chip {
		padding: 0.25rem 0.75rem;
		font-size: 0.8125rem;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.08);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 999px;
	}

	.btn-scan:disabled {
		opacity: 0.6;
		cursor: not-allowed;
		transform: none;
		box-shadow: none;
	}

	.scan-in-progress {
		color: #c9956c;
	}

	/* Welcome screen */
	.welcome {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		padding: 4rem 2rem;
		max-width: 520px;
		margin: 0 auto;
	}

	.welcome-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.5rem;
		font-weight: 400;
		color: #fffbeb;
		margin-bottom: 1.25rem;
	}

	.welcome-text {
		font-size: 0.9375rem;
		color: #a8a29e;
		line-height: 1.6;
		margin-bottom: 1rem;
	}

	.btn-scan-welcome {
		margin-top: 1rem;
		padding: 0.75rem 2rem;
		font-size: 1rem;
	}

	/* Accuracy Panel */
	.accuracy-panel {
		margin-bottom: 2.5rem;
		padding: 1.25rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 14px;
	}

	.accuracy-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 1rem;
	}

	.accuracy-grid {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 1rem;
	}

	.accuracy-stat {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.25rem;
	}

	.accuracy-num {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.5rem;
		font-weight: 400;
		color: #d6d3d1;
		line-height: 1;
	}

	.accuracy-label {
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: #57534e;
	}

	.accuracy-hint {
		font-size: 0.8125rem;
		color: #78716c;
		margin-top: 0.875rem;
		text-align: center;
	}

	@media (max-width: 640px) {
		.tier-grid {
			grid-template-columns: repeat(2, 1fr);
		}
		.accuracy-grid {
			grid-template-columns: repeat(2, 1fr);
		}
		.page-header {
			flex-direction: column;
		}
		.scan-area {
			align-items: flex-start;
		}
	}
</style>
