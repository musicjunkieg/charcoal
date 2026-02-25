<script lang="ts">
	import { onMount } from 'svelte';
	import { goto, pushState } from '$app/navigation';
	import { page } from '$app/stores';
	import { getAccounts } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import type { Account } from '$lib/types.js';

	const TIERS = ['All', 'High', 'Elevated', 'Watch', 'Low'] as const;
	const TIER_COLORS: Record<string, string> = {
		High: '#fca5a5',
		Elevated: '#fdba74',
		Watch: '#fcd34d',
		Low: '#a8a29e'
	};

	let accounts = $state<Account[]>([]);
	let total = $state(0);
	let currentPage = $state(1);
	let loading = $state(true);
	let selectedTier = $state('All');
	let searchQuery = $state('');
	let draftSearch = $state('');

	async function load() {
		loading = true;
		try {
			const params: Record<string, string | number> = { page: currentPage, per_page: 50 };
			if (selectedTier !== 'All') params.tier = selectedTier;
			if (searchQuery) params.q = searchQuery;

			const res = await getAccounts(params);
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

	function applyTier(tier: string) {
		selectedTier = tier;
		currentPage = 1;
		load();
	}

	function applySearch(e: KeyboardEvent | MouseEvent) {
		if (e instanceof KeyboardEvent && e.key !== 'Enter') return;
		searchQuery = draftSearch.trim();
		currentPage = 1;
		load();
	}

	function formatScore(s: number | null): string {
		return s != null ? s.toFixed(1) : '—';
	}

	function formatPct(s: number | null): string {
		return s != null ? `${(s * 100).toFixed(0)}%` : '—';
	}

	onMount(() => {
		// Pick up ?tier= and ?q= from URL params on initial load
		const u = $page.url.searchParams;
		const t = u.get('tier') ?? 'All';
		if (TIERS.includes(t as (typeof TIERS)[number])) selectedTier = t;
		const q = u.get('q') ?? '';
		draftSearch = q;
		searchQuery = q;
		load();
	});
</script>

<svelte:head>
	<title>Accounts — Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<h1 class="page-title">Accounts</h1>
		{#if !loading}
			<span class="total-badge">{total} accounts</span>
		{/if}
	</div>

	<!-- Filters -->
	<div class="filters">
		<div class="tier-pills">
			{#each TIERS as tier}
				<button
					class="pill"
					class:active={selectedTier === tier}
					onclick={() => applyTier(tier)}
					style={tier !== 'All' && selectedTier === tier ? `color: ${TIER_COLORS[tier]}; border-color: ${TIER_COLORS[tier]}40` : ''}
				>{tier}</button>
			{/each}
		</div>

		<div class="search-row">
			<div class="search-box">
				<span class="search-at">@</span>
				<input
					type="text"
					class="search-input"
					placeholder="Search handle..."
					bind:value={draftSearch}
					onkeydown={applySearch}
				/>
				<button class="search-btn" onclick={applySearch}>Search</button>
			</div>
		</div>
	</div>

	{#if loading}
		<div class="loading-state"><div class="spinner"></div></div>
	{:else if accounts.length === 0}
		<div class="empty-state">
			<p>No accounts found{selectedTier !== 'All' ? ` in ${selectedTier} tier` : ''}{searchQuery ? ` matching "${searchQuery}"` : ''}.</p>
		</div>
	{:else}
		<div class="table-wrap">
			<table class="table">
				<thead>
					<tr>
						<th class="col-rank">#</th>
						<th class="col-handle">Handle</th>
						<th class="col-tier">Tier</th>
						<th class="col-score">Score</th>
						<th class="col-tox">Toxicity</th>
						<th class="col-overlap">Overlap</th>
						<th class="col-date">Scored</th>
					</tr>
				</thead>
				<tbody>
					{#each accounts as account}
						<tr
							class="account-row"
							onclick={() => goto(`/accounts/${account.handle}`)}
							role="link"
							tabindex="0"
							onkeydown={(e) => e.key === 'Enter' && goto(`/accounts/${account.handle}`)}
						>
							<td class="col-rank muted">{account.rank}</td>
							<td class="col-handle">
								<span class="handle-text">@{account.handle}</span>
							</td>
							<td class="col-tier">
								{#if account.threat_tier}
									<span class="tier-badge" style="color: {TIER_COLORS[account.threat_tier] ?? '#a8a29e'}">
										{account.threat_tier}
									</span>
								{:else}
									<span class="muted">—</span>
								{/if}
							</td>
							<td class="col-score">{formatScore(account.threat_score)}</td>
							<td class="col-tox muted">{formatScore(account.toxicity_score)}</td>
							<td class="col-overlap muted">{formatPct(account.topic_overlap)}</td>
							<td class="col-date muted">{account.scored_at.slice(0, 10)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>

		{#if total > 50}
			<div class="pagination">
				<button
					class="page-btn"
					disabled={currentPage <= 1}
					onclick={() => { currentPage--; load(); }}
				>← Prev</button>
				<span class="page-info">Page {currentPage} of {Math.ceil(total / 50)}</span>
				<button
					class="page-btn"
					disabled={currentPage >= Math.ceil(total / 50)}
					onclick={() => { currentPage++; load(); }}
				>Next →</button>
			</div>
		{/if}
	{/if}
</div>

<style>
	.page { max-width: 900px; }

	.page-header {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-bottom: 1.5rem;
	}

	.page-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.75rem;
		font-weight: 400;
		color: #fffbeb;
	}

	.total-badge {
		font-size: 0.8125rem;
		color: #78716c;
		background: rgba(168, 162, 158, 0.08);
		padding: 0.25rem 0.625rem;
		border-radius: 999px;
	}

	.filters {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-bottom: 1.5rem;
		flex-wrap: wrap;
	}

	.tier-pills { display: flex; gap: 0.375rem; flex-wrap: wrap; }

	.pill {
		padding: 0.375rem 0.875rem;
		font-size: 0.875rem;
		font-weight: 400;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #78716c;
		background: rgba(28, 25, 23, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.12);
		border-radius: 999px;
		cursor: pointer;
		transition: all 0.2s;
	}

	.pill:hover { color: #d6d3d1; border-color: rgba(168, 162, 158, 0.25); }
	.pill.active { color: #fef3c7; background: rgba(201, 149, 108, 0.12); border-color: rgba(201, 149, 108, 0.3); }

	.search-row { flex: 1; min-width: 200px; }

	.search-box {
		display: flex;
		align-items: center;
		background: rgba(12, 10, 9, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.12);
		border-radius: 10px;
		padding: 0 0.875rem;
	}

	.search-box:focus-within {
		border-color: #c9956c;
		box-shadow: 0 0 0 2px rgba(201, 149, 108, 0.1);
	}

	.search-at { color: #44403c; font-size: 0.9375rem; margin-right: 0.25rem; }

	.search-input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 0.625rem 0;
		font-size: 0.875rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #fef3c7;
		outline: none;
	}

	.search-input::placeholder { color: #44403c; }

	.search-btn {
		padding: 0.375rem 0.75rem;
		font-size: 0.8125rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #c9956c;
		background: transparent;
		border: none;
		cursor: pointer;
	}

	.search-btn:hover { color: #e8b48a; }

	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.empty-state { padding: 3rem 0; text-align: center; color: #57534e; font-size: 0.9375rem; }

	.table-wrap { overflow-x: auto; }

	.table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.9375rem;
	}

	.table th {
		text-align: left;
		padding: 0.5rem 0.75rem;
		font-size: 0.75rem;
		font-weight: 500;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: #57534e;
		border-bottom: 1px solid rgba(168, 162, 158, 0.08);
	}

	.table td {
		padding: 0.75rem 0.75rem;
		border-bottom: 1px solid rgba(168, 162, 158, 0.05);
		color: #d6d3d1;
	}

	.account-row {
		cursor: pointer;
		transition: background 0.15s;
	}

	.account-row:hover td { background: rgba(201, 149, 108, 0.04); }

	.handle-text { color: #c9956c; font-weight: 500; }

	.tier-badge { font-weight: 500; font-size: 0.875rem; }

	.muted { color: #78716c; }

	.col-rank { width: 3rem; }
	.col-tier { width: 5rem; }
	.col-score { width: 5rem; }
	.col-tox { width: 5rem; }
	.col-overlap { width: 5rem; }
	.col-date { width: 7rem; }

	.pagination {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 1.5rem;
		padding: 1.5rem 0;
	}

	.page-btn {
		padding: 0.5rem 1rem;
		font-size: 0.875rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 8px;
		cursor: pointer;
		transition: background 0.2s;
	}

	.page-btn:hover:not(:disabled) { background: rgba(201, 149, 108, 0.18); }
	.page-btn:disabled { opacity: 0.4; cursor: not-allowed; }

	.page-info { font-size: 0.875rem; color: #78716c; }
</style>
