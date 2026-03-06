<script lang="ts">
	import { goto } from '$app/navigation';
	import { login } from '$lib/api.js';

	let password = $state('');
	let isSubmitting = $state(false);
	let isFocused = $state(false);
	let errorMessage = $state('');

	async function handleSubmit(e: SubmitEvent) {
		e.preventDefault();
		if (!password.trim() || isSubmitting) return;

		isSubmitting = true;
		errorMessage = '';

		try {
			await login(password);
			await goto('/dashboard');
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : 'Login failed';
		} finally {
			isSubmitting = false;
		}
	}
</script>

<svelte:head>
	<title>Sign In - Charcoal</title>
	<link rel="preconnect" href="https://fonts.googleapis.com" />
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
	<link
		href="https://fonts.googleapis.com/css2?family=Libre+Baskerville:ital,wght@0,400;0,700;1,400&family=Outfit:wght@300;400;500;600&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

<div class="ground">
	<!-- Ambient background layers -->
	<div class="bg-base"></div>
	<div class="bg-grain"></div>
	<div class="bg-warmth"></div>

	<main class="container">
		<!-- Subtle ambient orbs -->
		<div class="ambient-orbs" aria-hidden="true">
			<div class="orb orb-1"></div>
			<div class="orb orb-2"></div>
		</div>

		<div class="content">
			<header class="header">
				<div class="logo" aria-hidden="true">
					<svg viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
						<circle cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1.5" opacity="0.2" class="ring ring-outer" />
						<circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.4" class="ring ring-middle" />
						<circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.7" class="ring ring-inner" />
						<circle cx="32" cy="32" r="5" fill="currentColor" class="core" />
					</svg>
				</div>
				<h1 class="title">Charcoal</h1>
				<p class="tagline">Engage with confidence</p>
			</header>

			<div class="card" class:focused={isFocused}>
				<div class="card-inner">
					<p class="description">
						Personal threat intelligence dashboard. Sign in to view your threat analysis and
						amplification events.
					</p>

					<form onsubmit={handleSubmit}>
						<div class="field">
							<label for="password" class="label">Password</label>
							<div class="input-container" class:focused={isFocused}>
								<input
									type="password"
									id="password"
									name="password"
									bind:value={password}
									onfocus={() => (isFocused = true)}
									onblur={() => (isFocused = false)}
									placeholder="Enter dashboard password"
									autocomplete="current-password"
									required
									disabled={isSubmitting}
								/>
							</div>
							{#if errorMessage}
								<p class="error">{errorMessage}</p>
							{:else}
								<p class="hint">Single-user personal dashboard</p>
							{/if}
						</div>

						<button type="submit" class="btn-continue" disabled={!password.trim() || isSubmitting}>
							{#if isSubmitting}
								<span class="loading-pulse"></span>
								<span>Signing in...</span>
							{:else}
								<span>Continue</span>
								<svg class="arrow" viewBox="0 0 20 20" fill="none">
									<path
										d="M4 10h12m-4-4l4 4-4 4"
										stroke="currentColor"
										stroke-width="2"
										stroke-linecap="round"
										stroke-linejoin="round"
									/>
								</svg>
							{/if}
						</button>
					</form>
				</div>
			</div>

			<footer class="footer">
				<p>Your data stays local. Charcoal runs on your own infrastructure.</p>
			</footer>
		</div>
	</main>
</div>

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
		--amber-600: #d97706;
		--copper: #c9956c;
		--copper-glow: rgba(201, 149, 108, 0.3);
		--font-display: 'Libre Baskerville', Georgia, serif;
		--font-body: 'Outfit', system-ui, sans-serif;
		--ease-out-expo: cubic-bezier(0.16, 1, 0.3, 1);
		--ease-in-out: cubic-bezier(0.4, 0, 0.2, 1);
	}

	* { box-sizing: border-box; margin: 0; padding: 0; }

	.ground {
		min-height: 100vh;
		min-height: 100dvh;
		position: relative;
		overflow: hidden;
		font-family: var(--font-body);
		color: var(--cream-100);
		-webkit-font-smoothing: antialiased;
	}

	.bg-base {
		position: fixed;
		inset: 0;
		background: linear-gradient(165deg, var(--charcoal-900) 0%, var(--charcoal-950) 50%, #0a0705 100%);
	}

	.bg-grain {
		position: fixed;
		inset: 0;
		opacity: 0.08;
		background: radial-gradient(circle at 20% 80%, rgba(201, 149, 108, 0.15) 0%, transparent 50%),
			radial-gradient(circle at 80% 20%, rgba(245, 158, 11, 0.1) 0%, transparent 40%);
		pointer-events: none;
	}

	.bg-warmth {
		position: fixed;
		inset: 0;
		background: radial-gradient(ellipse 80% 60% at 50% 40%, rgba(201, 149, 108, 0.06) 0%, transparent 60%);
		pointer-events: none;
	}

	.ambient-orbs { position: fixed; inset: 0; pointer-events: none; overflow: hidden; }

	.orb {
		position: absolute;
		border-radius: 50%;
		filter: blur(80px);
		animation: drift 25s ease-in-out infinite;
	}

	.orb-1 {
		width: 500px; height: 500px;
		top: -20%; right: -15%;
		background: radial-gradient(circle, rgba(201, 149, 108, 0.15) 0%, transparent 70%);
	}

	.orb-2 {
		width: 400px; height: 400px;
		bottom: -10%; left: -10%;
		background: radial-gradient(circle, rgba(245, 158, 11, 0.1) 0%, transparent 70%);
		animation-delay: -12s;
	}

	@keyframes drift {
		0%, 100% { transform: translate(0, 0) scale(1); }
		25% { transform: translate(30px, -20px) scale(1.05); }
		50% { transform: translate(-20px, 30px) scale(0.95); }
		75% { transform: translate(20px, 20px) scale(1.02); }
	}

	.container {
		position: relative;
		min-height: 100vh;
		min-height: 100dvh;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 2rem;
	}

	.content {
		width: 100%;
		max-width: 400px;
		animation: emerge 1s var(--ease-out-expo) forwards;
	}

	@keyframes emerge {
		from { opacity: 0; transform: translateY(24px); }
		to { opacity: 1; transform: translateY(0); }
	}

	.header { text-align: center; margin-bottom: 2.5rem; }

	.logo {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 72px; height: 72px;
		margin-bottom: 1.25rem;
		color: var(--copper);
		animation: emerge 1s var(--ease-out-expo) 0.1s backwards;
	}

	.logo svg { width: 100%; height: 100%; }

	.ring { transform-origin: center; }
	.ring-outer { animation: pulse-ring 4s ease-in-out infinite; }
	.ring-middle { animation: pulse-ring 4s ease-in-out 0.5s infinite; }
	.ring-inner { animation: pulse-ring 4s ease-in-out 1s infinite; }
	.core { animation: breathe 4s ease-in-out infinite; }

	@keyframes pulse-ring {
		0%, 100% { opacity: 0.2; transform: scale(1); }
		50% { opacity: 0.5; transform: scale(1.02); }
	}

	@keyframes breathe {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.8; transform: scale(0.95); }
	}

	.title {
		font-family: var(--font-display);
		font-size: 2.25rem;
		font-weight: 400;
		letter-spacing: -0.01em;
		color: var(--cream-50);
		margin-bottom: 0.5rem;
		animation: emerge 1s var(--ease-out-expo) 0.15s backwards;
	}

	.tagline {
		font-size: 1.0625rem;
		font-weight: 300;
		color: var(--charcoal-400);
		letter-spacing: 0.02em;
		animation: emerge 1s var(--ease-out-expo) 0.2s backwards;
	}

	.card {
		position: relative;
		background: linear-gradient(145deg, rgba(41, 37, 36, 0.8) 0%, rgba(28, 25, 23, 0.9) 100%);
		backdrop-filter: blur(20px);
		border-radius: 20px;
		border: 1px solid rgba(168, 162, 158, 0.1);
		box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.3), 0 20px 50px -10px rgba(0, 0, 0, 0.5), 0 0 80px -20px var(--copper-glow);
		transition: all 0.5s var(--ease-out-expo);
		animation: emerge 1s var(--ease-out-expo) 0.25s backwards;
	}

	.card.focused {
		border-color: rgba(201, 149, 108, 0.3);
		box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.3), 0 25px 60px -10px rgba(0, 0, 0, 0.6), 0 0 100px -20px var(--copper-glow);
	}

	.card-inner { padding: 2rem; }

	.description {
		font-size: 0.9375rem;
		line-height: 1.7;
		color: var(--charcoal-300);
		text-align: center;
		margin-bottom: 2rem;
		font-weight: 300;
	}

	.field { margin-bottom: 1.5rem; }

	.label {
		display: block;
		font-size: 0.8125rem;
		font-weight: 500;
		color: var(--charcoal-300);
		margin-bottom: 0.625rem;
		letter-spacing: 0.03em;
		text-transform: uppercase;
	}

	.input-container {
		display: flex;
		align-items: center;
		background: rgba(12, 10, 9, 0.6);
		border: 1px solid rgba(168, 162, 158, 0.15);
		border-radius: 12px;
		padding: 0 1rem;
		transition: all 0.3s var(--ease-in-out);
	}

	.input-container.focused {
		border-color: var(--copper);
		background: rgba(12, 10, 9, 0.8);
		box-shadow: 0 0 0 3px rgba(201, 149, 108, 0.15);
	}

	.input-container input {
		flex: 1;
		border: none;
		background: transparent;
		padding: 1rem 0;
		font-size: 1rem;
		font-family: var(--font-body);
		font-weight: 400;
		color: var(--cream-100);
		outline: none;
		width: 100%;
	}

	.input-container input::placeholder { color: var(--charcoal-600); }
	.input-container input:disabled { opacity: 0.5; }

	.hint {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
		margin-top: 0.75rem;
		text-align: center;
		font-weight: 300;
	}

	.error {
		font-size: 0.8125rem;
		color: #f87171;
		margin-top: 0.75rem;
		text-align: center;
		font-weight: 400;
	}

	.btn-continue {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.625rem;
		padding: 1rem 1.5rem;
		font-size: 1rem;
		font-weight: 500;
		font-family: var(--font-body);
		color: var(--charcoal-950);
		background: linear-gradient(135deg, var(--amber-500) 0%, var(--copper) 100%);
		border: none;
		border-radius: 12px;
		cursor: pointer;
		transition: all 0.3s var(--ease-out-expo);
		box-shadow: 0 4px 15px -3px rgba(245, 158, 11, 0.4), inset 0 1px 0 rgba(255, 255, 255, 0.2);
	}

	.btn-continue:hover:not(:disabled) {
		transform: translateY(-2px);
		box-shadow: 0 8px 25px -5px rgba(245, 158, 11, 0.5), inset 0 1px 0 rgba(255, 255, 255, 0.2);
	}

	.btn-continue:active:not(:disabled) { transform: translateY(0); }
	.btn-continue:disabled { opacity: 0.4; cursor: not-allowed; transform: none; }

	.arrow {
		width: 18px; height: 18px;
		transition: transform 0.3s var(--ease-out-expo);
	}

	.btn-continue:hover:not(:disabled) .arrow { transform: translateX(4px); }

	.loading-pulse {
		width: 16px; height: 16px;
		background: var(--charcoal-950);
		border-radius: 50%;
		animation: pulse 1.2s ease-in-out infinite;
	}

	@keyframes pulse {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.5; transform: scale(0.85); }
	}

	.footer {
		margin-top: 2rem;
		text-align: center;
		animation: emerge 1s var(--ease-out-expo) 0.35s backwards;
	}

	.footer p {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
		line-height: 1.6;
		font-weight: 300;
	}

	@media (max-width: 480px) {
		.container { padding: 1.5rem; }
		.title { font-size: 1.875rem; }
		.logo { width: 64px; height: 64px; }
		.card-inner { padding: 1.5rem; }
	}

	@media (prefers-reduced-motion: reduce) {
		.orb, .content, .header, .logo, .title, .tagline, .card, .footer, .ring, .core {
			animation: none;
		}
		.card, .input-container, .btn-continue, .arrow { transition: none; }
		.loading-pulse { animation: none; opacity: 0.7; }
	}
</style>
