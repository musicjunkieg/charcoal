<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { getAccount } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import type { Account } from '$lib/types.js';

	const TIER_COLORS: Record<string, string> = {
		High: '#fca5a5',
		Elevated: '#fdba74',
		Watch: '#fcd34d',
		Low: '#a8a29e'
	};

	let account = $state<Account | null>(null);
	let loading = $state(true);
	let notFound = $state(false);

	function formatScore(s: number | null): string {
		return s != null ? s.toFixed(2) : '—';
	}

	function formatPct(s: number | null): string {
		return s != null ? `${(s * 100).toFixed(1)}%` : '—';
	}

	function scoreBar(s: number | null, max = 1.0): number {
		if (s == null) return 0;
		return Math.min(100, (s / max) * 100);
	}

	onMount(async () => {
		const handle = $page.params.handle;
		try {
			account = await getAccount(handle);
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			if (err instanceof Error && err.message === 'HTTP 404') {
				notFound = true;
			}
		} finally {
			loading = false;
		}
	});
</script>

<svelte:head>
	<title>{account ? `@${account.handle}` : 'Account'} — Charcoal</title>
</svelte:head>

<div class="page">
	<a href="/accounts" class="back-link">← All accounts</a>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if notFound}
		<div class="not-found">
			<h2>Account not found</h2>
			<p>@{$page.params.handle} hasn't been scored yet.</p>
		</div>
	{:else if account}
		<div class="account-header">
			<div>
				<h1 class="handle">@{account.handle}</h1>
				<p class="did">{account.did}</p>
			</div>
			<a
				href="https://bsky.app/profile/{account.handle}"
				target="_blank"
				rel="noopener noreferrer"
				class="bsky-link"
			>View on Bluesky ↗</a>
		</div>

		<!-- Score Overview -->
		<div class="score-grid">
			<div class="score-card">
				<div class="score-value">{formatScore(account.threat_score)}</div>
				<div class="score-label">Threat Score</div>
			</div>
			<div class="score-card">
				{#if account.threat_tier}
					<div class="score-value" style="color: {TIER_COLORS[account.threat_tier] ?? '#a8a29e'}">
						{account.threat_tier}
					</div>
				{:else}
					<div class="score-value muted">—</div>
				{/if}
				<div class="score-label">Tier</div>
			</div>
			<div class="score-card">
				<div class="score-value">{formatScore(account.toxicity_score)}</div>
				<div class="score-label">Toxicity</div>
			</div>
			<div class="score-card">
				<div class="score-value">{formatPct(account.topic_overlap)}</div>
				<div class="score-label">Topic Overlap</div>
			</div>
		</div>

		<p class="meta">
			{account.posts_analyzed} posts analyzed &nbsp;·&nbsp; Scored {account.scored_at.slice(0, 10)}
		</p>

		<!-- Behavioral Signals -->
		<section class="section">
			<h2 class="section-title">Behavioral Signals</h2>
			{#if account.behavioral_signals}
				{@const b = account.behavioral_signals}
				<div class="signals-grid">
					<div class="signal-row">
						<span class="signal-label">Quote ratio</span>
						<div class="signal-bar-wrap">
							<div class="signal-bar" style="width: {scoreBar(b.quote_ratio)}%"></div>
						</div>
						<span class="signal-value">{formatPct(b.quote_ratio ?? null)}</span>
					</div>
					<div class="signal-row">
						<span class="signal-label">Reply ratio</span>
						<div class="signal-bar-wrap">
							<div class="signal-bar" style="width: {scoreBar(b.reply_ratio)}%"></div>
						</div>
						<span class="signal-value">{formatPct(b.reply_ratio ?? null)}</span>
					</div>
					<div class="signal-row">
						<span class="signal-label">Avg engagement</span>
						<div class="signal-value alone">{b.avg_engagement?.toFixed(1) ?? '—'}</div>
					</div>
					<div class="signal-row">
						<span class="signal-label">Pile-on participant</span>
						<div class="signal-value alone {b.is_pile_on_participant ? 'warn' : ''}">
							{b.is_pile_on_participant ? 'Yes' : 'No'}
						</div>
					</div>
					<div class="signal-row">
						<span class="signal-label">Benign gate applied</span>
						<div class="signal-value alone">{b.benign_gate_applied ? 'Yes' : 'No'}</div>
					</div>
					{#if b.hostile_multiplier != null && b.hostile_multiplier > 1.0}
						<div class="signal-row">
							<span class="signal-label">Hostile multiplier</span>
							<div class="signal-value alone warn">{b.hostile_multiplier.toFixed(2)}×</div>
						</div>
					{/if}
				</div>
			{:else}
				<p class="empty-text">Behavioral analysis not available for this account.</p>
			{/if}
		</section>

		<!-- Evidence: Top Toxic Posts -->
		{#if account.top_toxic_posts.length > 0}
			<section class="section">
				<h2 class="section-title">Evidence — Top Toxic Posts</h2>
				<div class="posts-list">
					{#each account.top_toxic_posts as post}
						<div class="post-card">
							<div class="post-header">
								<span class="tox-badge" style="background: rgba(248, 113, 113, {Math.min(1, post.toxicity) * 0.3})">
									Toxicity: {(post.toxicity * 100).toFixed(0)}%
								</span>
								<a
									href={post.uri}
									target="_blank"
									rel="noopener noreferrer"
									class="post-link"
								>View post ↗</a>
							</div>
							<p class="post-text">"{post.text}"</p>
						</div>
					{/each}
				</div>
			</section>
		{/if}
	{/if}
</div>

<style>
	.page { max-width: 760px; }

	.back-link {
		display: inline-block;
		font-size: 0.875rem;
		color: #78716c;
		text-decoration: none;
		margin-bottom: 1.5rem;
		transition: color 0.2s;
	}

	.back-link:hover { color: #a8a29e; }

	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.not-found {
		padding: 3rem 0;
		text-align: center;
		color: #78716c;
	}

	.not-found h2 { font-size: 1.25rem; color: #d6d3d1; margin-bottom: 0.5rem; }

	.account-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 1.5rem;
		flex-wrap: wrap;
	}

	.handle {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.875rem;
		font-weight: 400;
		color: #fffbeb;
		letter-spacing: -0.01em;
	}

	.did { font-size: 0.8125rem; color: #57534e; margin-top: 0.25rem; font-family: monospace; }

	.bsky-link {
		padding: 0.5rem 1rem;
		font-size: 0.875rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 8px;
		text-decoration: none;
		transition: background 0.2s;
		white-space: nowrap;
		flex-shrink: 0;
	}

	.bsky-link:hover { background: rgba(201, 149, 108, 0.18); }

	.score-grid {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 1rem;
		margin-bottom: 1rem;
	}

	.score-card {
		padding: 1.25rem 1rem;
		background: rgba(28, 25, 23, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 12px;
		text-align: center;
	}

	.score-value {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.75rem;
		font-weight: 400;
		color: #fef3c7;
		line-height: 1;
		margin-bottom: 0.5rem;
	}

	.score-value.muted { color: #57534e; }

	.score-label {
		font-size: 0.75rem;
		font-weight: 500;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: #57534e;
	}

	.meta {
		font-size: 0.8125rem;
		color: #78716c;
		margin-bottom: 2rem;
	}

	.section { margin-bottom: 2rem; }

	.section-title {
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: #78716c;
		margin-bottom: 1rem;
	}

	/* Behavioral Signals */
	.signals-grid { display: flex; flex-direction: column; gap: 0.625rem; }

	.signal-row {
		display: grid;
		grid-template-columns: 160px 1fr auto;
		align-items: center;
		gap: 0.75rem;
	}

	.signal-label { font-size: 0.875rem; color: #a8a29e; }

	.signal-bar-wrap {
		height: 4px;
		background: rgba(168, 162, 158, 0.1);
		border-radius: 2px;
		overflow: hidden;
	}

	.signal-bar {
		height: 100%;
		background: linear-gradient(90deg, #c9956c, #f59e0b);
		border-radius: 2px;
		transition: width 0.5s ease;
	}

	.signal-value {
		font-size: 0.875rem;
		color: #d6d3d1;
		min-width: 3.5rem;
		text-align: right;
	}

	.signal-value.alone { grid-column: 2 / -1; justify-self: end; }
	.signal-value.warn { color: #fdba74; }

	.empty-text { font-size: 0.9375rem; color: #57534e; }

	/* Posts */
	.posts-list { display: flex; flex-direction: column; gap: 0.75rem; }

	.post-card {
		padding: 1rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.08);
		border-radius: 10px;
	}

	.post-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.625rem;
	}

	.tox-badge {
		font-size: 0.8125rem;
		font-weight: 500;
		color: #f87171;
		padding: 0.25rem 0.625rem;
		border-radius: 6px;
		border: 1px solid rgba(248, 113, 113, 0.2);
	}

	.post-link {
		font-size: 0.8125rem;
		color: #78716c;
		text-decoration: none;
	}

	.post-link:hover { color: #a8a29e; }

	.post-text {
		font-size: 0.9375rem;
		color: #d6d3d1;
		line-height: 1.6;
	}

	.muted { color: #57534e; }

	@media (max-width: 640px) {
		.score-grid { grid-template-columns: repeat(2, 1fr); }
		.signal-row { grid-template-columns: 120px 1fr auto; }
	}
</style>
