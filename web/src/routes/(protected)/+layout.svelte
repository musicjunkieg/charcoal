<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { getStatus, logout } from '$lib/api.js';
	import { AuthError } from '$lib/api.js';

	let { children } = $props();
	let checking = $state(true);

	onMount(async () => {
		try {
			await getStatus();
		} catch (err) {
			if (err instanceof AuthError) {
				await goto('/login');
				return;
			}
			// Non-auth error (network, server error) — still allow through;
			// individual pages handle error states.
		} finally {
			checking = false;
		}
	});
</script>

<svelte:head>
	<link rel="preconnect" href="https://fonts.googleapis.com" />
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
	<link
		href="https://fonts.googleapis.com/css2?family=Libre+Baskerville:ital,wght@0,400;0,700;1,400&family=Outfit:wght@300;400;500;600&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

{#if checking}
	<div class="auth-check">
		<div class="spinner"></div>
	</div>
{:else}
	<div class="app">
		<nav class="nav">
			<a href="/dashboard" class="nav-brand">
				<svg class="nav-logo" viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
					<circle cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
					<circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.5" />
					<circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.8" />
					<circle cx="32" cy="32" r="5" fill="currentColor" />
				</svg>
				<span class="nav-title">Charcoal</span>
			</a>

			<div class="nav-links">
				<a
					href="/dashboard"
					class="nav-link"
					class:active={$page.url.pathname === '/dashboard'}
				>Dashboard</a>
				<a
					href="/accounts"
					class="nav-link"
					class:active={$page.url.pathname.startsWith('/accounts')}
				>Accounts</a>
				<button
					class="nav-logout"
					onclick={async () => { await logout(); await goto('/login'); }}
				>Sign out</button>
			</div>
		</nav>

		<main class="main">
			{@render children()}
		</main>
	</div>
{/if}

<style>
	:root {
		--charcoal-950: #0c0a09;
		--charcoal-900: #1c1917;
		--charcoal-800: #292524;
		--charcoal-700: #44403c;
		--charcoal-600: #57534e;
		--charcoal-500: #78716c;
		--charcoal-400: #a8a29e;
		--charcoal-300: #d6d3d1;
		--cream-50: #fffbeb;
		--cream-100: #fef3c7;
		--amber-500: #f59e0b;
		--copper: #c9956c;
		--copper-glow: rgba(201, 149, 108, 0.25);
		--font-display: 'Libre Baskerville', Georgia, serif;
		--font-body: 'Outfit', system-ui, sans-serif;
	}

	* { box-sizing: border-box; margin: 0; padding: 0; }

	.auth-check {
		min-height: 100vh;
		display: flex;
		align-items: center;
		justify-content: center;
		background: #0c0a09;
	}

	.spinner {
		width: 32px;
		height: 32px;
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: var(--copper);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin { to { transform: rotate(360deg); } }

	.app {
		min-height: 100vh;
		background: var(--charcoal-950);
		font-family: var(--font-body);
		color: var(--cream-100);
		-webkit-font-smoothing: antialiased;
	}

	.nav {
		position: sticky;
		top: 0;
		z-index: 10;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0 2rem;
		height: 56px;
		background: rgba(12, 10, 9, 0.9);
		backdrop-filter: blur(12px);
		border-bottom: 1px solid rgba(168, 162, 158, 0.08);
	}

	.nav-brand {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		text-decoration: none;
		color: var(--cream-100);
	}

	.nav-logo {
		width: 28px;
		height: 28px;
		color: var(--copper);
	}

	.nav-title {
		font-family: var(--font-display);
		font-size: 1.125rem;
		font-weight: 400;
		letter-spacing: -0.01em;
	}

	.nav-links {
		display: flex;
		align-items: center;
		gap: 0.25rem;
	}

	.nav-link {
		padding: 0.375rem 0.875rem;
		font-size: 0.875rem;
		font-weight: 400;
		color: var(--charcoal-400);
		text-decoration: none;
		border-radius: 8px;
		transition: color 0.2s, background 0.2s;
	}

	.nav-link:hover { color: var(--cream-100); background: rgba(168, 162, 158, 0.08); }
	.nav-link.active { color: var(--cream-100); background: rgba(201, 149, 108, 0.12); }

	.nav-logout {
		padding: 0.375rem 0.875rem;
		font-size: 0.875rem;
		font-weight: 400;
		color: var(--charcoal-500);
		background: none;
		border: none;
		border-radius: 8px;
		cursor: pointer;
		font-family: var(--font-body);
		transition: color 0.2s;
	}

	.nav-logout:hover { color: var(--charcoal-300); }

	.main {
		max-width: 1200px;
		margin: 0 auto;
		padding: 2rem 2rem;
	}

	@media (max-width: 640px) {
		.nav { padding: 0 1rem; }
		.main { padding: 1.5rem 1rem; }
		.nav-title { display: none; }
	}
</style>
