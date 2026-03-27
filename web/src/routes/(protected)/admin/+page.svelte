<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { getAdminUsers, preSeedUser, triggerAdminScan, deleteAdminUser } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';
	import type { AdminUser } from '$lib/types.js';

	let users = $state<AdminUser[]>([]);
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

	async function loadUsers() {
		try {
			const res = await getAdminUsers();
			users = res.users;
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
			}
		} finally {
			loading = false;
		}
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
			if (anyBuilding) {
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
								<td class="col-scan muted">{formatDate(user.last_scan_at)}</td>
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
		color: #fffbeb;
	}

	.section-title {
		font-size: 1rem;
		font-weight: 500;
		color: #d6d3d1;
		letter-spacing: 0.01em;
		margin-bottom: 0.875rem;
	}

	/* Add user form */
	.add-section {
		margin-bottom: 2.5rem;
		padding: 1.25rem;
		background: rgba(28, 25, 23, 0.5);
		border: 1px solid rgba(168, 162, 158, 0.1);
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
		background: rgba(12, 10, 9, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.15);
		border-radius: 10px;
		padding: 0 0.875rem;
		transition: border-color 0.2s;
	}

	.add-input-wrap:focus-within {
		border-color: #c9956c;
		box-shadow: 0 0 0 2px rgba(201, 149, 108, 0.1);
	}

	.input-at { color: #44403c; font-size: 0.9375rem; margin-right: 0.25rem; }

	.add-input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 0.625rem 0;
		font-size: 0.9375rem;
		font-family: 'Outfit', system-ui, sans-serif;
		color: #fef3c7;
		outline: none;
	}

	.add-input::placeholder { color: #44403c; }
	.add-input:disabled { opacity: 0.5; }

	.btn-add {
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
		white-space: nowrap;
	}

	.btn-add:hover:not(:disabled) { transform: translateY(-1px); box-shadow: 0 6px 16px -2px rgba(245, 158, 11, 0.45); }
	.btn-add:disabled { opacity: 0.6; cursor: not-allowed; transform: none; box-shadow: none; }

	.msg-error { font-size: 0.8125rem; color: #f87171; margin-top: 0.625rem; }
	.msg-success { font-size: 0.8125rem; color: #86efac; margin-top: 0.625rem; }

	/* Users table */
	.users-section { margin-top: 1rem; }

	.loading-state { display: flex; justify-content: center; padding: 3rem 0; }
	.empty-state { padding: 3rem 0; text-align: center; color: #57534e; font-size: 0.9375rem; }

	.spinner {
		width: 32px; height: 32px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	.mini-spinner {
		display: inline-block;
		width: 12px; height: 12px;
		border: 1.5px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
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
		color: #57534e;
		border-bottom: 1px solid rgba(168, 162, 158, 0.08);
	}

	.table td {
		padding: 0.75rem 0.75rem;
		border-bottom: 1px solid rgba(168, 162, 158, 0.05);
		color: #d6d3d1;
	}

	.user-row { transition: background 0.15s; }
	.user-row:hover td { background: rgba(201, 149, 108, 0.04); }

	.handle-text { color: #c9956c; font-weight: 500; }
	.muted { color: #78716c; }

	.status-ready { color: #86efac; font-size: 0.875rem; }
	.status-building { color: #c9956c; font-size: 0.875rem; }
	.status-none { color: #57534e; font-size: 0.875rem; }

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
		color: #c9956c;
		background: rgba(201, 149, 108, 0.1);
		border: 1px solid rgba(201, 149, 108, 0.2);
	}

	.btn-scan:hover:not(:disabled) { background: rgba(201, 149, 108, 0.18); }
	.btn-scan:disabled { opacity: 0.4; cursor: not-allowed; }

	.btn-view {
		color: #a8a29e;
		background: rgba(168, 162, 158, 0.08);
		border: 1px solid rgba(168, 162, 158, 0.12);
	}

	.btn-view:hover { background: rgba(168, 162, 158, 0.15); color: #d6d3d1; }

	.btn-delete {
		color: #f87171;
		background: transparent;
		border: 1px solid rgba(248, 113, 113, 0.15);
		font-size: 0.75rem;
		padding: 0.375rem 0.5rem;
	}

	.btn-delete:hover:not(:disabled) { background: rgba(248, 113, 113, 0.08); }
	.btn-delete:disabled { opacity: 0.4; cursor: not-allowed; }

	@media (max-width: 640px) {
		.add-form { flex-direction: column; }
		.add-input-wrap { width: 100%; }
		.btn-add { width: 100%; }
		.action-btns { flex-wrap: wrap; }
	}
</style>
