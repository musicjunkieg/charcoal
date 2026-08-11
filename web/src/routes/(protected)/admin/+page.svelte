<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { getAdminUsers, preSeedUser, triggerAdminScan, deleteAdminUser } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import '$lib/website/styles/tokens.css';
	import type { AdminUser, AdminQueue, AdminScanRow } from '$lib/types.js';

	let users = $state<AdminUser[]>([]);
	let queue = $state<AdminQueue | null>(null);
	let loading = $state(true);
	let handle = $state('');
	let addLoading = $state(false);
	let addError = $state('');
	let addSuccess = $state('');
	let scanningDid = $state<string | null>(null);
	let deletingDid = $state<string | null>(null);
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	let anyBuilding = $derived(users.some((u) => u.fingerprint_building));
	let anyScanning = $derived(scanningDid !== null);
	let queueActive = $derived((queue?.active.length ?? 0) > 0);

	// Queued rows exist while running is UNDER the cap — nothing is claiming
	// them. This is #286's wedged state, which until now only ever appeared as
	// an ERROR line in the server log. "Scans stopped happening" is exactly
	// what an operator needs to see and will never find by grepping.
	//
	// It needs a DWELL, though, and running this locally is how I found that
	// out: the bare condition is routinely true for up to one admitter TICK.
	// A row inserted without going through `enqueue_scan` never fires the
	// `try_send` wake, so it simply waits for the next tick — and a slot that
	// frees is briefly idle before the next claim commits. Alarming instantly
	// would mean alarming several times an hour during entirely healthy
	// operation, which is how an alert gets ignored. TICK is 30s server-side;
	// 45s leaves margin for a slow claim without hiding a real stall for long.
	const WEDGE_GRACE_MS = 45_000;
	let wedgedSince = $state<number | null>(null);
	let wedged = $state(false);

	function assessWedged() {
		const stalled =
			queue !== null && queue.queued > 0 && queue.running < queue.concurrency_limit;
		wedgedSince = stalled ? (wedgedSince ?? Date.now()) : null;
		wedged = wedgedSince !== null && Date.now() - wedgedSince > WEDGE_GRACE_MS;
	}

	async function loadUsers() {
		try {
			const res = await getAdminUsers();
			users = res.users;
			queue = res.queue;
			assessWedged();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
			}
		} finally {
			loading = false;
		}
	}

	/** Elapsed since an ISO timestamp, as "4h 07m" / "12m 30s" / "45s". */
	function elapsedSince(iso: string | null): string {
		if (!iso) return '';
		const ms = Date.now() - new Date(iso).getTime();
		if (!Number.isFinite(ms) || ms < 0) return '';
		const s = Math.floor(ms / 1000);
		if (s < 60) return `${s}s`;
		const m = Math.floor(s / 60);
		if (m < 60) return `${m}m ${String(s % 60).padStart(2, '0')}s`;
		return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
	}

	/** What a row is doing, in words rather than a status code. A queued row
	 *  says WAITING — never anything that implies work is underway, which is
	 *  the product's stated rule for the user-facing view and applies just as
	 *  much to the operator's. */
	function scanSummary(scan: AdminScanRow): string {
		switch (scan.status) {
			case 'running':
				return `Scanning — ${elapsedSince(scan.started_at)} elapsed`;
			case 'queued':
				return scan.position > 0 ? `Waiting — position ${scan.position}` : 'Waiting to start';
			case 'failed':
				return scan.last_error ? `Failed — ${scan.last_error}` : 'Failed';
			case 'done':
				return 'Finished';
		}
	}

	/** Longest failure reason rendered inline in the Last Scan cell.
	 *
	 *  Whatever the scan pipeline threw ends up here — a RunPod payload or a
	 *  Postgres message runs to hundreds of characters and would push the
	 *  table's remaining columns off screen. Only the VISIBLE copy is
	 *  shortened: the full reason is announced from a visually-hidden span
	 *  alongside it. It is deliberately not a `title` tooltip, which is
	 *  unreachable by keyboard and simply absent on touch. */
	const MAX_INLINE_REASON = 72;

	function truncateReason(reason: string): string {
		// Errors arrive with newlines and doubled spaces from `{:#}` chains;
		// collapsing them keeps the cell to one readable phrase.
		const clean = reason.trim().replace(/\s+/g, ' ');
		if (clean.length <= MAX_INLINE_REASON) return clean;
		return `${clean.slice(0, MAX_INLINE_REASON - 1).trimEnd()}…`;
	}

	function displayHandle(scan: AdminScanRow): string {
		// An orphaned queue row (user deleted mid-scan) has no handle. Show the
		// DID tail rather than "unknown" — it is still identifiable, and the
		// row is listed precisely because it should not be hidden.
		return scan.handle ? `@${scan.handle}` : scan.user_did.slice(-12);
	}

	async function handleAdd() {
		const trimmed = handle.trim().replace(/^@/, '');
		if (!trimmed) return;
		addError = '';
		addSuccess = '';
		addLoading = true;
		try {
			const res = await preSeedUser(trimmed);
			addSuccess = `Added @${res.handle}`;
			handle = '';
			await loadUsers();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			addError = err instanceof Error ? err.message : 'Failed to add user';
		} finally {
			addLoading = false;
		}
	}

	async function handleScan(did: string) {
		scanningDid = did;
		try {
			await triggerAdminScan(did);
			await loadUsers();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
		} finally {
			scanningDid = null;
		}
	}

	async function handleDelete(user: AdminUser) {
		if (!confirm(`Remove @${user.handle}? This will delete all their data.`)) return;
		deletingDid = user.did;
		try {
			await deleteAdminUser(user.did);
			await loadUsers();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
		} finally {
			deletingDid = null;
		}
	}

	function formatDate(iso: string | null): string {
		if (!iso) return '--';
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

	function startPolling() {
		if (pollTimer) clearInterval(pollTimer);
		pollTimer = setInterval(() => {
			// Also poll while the queue has active work. Without this the panel
			// would render once and then sit frozen for the two hours a scan
			// takes — the poll used to fire only for fingerprint builds, which
			// finish in seconds.
			if (anyBuilding || queueActive) {
				loadUsers();
			}
		}, 3000);
	}

	onMount(() => {
		loadUsers();
		startPolling();
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<svelte:head>
	<title>Admin -- Charcoal</title>
</svelte:head>

<div class="page">
	<div class="page-header">
		<h1 class="page-title">Admin</h1>
	</div>

	<!-- Scan queue — first, because "what is the system doing right now" is the
	     reason to open this page. Adding a user is an occasional action. -->
	<section class="queue-section">
		<h2 class="section-title">Scan Queue</h2>

		{#if queue}
			<p class="queue-capacity">
				<strong>{queue.running}</strong> of
				<strong>{queue.concurrency_limit}</strong>
				{queue.concurrency_limit === 1 ? 'slot' : 'slots'} in use
				{#if queue.queued > 0}
					· <strong>{queue.queued}</strong> waiting
				{/if}
			</p>

			{#if wedged}
				<!-- Not decorative. Queued work with a free slot means nothing is
				     claiming it, which is how a broken queue looks from outside. -->
				<p class="queue-wedged" role="alert">
					{queue.queued}
					{queue.queued === 1 ? 'scan is' : 'scans are'} waiting while
					{queue.concurrency_limit - queue.running}
					{queue.concurrency_limit - queue.running === 1 ? 'slot is' : 'slots are'} free.
					Nothing is claiming them — the admitter may have stopped. Check the server log
					for a “scan admission is WEDGED” error.
				</p>
			{/if}

			{#if queue.active.length === 0}
				<p class="queue-idle">No scans running or queued.</p>
			{:else}
				<ul class="queue-list">
					{#each queue.active as scan (scan.user_did)}
						<li class="queue-item">
							<span class="queue-status queue-status-{scan.status}">
								{scan.status === 'running' ? 'Running' : 'Waiting'}
							</span>
							<span class="queue-handle">{displayHandle(scan)}</span>
							<span class="queue-detail">{scanSummary(scan)}</span>
						</li>
					{/each}
				</ul>
			{/if}
		{:else if loading}
			<div class="loading-state"><div class="spinner"></div></div>
		{/if}
	</section>

	<!-- Pre-seed form -->
	<section class="add-section">
		<h2 class="section-title">Add Protected User</h2>
		<div class="add-form">
			<div class="add-input-wrap">
				<span class="input-at">@</span>
				<input
					type="text"
					class="add-input"
					placeholder="handle.bsky.social"
					bind:value={handle}
					onkeydown={(e) => e.key === 'Enter' && handleAdd()}
					disabled={addLoading}
				/>
			</div>
			<button class="btn-add" onclick={handleAdd} disabled={addLoading || !handle.trim()}>
				{addLoading ? 'Adding...' : 'Add User'}
			</button>
		</div>
		{#if addError}
			<p class="msg-error">{addError}</p>
		{/if}
		{#if addSuccess}
			<p class="msg-success">{addSuccess}</p>
		{/if}
	</section>

	<!-- Users table -->
	<section class="users-section">
		<h2 class="section-title">Protected Users</h2>

		{#if loading}
			<div class="loading-state"><div class="spinner"></div></div>
		{:else if users.length === 0}
			<div class="empty-state">
				<p>No protected users yet. Add one above to get started.</p>
			</div>
		{:else}
			<div class="table-wrap">
				<table class="table">
					<thead>
						<tr>
							<th class="col-handle">Handle</th>
							<th class="col-fp">Fingerprint</th>
							<th class="col-scan">Last Scan</th>
							<th class="col-count">Scored</th>
							<th class="col-actions">Actions</th>
						</tr>
					</thead>
					<tbody>
						{#each users as user (user.did)}
							<tr class="user-row">
								<td class="col-handle">
									<span class="handle-text">@{user.handle}</span>
								</td>
								<td class="col-fp">
									{#if user.fingerprint_building}
										<span class="status-building">
											<span class="mini-spinner"></span>
											Building...
										</span>
									{:else if user.has_fingerprint}
										<span class="status-ready">Ready</span>
									{:else}
										<span class="status-none">--</span>
									{/if}
								</td>
								<td class="col-scan">
									{#if user.scan && (user.scan.status === 'running' || user.scan.status === 'queued')}
										<span class="scan-live scan-live-{user.scan.status}"
											>{scanSummary(user.scan)}</span
										>
									{:else if user.scan?.status === 'failed'}
										<!-- The visible copy is shortened and hidden from assistive
										     tech; the .sr-only sibling carries the whole reason. A
										     `title` tooltip on a non-focusable span reached neither
										     keyboard nor touch, so the reason was effectively secret. -->
										<span class="scan-failed" aria-hidden="true">Failed</span>
										{#if user.scan.last_error}
											<span class="scan-reason" aria-hidden="true"
												>{truncateReason(user.scan.last_error)}</span
											>
										{/if}
										<span class="sr-only">{scanSummary(user.scan)}</span>
										<span class="muted scan-when">{formatDate(user.scan.finished_at)}</span>
									{:else if user.last_scan_at}
										<span class="muted">{formatDate(user.last_scan_at)}</span>
									{:else}
										<!-- No queue row at all: never enqueued, which the old
										     dashboard could not distinguish from "not running". -->
										<span class="status-none">Never scanned</span>
									{/if}
								</td>
								<td class="col-count muted">{user.scored_accounts}</td>
								<td class="col-actions">
									<div class="action-btns">
										<button
											class="btn-action btn-scan"
											onclick={() => handleScan(user.did)}
											disabled={!user.has_fingerprint || user.fingerprint_building || anyScanning}
										>
											{scanningDid === user.did ? 'Starting...' : 'Scan'}
										</button>
										<a
											href="/dashboard?as_user={encodeURIComponent(user.did)}"
											class="btn-action btn-view"
										>View</a>
										<button
											class="btn-action btn-delete"
											onclick={() => handleDelete(user)}
											disabled={deletingDid === user.did}
										>
											{deletingDid === user.did ? '...' : 'Delete'}
										</button>
									</div>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</section>
</div>

<style>
	.page { max-width: 900px; }

	.page-header {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-bottom: 2rem;
	}

	.page-title {
		font-family: 'Libre Baskerville', Georgia, serif;
		font-size: 1.75rem;
		font-weight: 400;
		color: var(--cream-50);
	}

	.section-title {
		font-size: 1rem;
		font-weight: 500;
		color: var(--charcoal-300);
		letter-spacing: 0.01em;
		margin-bottom: 0.875rem;
	}

	/* ── Scan queue (#288) ───────────────────────────────────────────────
	   Sizes and radii come from the DESIGN.md ramp (1rem / 0.8125rem;
	   8px / 12px) and colours from tokens.css, so this section adds no new
	   literal values — the surrounding file predates that discipline (#250).
	   --charcoal-400 is the floor for body text here: --charcoal-500 on this
	   ground fails AA (#249). */
	.queue-capacity {
		font-size: 1rem;
		color: var(--charcoal-300);
		margin: 0 0 0.75rem;
	}

	.queue-capacity strong {
		color: var(--copper);
		font-weight: 500;
	}

	.queue-idle {
		font-size: 0.8125rem;
		color: var(--charcoal-400);
		margin: 0;
	}

	.queue-wedged {
		font-size: 0.8125rem;
		color: var(--status-error);
		background: rgb(var(--status-error-rgb) / 0.08);
		border: 1px solid rgb(var(--status-error-rgb) / 0.35);
		border-radius: 8px;
		padding: 0.625rem 0.75rem;
		margin: 0 0 0.75rem;
		line-height: 1.5;
	}

	.queue-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.queue-item {
		display: flex;
		align-items: baseline;
		gap: 0.75rem;
		flex-wrap: wrap;
		padding: 0.5rem 0.75rem;
		background: var(--charcoal-900);
		border-radius: 8px;
	}

	/* Status as a word, not a colour alone — colour is never the only carrier. */
	.queue-status {
		font-size: 0.8125rem;
		font-weight: 500;
		min-width: 4.5rem;
	}

	.queue-status-running {
		color: var(--amber-500);
	}

	.queue-status-queued {
		color: var(--charcoal-400);
	}

	.queue-handle {
		font-size: 0.8125rem;
		color: var(--charcoal-300);
	}

	.queue-detail {
		font-size: 0.8125rem;
		color: var(--charcoal-400);
	}

	/* Table cell — live scan state */
	.scan-live {
		font-size: 0.8125rem;
	}

	.scan-live-running {
		color: var(--amber-500);
	}

	.scan-live-queued {
		color: var(--charcoal-400);
	}

	.scan-failed {
		font-size: 0.8125rem;
		color: var(--status-error);
	}

	/* The failure reason itself, in body-text grey rather than the error red:
	   "Failed" is the alarm, the reason is prose. --charcoal-400 is the floor
	   for body text here; --charcoal-500 fails WCAG AA on this ground. */
	.scan-reason {
		display: block;
		font-size: 0.8125rem;
		color: var(--charcoal-400);
		/* A stack trace or URL has no spaces to break on, and this column is
		   9rem wide — without this one row widens the whole table. */
		overflow-wrap: anywhere;
	}

	.scan-when {
		display: block;
		font-size: 0.8125rem;
		margin-top: 0.25rem;
	}

	/* Visually hidden, still announced. The FULL failure reason lives here, so
	   the inline copy can stay short without the reason becoming unreachable.
	   The pattern this replaces was a `title` attribute on a non-focusable
	   span: no keyboard access, nothing at all on touch. */
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
		border: 0;
	}

	/* Add user form */
	.add-section {
		margin-bottom: 2.5rem;
		padding: 1.25rem;
		background: rgb(var(--charcoal-900-rgb) / 0.5);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.1);
		border-radius: 14px;
	}

	.add-form {
		display: flex;
		gap: 0.75rem;
		align-items: center;
	}

	.add-input-wrap {
		flex: 1;
		display: flex;
		align-items: center;
		background: rgb(var(--charcoal-950-rgb) / 0.6);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.15);
		border-radius: 10px;
		padding: 0 0.875rem;
		transition: border-color 0.2s;
	}

	.add-input-wrap:focus-within {
		border-color: var(--copper);
		box-shadow: 0 0 0 2px rgb(var(--copper-rgb) / 0.1);
	}

	.input-at { color: var(--charcoal-700); font-size: 0.9375rem; margin-right: 0.25rem; }

	.add-input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 0.625rem 0;
		font-size: 0.9375rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--cream-100);
		outline: none;
	}

	.add-input::placeholder { color: var(--charcoal-700); }
	.add-input:disabled { opacity: 0.5; }

	.btn-add {
		padding: 0.625rem 1.25rem;
		font-size: 0.9375rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		color: var(--charcoal-950);
		background: linear-gradient(135deg, var(--amber-500) 0%, var(--copper) 100%);
		border: none;
		border-radius: 10px;
		cursor: pointer;
		transition: transform 0.2s, box-shadow 0.2s;
		box-shadow: 0 4px 12px -2px rgb(var(--amber-500-rgb) / 0.35);
		white-space: nowrap;
	}

	.btn-add:hover:not(:disabled) { transform: translateY(-1px); box-shadow: 0 6px 16px -2px rgb(var(--amber-500-rgb) / 0.45); }
	.btn-add:disabled { opacity: 0.6; cursor: not-allowed; transform: none; box-shadow: none; }

	.msg-error { font-size: 0.8125rem; color: var(--status-error); margin-top: 0.625rem; }
	.msg-success { font-size: 0.8125rem; color: var(--status-ok); margin-top: 0.625rem; }

	/* Users table */
	.users-section { margin-top: 1rem; }

	.loading-state { display: flex; justify-content: center; padding: 3rem 0; }
	.empty-state { padding: 3rem 0; text-align: center; color: var(--charcoal-600); font-size: 0.9375rem; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	.mini-spinner {
		display: inline-block;
		width: 12px; height: 12px;
		border: 1.5px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
		vertical-align: middle;
		margin-right: 0.375rem;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

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

	.user-row { transition: background 0.15s; }
	.user-row:hover td { background: rgb(var(--copper-rgb) / 0.04); }

	.handle-text { color: var(--copper); font-weight: 500; }
	.muted { color: var(--charcoal-500); }

	.status-ready { color: var(--status-ok); font-size: 0.875rem; }
	.status-building { color: var(--copper); font-size: 0.875rem; }
	.status-none { color: var(--charcoal-600); font-size: 0.875rem; }

	.col-handle { min-width: 10rem; }
	.col-fp { width: 8rem; }
	.col-scan { width: 9rem; }
	.col-count { width: 5rem; }
	.col-actions { width: 12rem; }

	.action-btns { display: flex; gap: 0.375rem; }

	.btn-action {
		padding: 0.375rem 0.75rem;
		font-size: 0.8125rem;
		font-weight: 500;
		font-family: 'Outfit', system-ui, sans-serif;
		border-radius: 8px;
		cursor: pointer;
		transition: background 0.2s;
		text-decoration: none;
		display: inline-block;
		line-height: 1.4;
	}

	.btn-scan {
		color: var(--copper);
		background: rgb(var(--copper-rgb) / 0.1);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
	}

	.btn-scan:hover:not(:disabled) { background: rgb(var(--copper-rgb) / 0.18); }
	.btn-scan:disabled { opacity: 0.4; cursor: not-allowed; }

	.btn-view {
		color: var(--charcoal-400);
		background: rgb(var(--charcoal-400-rgb) / 0.08);
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.12);
	}

	.btn-view:hover { background: rgb(var(--charcoal-400-rgb) / 0.15); color: var(--charcoal-300); }

	.btn-delete {
		color: var(--status-error);
		background: transparent;
		border: 1px solid rgb(var(--status-error-rgb) / 0.15);
		font-size: 0.75rem;
		padding: 0.375rem 0.5rem;
	}

	.btn-delete:hover:not(:disabled) { background: rgb(var(--status-error-rgb) / 0.08); }
	.btn-delete:disabled { opacity: 0.4; cursor: not-allowed; }

	@media (max-width: 640px) {
		.add-form { flex-direction: column; }
		.add-input-wrap { width: 100%; }
		.btn-add { width: 100%; }
		.action-btns { flex-wrap: wrap; }
	}
</style>
