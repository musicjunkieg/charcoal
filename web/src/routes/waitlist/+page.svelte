<!--
	/waitlist — where a not-yet-allowed sign-in lands (#309).

	Content contract (spec, binding): confirms the person is on the list and
	nothing more. No status detail. A pending requester and a denied one see
	byte-identical output — this page must never leak which they are, because
	its audience includes exactly the population Charcoal exists to guard
	against. Tone is the Hearth Watch register: calm, warm, unhurried.
-->
<script lang="ts">
	import '$lib/website/styles/tokens.css';
</script>

<svelte:head>
	<title>You're on the list — Charcoal</title>
	<link rel="preconnect" href="https://fonts.googleapis.com" />
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
	<link
		href="https://fonts.googleapis.com/css2?family=Libre+Baskerville:ital,wght@0,400;0,700;1,400&family=Outfit:wght@300;400;500;600&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

<div class="ground">
	<div class="bg-base" aria-hidden="true"></div>
	<div class="bg-warmth" aria-hidden="true"></div>
	<div class="ambient-orbs" aria-hidden="true">
		<div class="orb orb-1"></div>
		<div class="orb orb-2"></div>
	</div>

	<main class="container">
		<div class="content">
			<div class="logo" aria-hidden="true">
				<svg viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
					<circle cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1.5" opacity="0.2" class="ring ring-outer" />
					<circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.4" class="ring ring-middle" />
					<circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.7" class="ring ring-inner" />
					<circle cx="32" cy="32" r="5" fill="currentColor" class="core" />
				</svg>
			</div>

			<h1 class="title">You're on the list.</h1>

			<p class="body-copy">
				Charcoal is inviting people in slowly, a few at a time. Your request is
				recorded — if access opens for you, signing in with Bluesky will simply
				work.
			</p>

			<p class="reassure">There's nothing else you need to do.</p>

			<a href="/" class="home-link">
				<svg class="home-arrow" viewBox="0 0 20 20" fill="none" aria-hidden="true">
					<path
						d="M16 10H4m4 4l-4-4 4-4"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
					/>
				</svg>
				<span>Back to charcoal.watch</span>
			</a>
		</div>
	</main>
</div>

<style>
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
		background: linear-gradient(165deg, var(--charcoal-900) 0%, var(--charcoal-950) 60%, #0a0705 100%);
	}

	.bg-warmth {
		position: fixed;
		inset: 0;
		background: radial-gradient(ellipse 80% 60% at 50% 38%, rgb(var(--copper-rgb) / 0.06) 0%, transparent 60%);
		pointer-events: none;
	}

	.ambient-orbs {
		position: fixed;
		inset: 0;
		pointer-events: none;
		overflow: hidden;
	}

	.orb {
		position: absolute;
		border-radius: 50%;
		filter: blur(80px);
		animation: drift 28s ease-in-out infinite;
	}

	.orb-1 {
		width: 460px;
		height: 460px;
		top: -18%;
		right: -12%;
		background: radial-gradient(circle, rgb(var(--copper-rgb) / 0.13) 0%, transparent 70%);
	}

	.orb-2 {
		width: 380px;
		height: 380px;
		bottom: -12%;
		left: -10%;
		background: radial-gradient(circle, rgb(var(--amber-500-rgb) / 0.08) 0%, transparent 70%);
		animation-delay: -13s;
	}

	@keyframes drift {
		0%, 100% { transform: translate(0, 0) scale(1); }
		25% { transform: translate(28px, -18px) scale(1.04); }
		50% { transform: translate(-18px, 26px) scale(0.96); }
		75% { transform: translate(18px, 18px) scale(1.02); }
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
		text-align: center;
		animation: emerge 1s var(--ease-out-expo) forwards;
	}

	@keyframes emerge {
		from { opacity: 0; transform: translateY(24px); }
		to { opacity: 1; transform: translateY(0); }
	}

	.logo {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 72px;
		height: 72px;
		margin-bottom: 1.75rem;
		color: var(--copper);
		animation: emerge 1s var(--ease-out-expo) 0.1s backwards;
	}

	.logo svg {
		width: 100%;
		height: 100%;
	}

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
		font-size: clamp(1.75rem, 4vw, 2.5rem);
		font-weight: 400;
		line-height: 1.2;
		letter-spacing: -0.01em;
		color: var(--cream-50);
		margin-bottom: 1.25rem;
		animation: emerge 1s var(--ease-out-expo) 0.15s backwards;
	}

	.body-copy {
		font-size: 1rem;
		font-weight: 300;
		line-height: 1.7;
		color: var(--charcoal-300);
		margin-bottom: 1rem;
		animation: emerge 1s var(--ease-out-expo) 0.2s backwards;
	}

	.reassure {
		font-size: 1rem;
		font-weight: 400;
		color: var(--charcoal-400);
		margin-bottom: 2.5rem;
		animation: emerge 1s var(--ease-out-expo) 0.25s backwards;
	}

	.home-link {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.625rem 1.25rem;
		font-size: 1rem;
		font-weight: 500;
		color: var(--charcoal-300);
		text-decoration: none;
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.2);
		border-radius: 8px;
		transition: color 0.3s var(--ease-in-out), border-color 0.3s var(--ease-in-out),
			background 0.3s var(--ease-in-out);
		animation: emerge 1s var(--ease-out-expo) 0.3s backwards;
	}

	.home-link:hover {
		color: var(--cream-50);
		border-color: rgb(var(--copper-rgb) / 0.4);
		background: rgb(var(--copper-rgb) / 0.1);
	}

	.home-link:focus-visible {
		outline: none;
		border-color: var(--copper);
		box-shadow: 0 0 0 3px rgb(var(--copper-rgb) / 0.15);
	}

	.home-arrow {
		width: 16px;
		height: 16px;
		transition: transform 0.3s var(--ease-out-expo);
	}

	.home-link:hover .home-arrow {
		transform: translateX(-3px);
	}

	@media (max-width: 480px) {
		.container { padding: 1.5rem; }
		.title { font-size: 1.75rem; }
		.logo { width: 64px; height: 64px; }
	}

	@media (prefers-reduced-motion: reduce) {
		.orb, .content, .logo, .title, .body-copy, .reassure, .home-link, .ring, .core {
			animation: none;
		}
		.home-link, .home-arrow { transition: none; }
	}
</style>
