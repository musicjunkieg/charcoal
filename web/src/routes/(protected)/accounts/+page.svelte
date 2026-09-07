<script lang="ts">
	import { onMount } from 'svelte';
	import { goto, pushState } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		getAccounts,
		getActionsStatus,
		getActiveActions,
		createActionBatch,
		startConsent,
		NotConnectedError,
		AuthError,
		AccessRevokedError
	} from '$lib/api.js';
	import ConfirmSheet from '$lib/components/ConfirmSheet.svelte';
	import { bulkTierFor, showBulkBar, bulkErrorMessage, alreadyDoneMessage } from '$lib/bulk-tier-actions.js';
	import { buildSheetRows } from '$lib/sheet-rows.js';
	import type { Account, ActionKind, ActionsStatus, SheetRow } from '$lib/types.js';
	import { tierClass } from '$lib/tier-class';
	import '$lib/website/styles/tokens.css';
	import '$lib/website/styles/tiers.css';

	let asUser = $derived($page.url.searchParams.get('as_user'));
	let asUserSuffix = $derived(asUser ? `?as_user=${encodeURIComponent(asUser)}` : '');

	const TIERS = ['All', 'High', 'Elevated', 'Watch', 'Low'] as const;

	let accounts = $state<Account[]>([]);
	let total = $state(0);
	let currentPage = $state(1);
	let loading = $state(true);
	let selectedTier = $state('All');
	let searchQuery = $state('');
	let draftSearch = $state('');

	// Bulk tier actions (#315, spec §5.1).
	let actionsStatus = $state<ActionsStatus | null>(null);
	let sheet = $state<ActionKind | null>(null);
	let sheetRows = $state<SheetRow[]>([]);
	// The tier captured when the sheet opened, not the live filter — the tier
	// pills can change while `loadTierAccounts`/`getActiveActions` are in
	// flight, and the confirm/consent request must use what the person saw.
	let sheetTier = $state('');
	let bulkBusy = $state(false);
	let bulkError = $state('');
	let bulkTier = $derived(bulkTierFor(selectedTier));
	let showBulk = $derived(showBulkBar({ bulkTier, actionsStatus, asUser, total, searchQuery }));

	/** An expired cookie or a revoked grant is not a bulk-action error — it is
	 *  the same "you are signed out" that `load()` handles. Returns true when
	 *  it has taken over with a redirect. */
	function redirectedForAuth(e: unknown): boolean {
		if (e instanceof AuthError) {
			goto('/login');
			return true;
		}
		if (e instanceof AccessRevokedError) {
			goto('/waitlist');
			return true;
		}
		return false;
	}

	/** Every account in the given tier, across pages (server caps per_page at 200). */
	async function loadTierAccounts(tier: string): Promise<Account[]> {
		const got: Account[] = [];
		for (let p = 1; ; p++) {
			const res = await getAccounts({ tier, page: p, per_page: 200 });
			got.push(...res.accounts);
			if (res.accounts.length < 200 || got.length >= res.total) break;
		}
		return got;
	}

	async function openSheet(kind: ActionKind) {
		const tier = bulkTier;
		if (!tier) return;
		bulkError = '';
		bulkBusy = true;
		try {
			const [accounts, act] = await Promise.all([loadTierAccounts(tier), getActiveActions()]);
			sheetRows = buildSheetRows(accounts, act.active, kind);
			sheetTier = tier;
			sheet = kind;
		} catch (e) {
			if (redirectedForAuth(e)) return;
			bulkError = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			bulkBusy = false;
		}
	}

	async function confirmBulk(dids: string[]) {
		const kind = sheet;
		if (!kind || !sheetTier) return;
		sheet = null;
		bulkBusy = true;
		bulkError = '';
		try {
			const res = await createActionBatch(kind, `tier:${sheetTier}`, dids);
			if (res.batch_id !== null) await goto(`/actions/${res.batch_id}`);
			else bulkError = alreadyDoneMessage(kind, res.skipped_active);
		} catch (e) {
			if (e instanceof NotConnectedError) {
				await startConsent(kind, { tier: sheetTier });
				return;
			}
			if (redirectedForAuth(e)) return;
			bulkError = e instanceof Error ? e.message : 'Something went wrong';
		} finally {
			bulkBusy = false;
		}
	}

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
			} else if (err instanceof AccessRevokedError) {
				await goto('/waitlist');
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

		getActionsStatus().then((s) => (actionsStatus = s)).catch(() => (actionsStatus = null));
		const resume = u.get('resume');
		const actionsError = u.get('actions_error');
		if (actionsError) bulkError = bulkErrorMessage(actionsError);
		else if ((resume === 'mute' || resume === 'block') && t !== 'All') {
			// Back from consent: re-open the sheet the person was looking at.
			load().then(() => openSheet(resume));
			return;
		}
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
			{#each TIERS as tier (tier)}
				<button
					class="pill"
					class:active={selectedTier === tier}
					data-tier={tier}
					onclick={() => applyTier(tier)}
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

	{#if showBulk}
		<div class="bulk-bar">
			<span class="bulk-count">{total} {total === 1 ? 'account' : 'accounts'} in {bulkTier}</span>
			<div class="bulk-buttons">
				<button class="bulk-btn" onclick={() => openSheet('mute')} disabled={bulkBusy}>Mute all</button>
				<button class="bulk-btn block" onclick={() => openSheet('block')} disabled={bulkBusy}>Block all</button>
			</div>
			{#if bulkError}
				<p class="bulk-error">{bulkError}</p>
			{/if}
		</div>
	{/if}

	{#if sheet}
		<ConfirmSheet
			kind={sheet}
			rows={sheetRows}
			count={sheetRows.length}
			label={sheetTier}
			connected={actionsStatus?.connected ?? false}
			onconfirm={confirmBulk}
			oncancel={() => (sheet = null)}
		/>
	{/if}

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
					{#each accounts as account, i (account.did || i)}
						<tr
							class="account-row"
							onclick={() => goto(`/accounts/${account.handle}${asUserSuffix}`)}
							role="link"
							tabindex="0"
							onkeydown={(e) => e.key === 'Enter' && goto(`/accounts/${account.handle}${asUserSuffix}`)}
						>
							<td class="col-rank muted">{account.rank}</td>
							<td class="col-handle">
								<span class="handle-text">@{account.handle}</span>
							</td>
							<td class="col-tier">
								{#if account.threat_tier}
									<span class="tier-badge {tierClass(account.threat_tier)}">
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
		color: var(--cream-50);
	}

	.total-badge {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
		background: rgb(var(--charcoal-400-rgb) / 0.08);
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

	.bulk-bar { display: flex; align-items: center; gap: 0.75rem; flex-wrap: wrap; padding: 0.625rem 0.875rem; margin-bottom: 1rem; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: 10px; font-size: 0.875rem; }
	.bulk-count { color: var(--charcoal-400); }
	.bulk-buttons { display: flex; gap: 0.375rem; margin-left: auto; }
	.bulk-btn { padding: 0.375rem 0.75rem; font: inherit; font-size: 0.8125rem; border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15); border-radius: 8px; background: transparent; color: var(--charcoal-400); cursor: pointer; }
	.bulk-btn.block { color: var(--tier-high); }
	.bulk-btn:disabled { opacity: 0.5; cursor: not-allowed; }
	.bulk-error { width: 100%; margin: 0; font-size: 0.75rem; color: var(--status-error); }

	.tier-pills { display: flex; gap: 0.375rem; flex-wrap: wrap; }

	.pill {
		padding: 0.375rem 0.875rem;
		font-size: 0.875rem;
		font-weight: 400;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--charcoal-500);
		background: rgb(var(--charcoal-900-rgb) / 0.6);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.12);
		border-radius: 999px;
		cursor: pointer;
		transition: all 0.2s;
	}

	.pill:hover { color: var(--charcoal-300); border-color: rgb(var(--charcoal-400-rgb) / 0.25); }
	.pill.active { color: var(--cream-100); background: rgb(var(--copper-rgb) / 0.12); border-color: rgb(var(--copper-rgb) / 0.3); }

	.pill.active[data-tier='High'] {
		color: var(--tier-high);
		border-color: rgb(var(--tier-high-rgb) / 0.25);
	}
	.pill.active[data-tier='Elevated'] {
		color: var(--tier-elevated);
		border-color: rgb(var(--tier-elevated-rgb) / 0.25);
	}
	.pill.active[data-tier='Watch'] {
		color: var(--tier-watch);
		border-color: rgb(var(--tier-watch-rgb) / 0.25);
	}
	.pill.active[data-tier='Low'] {
		color: var(--tier-low);
		border-color: rgb(var(--tier-low-rgb) / 0.25);
	}

	.search-row { flex: 1; min-width: 200px; }

	.search-box {
		display: flex;
		align-items: center;
		background: rgb(var(--charcoal-950-rgb) / 0.6);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.12);
		border-radius: 10px;
		padding: 0 0.875rem;
	}

	.search-box:focus-within {
		border-color: var(--copper);
		box-shadow: 0 0 0 2px rgb(var(--copper-rgb) / 0.1);
	}

	.search-at { color: var(--charcoal-700); font-size: 0.9375rem; margin-right: 0.25rem; }

	.search-input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 0.625rem 0;
		font-size: 0.875rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--cream-100);
		outline: none;
	}

	.search-input::placeholder { color: var(--charcoal-700); }

	.search-btn {
		padding: 0.375rem 0.75rem;
		font-size: 0.8125rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--copper);
		background: transparent;
		border: none;
		cursor: pointer;
	}

	.search-btn:hover { color: var(--copper-light); }

	.loading-state { display: flex; justify-content: center; padding: 4rem 0; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.empty-state { padding: 3rem 0; text-align: center; color: var(--charcoal-600); font-size: 0.9375rem; }

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
		color: var(--charcoal-600);
		border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.08);
	}

	.table td {
		padding: 0.75rem 0.75rem;
		border-bottom: 1px solid rgb(var(--charcoal-400-rgb) / 0.05);
		color: var(--charcoal-300);
	}

	.account-row {
		cursor: pointer;
		transition: background 0.15s;
	}

	.account-row:hover td { background: rgb(var(--copper-rgb) / 0.04); }

	.handle-text { color: var(--copper); font-weight: 500; }

	.tier-badge { font-weight: 500; font-size: 0.875rem; }

	.muted { color: var(--charcoal-500); }

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
		color: var(--copper);
		background: rgb(var(--copper-rgb) / 0.1);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
		border-radius: 8px;
		cursor: pointer;
		transition: background 0.2s;
	}

	.page-btn:hover:not(:disabled) { background: rgb(var(--copper-rgb) / 0.18); }
	.page-btn:disabled { opacity: 0.4; cursor: not-allowed; }

	.page-info { font-size: 0.875rem; color: var(--charcoal-500); }
</style>
