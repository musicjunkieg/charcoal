<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { getReviewQueue } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import LabelButtons from '$lib/components/LabelButtons.svelte';
	import type { ReviewAccount } from '$lib/types.js';

	const TIER_COLORS: Record<string, string> = {
		High: '#fca5a5',
		Elevated: '#fdba74',
		Watch: '#fcd34d',
		Low: '#a8a29e'
	};

	let accounts = $state<ReviewAccount[]>([]);
	let loading = $state(true);
	let labeled = $state(0);
	let total = $state(0);

	async function loadQueue() {
		try {
			const res = await getReviewQueue(50);
			accounts = res.accounts;
			total = res.total;
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
			}
		} finally {
			loading = false;
		}
	}

	function handleLabeled(did: string) {
		labeled++;
		accounts = accounts.filter((a) => a.did !== did);
	}

	function formatScore(s: number | null): string {
		return s != null ? s.toFixed(2) : '—';
	}

	function formatPct(s: number | null): string {
		return s != null ? `${(s * 100).toFixed(1)}%` : '—';
	}

	onMount(() => {
		loadQueue();
	});
</script>

<svelte:head>
	<title>Review Queue — Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<div>
			<h1 class="page-title">Triage Queue</h1>
			<p class="page-subtitle">
				{#if labeled > 0}
					{labeled} labeled this session
				{:else}
					Label accounts to improve scoring accuracy
				{/if}
			</p>
		</div>
		{#if total > 0}
			<div class="progress-badge">{accounts.length} remaining</div>
		{/if}
	</div>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if accounts.length === 0}
		<div class="empty-state">
			{#if labeled > 0}
				<div class="done-icon">&#10003;</div>
				<h2>All caught up</h2>
				<p>You've labeled {labeled} accounts this session. Run another scan to find more.</p>
			{:else}
				<p>No unlabeled accounts. Run a scan first to detect amplifiers.</p>
			{/if}
			<a href="/dashboard" class="back-btn">Back to dashboard</a>
		</div>
	{:else}
		<div class="review-list">
			{#each accounts as account (account.did)}
				<div class="review-card">
					<div class="card-header">
						<div class="card-identity">
							<a href="/accounts/{account.handle}" class="card-handle">
								@{account.handle}
							</a>
							{#if account.threat_tier}
								<span
									class="card-tier"
									style="color: {TIER_COLORS[account.threat_tier] ?? '#a8a29e'}"
								>
									{account.threat_tier}
								</span>
							{/if}
						</div>
						<a
							href="https://bsky.app/profile/{account.handle}"
							target="_blank"
							rel="noopener noreferrer"
							class="bsky-link"
						>Bluesky ↗</a>
					</div>

					<div class="card-scores">
						<div class="score-pill">
							<span class="score-name">Score</span>
							<span class="score-num">{formatScore(account.threat_score)}</span>
						</div>
						<div class="score-pill">
							<span class="score-name">Toxicity</span>
							<span class="score-num">{formatScore(account.toxicity_score)}</span>
						</div>
						<div class="score-pill">
							<span class="score-name">Overlap</span>
							<span class="score-num">{formatPct(account.topic_overlap)}</span>
						</div>
						{#if account.context_score != null}
							<div class="score-pill">
								<span class="score-name">Context</span>
								<span class="score-num">{formatScore(account.context_score)}</span>
							</div>
						{/if}
					</div>

					<div class="card-actions">
						<LabelButtons
							targetDid={account.did}
							predictedTier={account.threat_tier}
							onlabeled={() => handleLabeled(account.did)}
						/>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.page { max-width: 760px; }

	.page-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 2rem;
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

	.progress-badge {
		padding: 0.375rem 0.875rem;
		font-size: 0.8125rem;
		font-weight: 500;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 8px;
		white-space: nowrap;
	}

	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		padding: 4rem 2rem;
		color: #78716c;
	}

	.empty-state h2 {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.25rem;
		font-weight: 400;
		color: #d6d3d1;
		margin-bottom: 0.5rem;
	}

	.empty-state p {
		font-size: 0.9375rem;
		margin-bottom: 1.5rem;
	}

	.done-icon {
		width: 48px;
		height: 48px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 1.5rem;
		color: #86efac;
		background: rgba(134, 239, 172, 0.1);
		border: 1px solid rgba(134, 239, 172, 0.2);
		border-radius: 50%;
		margin-bottom: 1rem;
	}

	.back-btn {
		padding: 0.5rem 1.25rem;
		font-size: 0.875rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 8px;
		text-decoration: none;
		transition: background 0.2s;
	}

	.back-btn:hover { background: rgba(201, 149, 108, 0.18); }

	.review-list {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.review-card {
		padding: 1.25rem;
		background: rgba(28, 25, 23, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 14px;
		transition: border-color 0.2s;
	}

	.review-card:hover {
		border-color: rgba(168, 162, 158, 0.18);
	}

	.card-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 0.875rem;
	}

	.card-identity {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		min-width: 0;
	}

	.card-handle {
		font-weight: 500;
		font-size: 1rem;
		color: #c9956c;
		text-decoration: none;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.card-handle:hover { color: #e8b48a; }

	.card-tier {
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		flex-shrink: 0;
	}

	.bsky-link {
		font-size: 0.8125rem;
		color: #78716c;
		text-decoration: none;
		flex-shrink: 0;
	}

	.bsky-link:hover { color: #a8a29e; }

	.card-scores {
		display: flex;
		gap: 0.5rem;
		margin-bottom: 1rem;
		flex-wrap: wrap;
	}

	.score-pill {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		padding: 0.25rem 0.625rem;
		background: rgba(12, 10, 9, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.08);
		border-radius: 6px;
	}

	.score-name {
		font-size: 0.6875rem;
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: #57534e;
	}

	.score-num {
		font-size: 0.8125rem;
		color: #d6d3d1;
		font-variant-numeric: tabular-nums;
	}

	.card-actions {
		border-top: 1px solid rgba(168, 162, 158, 0.07);
		padding-top: 0.875rem;
	}

	@media (max-width: 640px) {
		.page-header { flex-direction: column; }
		.card-scores { flex-direction: column; }
	}
</style>
