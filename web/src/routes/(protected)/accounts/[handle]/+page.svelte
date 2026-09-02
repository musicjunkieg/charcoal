<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { getAccount } from '$lib/api.js';
	import { AuthError, AccessRevokedError } from '$lib/api.js';
	import type { Account } from '$lib/types.js';
	import LabelButtons from '$lib/components/LabelButtons.svelte';
	import ActionButtons from '$lib/components/ActionButtons.svelte';
	import { tierClass } from '$lib/tier-class';
	import '$lib/website/styles/tokens.css';
	import '$lib/website/styles/tiers.css';

	let asUser = $derived($page.url.searchParams.get('as_user'));
	let asUserSuffix = $derived(asUser ? `?as_user=${encodeURIComponent(asUser)}` : '');
	let resume = $derived($page.url.searchParams.get('resume'));
	let actionsError = $derived($page.url.searchParams.get('actions_error'));

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
			if (err instanceof AccessRevokedError) {
				await goto('/waitlist');
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
	<a href="/accounts{asUserSuffix}" class="back-link">← All accounts</a>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if notFound}
		<div class="not-found">
			<h2>Account not found</h2>
			<p>@{$page.params.handle} hasn't been scored yet.</p>
		</div>
	{:else if account}
		{#if account.scored_at === null}
			<div class="not-scored-banner">
				This account was detected as an amplifier but hasn't been fully scored yet.
				Scores will appear after the next scan.
			</div>
		{/if}
		<div class="account-header">
			<div>
				<h1 class="handle">@{account.handle}</h1>
				<p class="did">{account.did ?? 'DID not yet resolved'}</p>
			</div>
			<a
				href="https://bsky.app/profile/{account.handle}"
				target="_blank"
				rel="noopener noreferrer"
				class="bsky-link"
			>View on Bluesky ↗</a>
		</div>

		<!-- Label + actions -->
		{#if account.did}
			<div class="label-section">
				<LabelButtons
					targetDid={account.did}
					currentLabel={(account as any).user_label?.label ?? null}
					predictedTier={account.threat_tier}
				/>
				<ActionButtons
					handle={account.handle}
					did={account.did}
					tier={account.threat_tier}
					{resume}
					{actionsError}
					impersonating={asUser !== null}
				/>
			</div>
		{/if}

		<!-- Score Overview -->
		<div class="score-grid">
			<div class="score-card">
				<div class="score-value">{formatScore(account.threat_score)}</div>
				<div class="score-label">Threat Score</div>
			</div>
			<div class="score-card">
				{#if account.threat_tier}
					<div class="score-value {tierClass(account.threat_tier)}">
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
			{#if account.context_score != null}
				<div class="score-card">
					<div class="score-value">{formatScore(account.context_score)}</div>
					<div class="score-label">Context Score</div>
				</div>
			{/if}
		</div>

		<p class="meta">
			{account.posts_analyzed} posts analyzed
			{#if account.scored_at}
				&nbsp;·&nbsp; Scored {account.scored_at.slice(0, 10)}
			{/if}
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
		{#if account.top_toxic_posts && account.top_toxic_posts.length > 0}
			<section class="section">
				<h2 class="section-title">Evidence — Top Toxic Posts</h2>
				<div class="posts-list">
					{#each account.top_toxic_posts as post (post.uri)}
						<div class="post-card">
							<div class="post-header">
								<span class="tox-badge" style="background: rgb(var(--status-error-rgb) / {Math.min(1, post.toxicity) * 0.3})">
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
		color: var(--charcoal-500);
		text-decoration: none;
		margin-bottom: 1.5rem;
		transition: color 0.2s;
	}

	.back-link:hover { color: var(--charcoal-400); }

	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.not-found {
		padding: 3rem 0;
		text-align: center;
		color: var(--charcoal-500);
	}

	.not-found h2 { font-size: 1.25rem; color: var(--charcoal-300); margin-bottom: 0.5rem; }

	.not-scored-banner {
		padding: 1rem 1.25rem;
		margin-bottom: 1.5rem;
		background: rgb(var(--amber-500-rgb) / 0.08);
		border: 1px solid rgb(var(--amber-500-rgb) / 0.2);
		border-radius: 10px;
		color: var(--tier-watch);
		font-size: 0.875rem;
		line-height: 1.5;
	}

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
		color: var(--cream-50);
		letter-spacing: -0.01em;
	}

	.did { font-size: 0.8125rem; color: var(--charcoal-600); margin-top: 0.25rem; font-family: monospace; }

	.bsky-link {
		padding: 0.5rem 1rem;
		font-size: 0.875rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--copper);
		background: rgb(var(--copper-rgb) / 0.1);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
		border-radius: 8px;
		text-decoration: none;
		transition: background 0.2s;
		white-space: nowrap;
		flex-shrink: 0;
	}

	.bsky-link:hover { background: rgb(var(--copper-rgb) / 0.18); }

	.label-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		margin-bottom: 1.5rem;
		padding: 1rem 1.25rem;
		background: rgb(var(--charcoal-900-rgb) / 0.4);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.08);
		border-radius: 12px;
	}

	.score-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
		gap: 1rem;
		margin-bottom: 1rem;
	}

	.score-card {
		padding: 1.25rem 1rem;
		background: rgb(var(--charcoal-900-rgb) / 0.6);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.1);
		border-radius: 12px;
		text-align: center;
	}

	.score-value {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.75rem;
		font-weight: 400;
		color: var(--cream-100);
		line-height: 1;
		margin-bottom: 0.5rem;
	}

	.score-value.muted { color: var(--charcoal-600); }

	/* Overrides .score-value's own colour above: without these, the plain
	   global .tier-* rules (specificity 0-1-0) lose to this file's own
	   .score-value rule (0-2-0 once Svelte's scoping class is attached),
	   the same way .score-value.muted and .signal-value.warn below already
	   override the base rule for their class. The inline style this
	   replaced always won regardless of specificity; a bare tier-* class
	   would not have. */
	.score-value.tier-high { color: var(--tier-high); }
	.score-value.tier-elevated { color: var(--tier-elevated); }
	.score-value.tier-watch { color: var(--tier-watch); }
	.score-value.tier-low { color: var(--tier-low); }

	.score-label {
		font-size: 0.75rem;
		font-weight: 500;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--charcoal-600);
	}

	.meta {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
		margin-bottom: 2rem;
	}

	.section { margin-bottom: 2rem; }

	.section-title {
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--charcoal-500);
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

	.signal-label { font-size: 0.875rem; color: var(--charcoal-400); }

	.signal-bar-wrap {
		height: 4px;
		background: rgb(var(--charcoal-400-rgb) / 0.1);
		border-radius: 2px;
		overflow: hidden;
	}

	.signal-bar {
		height: 100%;
		background: linear-gradient(90deg, var(--copper), var(--amber-500));
		border-radius: 2px;
		transition: width 0.5s ease;
	}

	.signal-value {
		font-size: 0.875rem;
		color: var(--charcoal-300);
		min-width: 3.5rem;
		text-align: right;
	}

	.signal-value.alone { grid-column: 2 / -1; justify-self: end; }
	.signal-value.warn { color: var(--tier-elevated); }

	.empty-text { font-size: 0.9375rem; color: var(--charcoal-600); }

	/* Posts */
	.posts-list { display: flex; flex-direction: column; gap: 0.75rem; }

	.post-card {
		padding: 1rem;
		background: rgb(var(--charcoal-900-rgb) / 0.5);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.08);
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
		color: var(--status-error);
		padding: 0.25rem 0.625rem;
		border-radius: 6px;
		border: 1px solid rgb(var(--status-error-rgb) / 0.2);
	}

	.post-link {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
		text-decoration: none;
	}

	.post-link:hover { color: var(--charcoal-400); }

	.post-text {
		font-size: 0.9375rem;
		color: var(--charcoal-300);
		line-height: 1.6;
	}

	.muted { color: var(--charcoal-600); }

	@media (max-width: 640px) {
		.score-grid { grid-template-columns: repeat(2, 1fr); }
		.signal-row { grid-template-columns: 120px 1fr auto; }
	}
</style>
