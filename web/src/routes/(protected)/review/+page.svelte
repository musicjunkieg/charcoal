<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { getReviewQueue } from '$lib/api.js';
	import { AuthError, AccessRevokedError } from '$lib/api.js';
	import LabelButtons from '$lib/components/LabelButtons.svelte';
	import type { ReviewAccount } from '$lib/types.js';
	import { tierClass } from '$lib/tier-class';
	import '$lib/website/styles/tokens.css';
	import '$lib/website/styles/tiers.css';

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
			} else if (err instanceof AccessRevokedError) {
				await goto('/waitlist');
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
								<span class="card-tier {tierClass(account.threat_tier)}">
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
		gap: var(--space-16);
		margin-bottom: var(--space-32);
	}

	.page-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: var(--text-page-title);
		font-weight: 400;
		color: var(--cream-50);
		letter-spacing: -0.01em;
	}

	.page-subtitle {
		font-size: var(--text-small);
		color: var(--charcoal-500);
		margin-top: var(--space-4);
	}

	.progress-badge {
		padding: var(--space-6) var(--space-14);
		font-size: var(--text-label);
		font-weight: 500;
		color: var(--copper);
		background: rgb(var(--copper-rgb) / 0.1);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
		border-radius: var(--radius-8);
		white-space: nowrap;
	}

	.loading-state { display: flex; justify-content: center; padding: var(--space-64) 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: var(--radius-circle);
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		padding: var(--space-64) var(--space-32);
		color: var(--charcoal-500);
	}

	.empty-state h2 {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: var(--text-title);
		font-weight: 400;
		color: var(--charcoal-300);
		margin-bottom: var(--space-8);
	}

	.empty-state p {
		font-size: var(--text-body-sm);
		margin-bottom: var(--space-24);
	}

	.done-icon {
		width: 48px;
		height: 48px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: var(--text-section-title);
		color: var(--status-ok);
		background: rgb(var(--status-ok-rgb) / 0.1);
		border: 1px solid rgb(var(--status-ok-rgb) / 0.2);
		border-radius: var(--radius-circle);
		margin-bottom: var(--space-16);
	}

	.back-btn {
		padding: var(--space-8) var(--space-20);
		font-size: var(--text-small);
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--copper);
		background: rgb(var(--copper-rgb) / 0.1);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
		border-radius: var(--radius-8);
		text-decoration: none;
		transition: background 0.2s;
	}

	.back-btn:hover { background: rgb(var(--copper-rgb) / 0.18); }

	.review-list {
		display: flex;
		flex-direction: column;
		gap: var(--space-12);
	}

	.review-card {
		padding: var(--space-20);
		background: rgb(var(--charcoal-900-rgb) / 0.6);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.1);
		border-radius: var(--radius-14);
		transition: border-color 0.2s;
	}

	.review-card:hover {
		border-color: rgb(var(--charcoal-400-rgb) / 0.18);
	}

	.card-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-12);
		margin-bottom: var(--space-14);
	}

	.card-identity {
		display: flex;
		align-items: center;
		gap: var(--space-10);
		min-width: 0;
	}

	.card-handle {
		font-weight: 500;
		font-size: var(--text-body);
		color: var(--copper);
		text-decoration: none;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.card-handle:hover { color: var(--copper-light); }

	.card-tier {
		font-size: var(--text-caption);
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		flex-shrink: 0;
	}

	.bsky-link {
		font-size: var(--text-label);
		color: var(--charcoal-500);
		text-decoration: none;
		flex-shrink: 0;
	}

	.bsky-link:hover { color: var(--charcoal-400); }

	.card-scores {
		display: flex;
		gap: var(--space-8);
		margin-bottom: var(--space-16);
		flex-wrap: wrap;
	}

	.score-pill {
		display: flex;
		align-items: center;
		gap: var(--space-6);
		padding: var(--space-4) var(--space-10);
		background: rgb(var(--charcoal-950-rgb) / 0.5);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.08);
		border-radius: var(--radius-6);
	}

	.score-name {
		font-size: var(--text-micro);
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--charcoal-600);
	}

	.score-num {
		font-size: var(--text-label);
		color: var(--charcoal-300);
		font-variant-numeric: tabular-nums;
	}

	.card-actions {
		border-top: 1px solid rgb(var(--charcoal-400-rgb) / 0.07);
		padding-top: var(--space-14);
	}

	@media (max-width: 640px) {
		.page-header { flex-direction: column; }
		.card-scores { flex-direction: column; }
	}
</style>
