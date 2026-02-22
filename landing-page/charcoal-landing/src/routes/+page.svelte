<script lang="ts">
	import { onMount } from 'svelte';

	// Leaflet publication URL - update this when you create your publication
	const LEAFLET_URL = 'https://charcoal.leaflet.pub';

	// Intersection Observer for scroll animations
	let sectionsVisible = $state<Record<string, boolean>>({});

	onMount(() => {
		const observer = new IntersectionObserver(
			(entries) => {
				entries.forEach((entry) => {
					if (entry.isIntersecting) {
						sectionsVisible[entry.target.id] = true;
					}
				});
			},
			{ threshold: 0.15, rootMargin: '0px 0px -50px 0px' }
		);

		document.querySelectorAll('[data-animate]').forEach((el) => {
			observer.observe(el);
		});

		return () => observer.disconnect();
	});
</script>

<svelte:head>
	<title>Charcoal - Engage with confidence on Bluesky</title>
	<meta
		name="description"
		content="Charcoal absorbs harmful engagement so you can focus on good-faith conversations. Join the waitlist for early access."
	/>
	<link rel="preconnect" href="https://fonts.googleapis.com" />
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
	<link
		href="https://fonts.googleapis.com/css2?family=Libre+Baskerville:ital,wght@0,400;0,700;1,400&family=Outfit:wght@300;400;500;600&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

<div class="page">
	<!-- Background layers -->
	<div class="bg-base" aria-hidden="true"></div>
	<div class="bg-warmth" aria-hidden="true"></div>

	<!-- Ambient orbs -->
	<div class="ambient-orbs" aria-hidden="true">
		<div class="orb orb-1"></div>
		<div class="orb orb-2"></div>
		<div class="orb orb-3"></div>
	</div>

	<!-- Navigation -->
	<nav class="nav">
		<a href="/" class="nav-logo" aria-label="Charcoal home">
			<svg viewBox="0 0 64 64" fill="none">
				<circle cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
				<circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.5" />
				<circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.7" />
				<circle cx="32" cy="32" r="5" fill="currentColor" />
			</svg>
		</a>
		<a href="/auth/login" class="nav-login">Sign in</a>
	</nav>

	<main>
		<!-- Hero Section -->
		<section class="hero">
			<div class="hero-content">
				<div class="hero-logo" aria-hidden="true">
					<svg viewBox="0 0 64 64" fill="none" class="logo-svg">
						<circle
							cx="32"
							cy="32"
							r="30"
							stroke="currentColor"
							stroke-width="1"
							opacity="0.15"
							class="ring ring-1"
						/>
						<circle
							cx="32"
							cy="32"
							r="26"
							stroke="currentColor"
							stroke-width="1"
							opacity="0.25"
							class="ring ring-2"
						/>
						<circle
							cx="32"
							cy="32"
							r="22"
							stroke="currentColor"
							stroke-width="1.5"
							opacity="0.35"
							class="ring ring-3"
						/>
						<circle
							cx="32"
							cy="32"
							r="18"
							stroke="currentColor"
							stroke-width="1.5"
							opacity="0.5"
							class="ring ring-4"
						/>
						<circle
							cx="32"
							cy="32"
							r="14"
							stroke="currentColor"
							stroke-width="2"
							opacity="0.7"
							class="ring ring-5"
						/>
						<circle cx="32" cy="32" r="6" fill="currentColor" class="core" />
					</svg>
				</div>

				<p class="hero-eyebrow">For Bluesky</p>
				<h1 class="hero-title">
					<span class="title-word">Engage</span>
					<span class="title-word">with</span>
					<span class="title-word title-accent">confidence</span>
				</h1>
				<p class="hero-subtitle">
					Charcoal absorbs harmful engagement so you can focus on what matters—authentic
					conversations and open dialogue.
				</p>

				<a href="#early-access" class="hero-cta">
					<span>Join the waitlist</span>
					<svg viewBox="0 0 20 20" fill="none" class="cta-arrow">
						<path
							d="M10 4v12m0 0l-4-4m4 4l4-4"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
						/>
					</svg>
				</a>
			</div>

			<div class="scroll-hint" aria-hidden="true">
				<span class="scroll-line"></span>
			</div>
		</section>

		<!-- How It Works Section -->
		<section id="how-it-works" class="section how-it-works" data-animate class:visible={sectionsVisible['how-it-works']}>
			<div class="section-inner">
				<h2 class="section-title">How Charcoal works</h2>
				<p class="section-subtitle">
					Like activated charcoal absorbs toxins, we filter out bad-faith engagement while letting
					authentic interactions through.
				</p>

				<div class="pipeline">
					<div class="pipeline-step step-1">
						<div class="step-icon">
							<svg viewBox="0 0 48 48" fill="none">
								<circle cx="24" cy="24" r="20" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
								<circle cx="24" cy="24" r="12" stroke="currentColor" stroke-width="1.5" opacity="0.5" />
								<circle cx="24" cy="24" r="4" fill="currentColor" />
							</svg>
						</div>
						<h3 class="step-title">Monitor</h3>
						<p class="step-desc">
							Charcoal watches your engagement in the background, tracking interactions as they
							happen.
						</p>
					</div>

					<div class="pipeline-connector" aria-hidden="true">
						<svg viewBox="0 0 80 24" fill="none">
							<path
								d="M0 12h80"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-dasharray="4 4"
								opacity="0.3"
							/>
							<circle cx="40" cy="12" r="3" fill="currentColor" opacity="0.5" />
						</svg>
					</div>

					<div class="pipeline-step step-2">
						<div class="step-icon absorbing">
							<svg viewBox="0 0 48 48" fill="none">
								<circle
									cx="24"
									cy="24"
									r="20"
									stroke="currentColor"
									stroke-width="1.5"
									opacity="0.2"
									class="absorb-ring"
								/>
								<circle
									cx="24"
									cy="24"
									r="14"
									stroke="currentColor"
									stroke-width="1.5"
									opacity="0.4"
									class="absorb-ring"
								/>
								<circle
									cx="24"
									cy="24"
									r="8"
									stroke="currentColor"
									stroke-width="2"
									opacity="0.6"
									class="absorb-ring"
								/>
								<circle cx="24" cy="24" r="3" fill="currentColor" />
							</svg>
							<!-- Absorbed particles -->
							<div class="particles" aria-hidden="true">
								<span class="particle p1"></span>
								<span class="particle p2"></span>
								<span class="particle p3"></span>
							</div>
						</div>
						<h3 class="step-title">Absorb</h3>
						<p class="step-desc">
							Harmful patterns are identified and absorbed—harassment, bad faith, and toxic
							behavior.
						</p>
					</div>

					<div class="pipeline-connector" aria-hidden="true">
						<svg viewBox="0 0 80 24" fill="none">
							<path
								d="M0 12h80"
								stroke="currentColor"
								stroke-width="1.5"
								stroke-dasharray="4 4"
								opacity="0.3"
							/>
							<circle cx="40" cy="12" r="3" fill="currentColor" opacity="0.5" />
						</svg>
					</div>

					<div class="pipeline-step step-3">
						<div class="step-icon">
							<svg viewBox="0 0 48 48" fill="none">
								<circle
									cx="24"
									cy="24"
									r="18"
									stroke="currentColor"
									stroke-width="1.5"
									opacity="0.3"
								/>
								<path
									d="M16 24l6 6 10-12"
									stroke="currentColor"
									stroke-width="2.5"
									stroke-linecap="round"
									stroke-linejoin="round"
								/>
							</svg>
						</div>
						<h3 class="step-title">Protect</h3>
						<p class="step-desc">
							You decide what happens—auto-mute or block, then review later and receive a digest of flagged
							accounts.
						</p>
					</div>
				</div>
			</div>
		</section>

		<!-- Benefits Section -->
		<section id="benefits" class="section benefits" data-animate class:visible={sectionsVisible['benefits']}>
			<div class="section-inner">
				<div class="benefits-grid">
					<div class="benefit">
						<div class="benefit-icon">
							<svg viewBox="0 0 32 32" fill="none">
								<circle cx="16" cy="16" r="14" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
								<path
									d="M16 8v8l5 3"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
								/>
							</svg>
						</div>
						<h3 class="benefit-title">Less time worrying</h3>
						<p class="benefit-desc">
							Stop scanning every notification for potential harm. Charcoal handles the vigilance so
							you don't have to.
						</p>
					</div>

					<div class="benefit">
						<div class="benefit-icon">
							<svg viewBox="0 0 32 32" fill="none">
								<circle cx="16" cy="16" r="14" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
								<circle cx="16" cy="16" r="6" stroke="currentColor" stroke-width="2" />
								<circle cx="16" cy="16" r="2" fill="currentColor" />
							</svg>
						</div>
						<h3 class="benefit-title">Stay centered</h3>
						<p class="benefit-desc">
							Maintain your calm when engaging online. No more bracing for impact with every reply.
						</p>
					</div>

					<div class="benefit">
						<div class="benefit-icon">
							<svg viewBox="0 0 32 32" fill="none">
								<circle cx="16" cy="16" r="14" stroke="currentColor" stroke-width="1.5" opacity="0.3" />
								<path
									d="M10 16h12M16 10v12"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
								/>
							</svg>
						</div>
						<h3 class="benefit-title">More authentic connection</h3>
						<p class="benefit-desc">
							When the noise is absorbed, what remains is what matters—good-faith conversations and
							genuine community.
						</p>
					</div>
				</div>
			</div>
		</section>

		<!-- Social Proof Section -->
		<section id="voices" class="section voices" data-animate class:visible={sectionsVisible['voices']}>
			<div class="section-inner">
				<h2 class="section-title">Voices from the community</h2>

				<div class="testimonials">
					<blockquote class="testimonial">
						<p class="testimonial-text">
							"I used to dread checking my mentions. Now I actually look forward to engaging again."
						</p>
						<footer class="testimonial-author">
							<span class="author-name">Early tester</span>
							<span class="author-handle">@community.bsky.social</span>
						</footer>
					</blockquote>

					<blockquote class="testimonial">
						<p class="testimonial-text">
							"It's like having a thoughtful friend who handles the difficult conversations you'd
							rather avoid."
						</p>
						<footer class="testimonial-author">
							<span class="author-name">Beta user</span>
							<span class="author-handle">@earlyaccess.bsky.social</span>
						</footer>
					</blockquote>

					<blockquote class="testimonial">
						<p class="testimonial-text">
							"Finally, I can focus on the conversations that actually matter to me."
						</p>
						<footer class="testimonial-author">
							<span class="author-name">Waitlist member</span>
							<span class="author-handle">@waiting.bsky.social</span>
						</footer>
					</blockquote>
				</div>
			</div>
		</section>

		<!-- Early Access Section -->
		<section id="early-access" class="section early-access" data-animate class:visible={sectionsVisible['early-access']}>
			<div class="section-inner">
				<div class="cta-card">
					<div class="cta-logo" aria-hidden="true">
						<svg viewBox="0 0 64 64" fill="none">
							<circle
								cx="32"
								cy="32"
								r="28"
								stroke="currentColor"
								stroke-width="1"
								opacity="0.2"
							/>
							<circle
								cx="32"
								cy="32"
								r="20"
								stroke="currentColor"
								stroke-width="1.5"
								opacity="0.4"
							/>
							<circle
								cx="32"
								cy="32"
								r="12"
								stroke="currentColor"
								stroke-width="2"
								opacity="0.6"
							/>
							<circle cx="32" cy="32" r="5" fill="currentColor" />
						</svg>
					</div>

					<h2 class="cta-title">Get early access</h2>
					<p class="cta-subtitle">
						Be among the first to experience a calmer way to engage on Bluesky. Subscribe to our
						publication and we'll notify you when Charcoal is ready.
					</p>

					<a href={LEAFLET_URL} target="_blank" rel="noopener noreferrer" class="subscribe-btn">
						<svg viewBox="0 0 24 24" fill="none" class="leaflet-icon">
							<path
								d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"
							/>
						</svg>
						<span>Subscribe on Leaflet</span>
						<svg viewBox="0 0 20 20" fill="none" class="external-icon">
							<path
								d="M11 3h6v6M6 14l11-11"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"
							/>
						</svg>
					</a>
					<p class="subscribe-hint">
						Updates delivered to your Bluesky feed. No email required.
					</p>
				</div>
			</div>
		</section>
	</main>

	<!-- Footer -->
	<footer class="site-footer">
		<div class="footer-inner">
			<div class="footer-brand">
				<svg viewBox="0 0 64 64" fill="none" class="footer-logo">
					<circle cx="32" cy="32" r="22" stroke="currentColor" stroke-width="1.5" opacity="0.4" />
					<circle cx="32" cy="32" r="14" stroke="currentColor" stroke-width="2" opacity="0.6" />
					<circle cx="32" cy="32" r="5" fill="currentColor" />
				</svg>
				<span class="footer-name">Charcoal</span>
			</div>

			<nav class="footer-nav">
				<a href="/auth/login">Sign in</a>
				<a href="/privacy">Privacy</a>
				<a href="/terms">Terms</a>
			</nav>

			<p class="footer-copy">&copy; 2024 Charcoal. Engage with confidence.</p>
		</div>
	</footer>
</div>

<style>
	/* ===== Design Tokens ===== */
	:root {
		/* Warm charcoal palette */
		--charcoal-950: #0c0a09;
		--charcoal-900: #1c1917;
		--charcoal-800: #292524;
		--charcoal-700: #44403c;
		--charcoal-600: #57534e;
		--charcoal-500: #78716c;
		--charcoal-400: #a8a29e;
		--charcoal-300: #d6d3d1;

		/* Warm cream tones */
		--cream-50: #fffbeb;
		--cream-100: #fef3c7;

		/* Amber accent */
		--amber-500: #f59e0b;
		--amber-600: #d97706;

		/* Copper accent */
		--copper: #c9956c;
		--copper-glow: rgba(201, 149, 108, 0.3);

		/* Typography */
		--font-display: 'Libre Baskerville', Georgia, serif;
		--font-body: 'Outfit', system-ui, sans-serif;

		/* Easing */
		--ease-out-expo: cubic-bezier(0.16, 1, 0.3, 1);
		--ease-in-out: cubic-bezier(0.4, 0, 0.2, 1);
	}

	/* ===== Reset ===== */
	* {
		box-sizing: border-box;
		margin: 0;
		padding: 0;
	}

	/* ===== Page Container ===== */
	.page {
		min-height: 100vh;
		position: relative;
		font-family: var(--font-body);
		color: var(--cream-100);
		-webkit-font-smoothing: antialiased;
		-moz-osx-font-smoothing: grayscale;
		overflow-x: hidden;
	}

	/* ===== Background ===== */
	.bg-base {
		position: fixed;
		inset: 0;
		background: linear-gradient(
			165deg,
			var(--charcoal-900) 0%,
			var(--charcoal-950) 50%,
			#0a0705 100%
		);
		z-index: -3;
	}

	.bg-warmth {
		position: fixed;
		inset: 0;
		background: radial-gradient(
				ellipse 100% 80% at 50% 20%,
				rgba(201, 149, 108, 0.08) 0%,
				transparent 60%
			),
			radial-gradient(ellipse 80% 60% at 20% 80%, rgba(245, 158, 11, 0.05) 0%, transparent 50%),
			radial-gradient(ellipse 60% 50% at 90% 50%, rgba(217, 119, 6, 0.04) 0%, transparent 40%);
		z-index: -2;
		pointer-events: none;
	}

	/* ===== Ambient Orbs ===== */
	.ambient-orbs {
		position: fixed;
		inset: 0;
		z-index: -1;
		pointer-events: none;
		overflow: hidden;
	}

	.orb {
		position: absolute;
		border-radius: 50%;
		filter: blur(100px);
		animation: drift 30s ease-in-out infinite;
	}

	.orb-1 {
		width: 600px;
		height: 600px;
		top: -10%;
		right: -10%;
		background: radial-gradient(circle, rgba(201, 149, 108, 0.12) 0%, transparent 70%);
	}

	.orb-2 {
		width: 500px;
		height: 500px;
		bottom: 20%;
		left: -15%;
		background: radial-gradient(circle, rgba(245, 158, 11, 0.08) 0%, transparent 70%);
		animation-delay: -10s;
	}

	.orb-3 {
		width: 400px;
		height: 400px;
		top: 50%;
		right: 20%;
		background: radial-gradient(circle, rgba(201, 149, 108, 0.06) 0%, transparent 70%);
		animation-delay: -20s;
	}

	@keyframes drift {
		0%,
		100% {
			transform: translate(0, 0) scale(1);
		}
		33% {
			transform: translate(40px, -30px) scale(1.05);
		}
		66% {
			transform: translate(-30px, 40px) scale(0.95);
		}
	}

	/* ===== Navigation ===== */
	.nav {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		z-index: 100;
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 1.5rem 2rem;
		background: linear-gradient(to bottom, rgba(12, 10, 9, 0.8) 0%, transparent 100%);
	}

	.nav-logo {
		width: 36px;
		height: 36px;
		color: var(--copper);
		transition: opacity 0.3s var(--ease-in-out);
	}

	.nav-logo:hover {
		opacity: 0.8;
	}

	.nav-logo svg {
		width: 100%;
		height: 100%;
	}

	.nav-login {
		font-size: 0.9375rem;
		font-weight: 500;
		color: var(--charcoal-300);
		text-decoration: none;
		padding: 0.625rem 1.25rem;
		border: 1px solid rgba(168, 162, 158, 0.2);
		border-radius: 8px;
		transition: all 0.3s var(--ease-in-out);
	}

	.nav-login:hover {
		color: var(--cream-50);
		border-color: rgba(201, 149, 108, 0.4);
		background: rgba(201, 149, 108, 0.1);
	}

	/* ===== Hero Section ===== */
	.hero {
		min-height: 100vh;
		min-height: 100dvh;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		padding: 6rem 2rem 4rem;
		text-align: center;
		position: relative;
	}

	.hero-content {
		max-width: 720px;
		animation: emerge 1.2s var(--ease-out-expo) forwards;
	}

	@keyframes emerge {
		from {
			opacity: 0;
			transform: translateY(40px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	.hero-logo {
		width: 120px;
		height: 120px;
		margin: 0 auto 2rem;
		color: var(--copper);
	}

	.logo-svg {
		width: 100%;
		height: 100%;
	}

	.logo-svg .ring {
		transform-origin: center;
	}

	.logo-svg .ring-1 {
		animation: ring-pulse 5s ease-in-out infinite;
	}
	.logo-svg .ring-2 {
		animation: ring-pulse 5s ease-in-out 0.3s infinite;
	}
	.logo-svg .ring-3 {
		animation: ring-pulse 5s ease-in-out 0.6s infinite;
	}
	.logo-svg .ring-4 {
		animation: ring-pulse 5s ease-in-out 0.9s infinite;
	}
	.logo-svg .ring-5 {
		animation: ring-pulse 5s ease-in-out 1.2s infinite;
	}

	.logo-svg .core {
		animation: core-breathe 5s ease-in-out infinite;
	}

	@keyframes ring-pulse {
		0%,
		100% {
			transform: scale(1);
			opacity: 0.15;
		}
		50% {
			transform: scale(1.03);
			opacity: 0.35;
		}
	}

	@keyframes core-breathe {
		0%,
		100% {
			transform: scale(1);
		}
		50% {
			transform: scale(0.9);
		}
	}

	.hero-eyebrow {
		font-size: 0.875rem;
		font-weight: 500;
		letter-spacing: 0.15em;
		text-transform: uppercase;
		color: var(--copper);
		margin-bottom: 1.5rem;
		animation: emerge 1.2s var(--ease-out-expo) 0.1s backwards;
	}

	.hero-title {
		font-family: var(--font-display);
		font-size: clamp(2.5rem, 8vw, 4.5rem);
		font-weight: 400;
		line-height: 1.1;
		color: var(--cream-50);
		margin-bottom: 1.5rem;
	}

	.title-word {
		display: inline-block;
		animation: word-emerge 1s var(--ease-out-expo) backwards;
	}

	.title-word:nth-child(1) {
		animation-delay: 0.2s;
	}
	.title-word:nth-child(2) {
		animation-delay: 0.3s;
	}
	.title-word:nth-child(3) {
		animation-delay: 0.4s;
	}

	@keyframes word-emerge {
		from {
			opacity: 0;
			transform: translateY(30px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	.title-accent {
		color: var(--copper);
		font-style: italic;
	}

	.hero-subtitle {
		font-size: 1.25rem;
		line-height: 1.7;
		color: var(--charcoal-300);
		max-width: 560px;
		margin: 0 auto 2.5rem;
		font-weight: 300;
		animation: emerge 1.2s var(--ease-out-expo) 0.5s backwards;
	}

	.hero-cta {
		display: inline-flex;
		align-items: center;
		gap: 0.75rem;
		padding: 1rem 2rem;
		font-size: 1.0625rem;
		font-weight: 500;
		color: var(--charcoal-950);
		background: linear-gradient(135deg, var(--amber-500) 0%, var(--copper) 100%);
		border-radius: 12px;
		text-decoration: none;
		transition: all 0.4s var(--ease-out-expo);
		box-shadow: 0 4px 20px -4px rgba(245, 158, 11, 0.4);
		animation: emerge 1.2s var(--ease-out-expo) 0.6s backwards;
	}

	.hero-cta:hover {
		transform: translateY(-3px);
		box-shadow: 0 8px 30px -4px rgba(245, 158, 11, 0.5);
	}

	.cta-arrow {
		width: 20px;
		height: 20px;
		animation: bounce-arrow 2s ease-in-out infinite;
	}

	@keyframes bounce-arrow {
		0%,
		100% {
			transform: translateY(0);
		}
		50% {
			transform: translateY(4px);
		}
	}

	.scroll-hint {
		position: absolute;
		bottom: 2rem;
		left: 50%;
		transform: translateX(-50%);
		animation: emerge 1.2s var(--ease-out-expo) 1s backwards;
	}

	.scroll-line {
		display: block;
		width: 1px;
		height: 60px;
		background: linear-gradient(to bottom, var(--copper), transparent);
		animation: scroll-fade 2s ease-in-out infinite;
	}

	@keyframes scroll-fade {
		0%,
		100% {
			opacity: 0.3;
			transform: scaleY(1);
		}
		50% {
			opacity: 0.8;
			transform: scaleY(1.1);
		}
	}

	/* ===== Sections ===== */
	.section {
		padding: 8rem 2rem;
		opacity: 0;
		transform: translateY(60px);
		transition:
			opacity 0.8s var(--ease-out-expo),
			transform 0.8s var(--ease-out-expo);
	}

	.section.visible {
		opacity: 1;
		transform: translateY(0);
	}

	.section-inner {
		max-width: 1100px;
		margin: 0 auto;
	}

	.section-title {
		font-family: var(--font-display);
		font-size: clamp(1.75rem, 4vw, 2.5rem);
		font-weight: 400;
		color: var(--cream-50);
		text-align: center;
		margin-bottom: 1rem;
	}

	.section-subtitle {
		font-size: 1.125rem;
		line-height: 1.7;
		color: var(--charcoal-400);
		text-align: center;
		max-width: 600px;
		margin: 0 auto 4rem;
		font-weight: 300;
	}

	/* ===== How It Works ===== */
	.pipeline {
		display: flex;
		align-items: flex-start;
		justify-content: center;
		gap: 1rem;
		flex-wrap: wrap;
	}

	.pipeline-step {
		flex: 1;
		min-width: 200px;
		max-width: 280px;
		text-align: center;
		padding: 2rem 1.5rem;
		background: linear-gradient(145deg, rgba(41, 37, 36, 0.5) 0%, rgba(28, 25, 23, 0.6) 100%);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 20px;
		transition: all 0.4s var(--ease-out-expo);
	}

	.pipeline-step:hover {
		transform: translateY(-4px);
		border-color: rgba(201, 149, 108, 0.2);
		box-shadow: 0 20px 40px -10px rgba(0, 0, 0, 0.4);
	}

	.step-icon {
		width: 64px;
		height: 64px;
		margin: 0 auto 1.25rem;
		color: var(--copper);
		position: relative;
	}

	.step-icon svg {
		width: 100%;
		height: 100%;
	}

	.step-icon.absorbing .absorb-ring {
		animation: absorb 3s ease-in-out infinite;
	}

	.step-icon.absorbing .absorb-ring:nth-child(1) {
		animation-delay: 0s;
	}
	.step-icon.absorbing .absorb-ring:nth-child(2) {
		animation-delay: 0.2s;
	}
	.step-icon.absorbing .absorb-ring:nth-child(3) {
		animation-delay: 0.4s;
	}

	@keyframes absorb {
		0%,
		100% {
			transform: scale(1);
			opacity: 0.2;
		}
		50% {
			transform: scale(0.95);
			opacity: 0.5;
		}
	}

	.particles {
		position: absolute;
		inset: 0;
	}

	.particle {
		position: absolute;
		width: 6px;
		height: 6px;
		background: var(--charcoal-500);
		border-radius: 50%;
		animation: absorb-particle 3s ease-in-out infinite;
	}

	.p1 {
		top: 0;
		left: 50%;
		animation-delay: 0s;
	}
	.p2 {
		top: 50%;
		right: 0;
		animation-delay: 1s;
	}
	.p3 {
		bottom: 10%;
		left: 10%;
		animation-delay: 2s;
	}

	@keyframes absorb-particle {
		0% {
			opacity: 0.8;
			transform: translate(0, 0) scale(1);
		}
		100% {
			opacity: 0;
			transform: translate(
					calc(50% - var(--particle-x, 0px)),
					calc(50% - var(--particle-y, 0px))
				)
				scale(0);
		}
	}

	.p1 {
		--particle-x: 0px;
		--particle-y: -20px;
	}
	.p2 {
		--particle-x: 20px;
		--particle-y: 0px;
	}
	.p3 {
		--particle-x: -15px;
		--particle-y: 15px;
	}

	.pipeline-connector {
		display: flex;
		align-items: center;
		padding-top: 3rem;
		color: var(--copper);
	}

	.pipeline-connector svg {
		width: 60px;
		height: 24px;
	}

	.step-title {
		font-family: var(--font-display);
		font-size: 1.25rem;
		font-weight: 400;
		color: var(--cream-50);
		margin-bottom: 0.75rem;
	}

	.step-desc {
		font-size: 0.9375rem;
		line-height: 1.6;
		color: var(--charcoal-400);
		font-weight: 300;
	}

	/* ===== Benefits ===== */
	.benefits-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		gap: 2rem;
	}

	.benefit {
		padding: 2rem;
		background: linear-gradient(145deg, rgba(41, 37, 36, 0.4) 0%, rgba(28, 25, 23, 0.5) 100%);
		border: 1px solid rgba(168, 162, 158, 0.08);
		border-radius: 16px;
		transition: all 0.4s var(--ease-out-expo);
	}

	.benefit:hover {
		border-color: rgba(201, 149, 108, 0.15);
		transform: translateY(-2px);
	}

	.benefit-icon {
		width: 48px;
		height: 48px;
		color: var(--copper);
		margin-bottom: 1.25rem;
	}

	.benefit-icon svg {
		width: 100%;
		height: 100%;
	}

	.benefit-title {
		font-family: var(--font-display);
		font-size: 1.125rem;
		font-weight: 400;
		color: var(--cream-50);
		margin-bottom: 0.625rem;
	}

	.benefit-desc {
		font-size: 0.9375rem;
		line-height: 1.6;
		color: var(--charcoal-400);
		font-weight: 300;
	}

	/* ===== Voices / Testimonials ===== */
	.testimonials {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
		gap: 2rem;
	}

	.testimonial {
		padding: 2rem;
		background: linear-gradient(145deg, rgba(41, 37, 36, 0.5) 0%, rgba(28, 25, 23, 0.6) 100%);
		border: 1px solid rgba(168, 162, 158, 0.1);
		border-radius: 16px;
		transition: all 0.4s var(--ease-out-expo);
	}

	.testimonial:hover {
		border-color: rgba(201, 149, 108, 0.2);
	}

	.testimonial-text {
		font-family: var(--font-display);
		font-size: 1.0625rem;
		font-style: italic;
		line-height: 1.7;
		color: var(--charcoal-300);
		margin-bottom: 1.5rem;
	}

	.testimonial-author {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.author-name {
		font-size: 0.9375rem;
		font-weight: 500;
		color: var(--cream-50);
	}

	.author-handle {
		font-size: 0.8125rem;
		color: var(--charcoal-500);
	}

	/* ===== Early Access CTA ===== */
	.cta-card {
		max-width: 540px;
		margin: 0 auto;
		padding: 3rem;
		background: linear-gradient(145deg, rgba(41, 37, 36, 0.7) 0%, rgba(28, 25, 23, 0.8) 100%);
		border: 1px solid rgba(168, 162, 158, 0.15);
		border-radius: 24px;
		text-align: center;
		box-shadow:
			0 0 0 1px rgba(0, 0, 0, 0.2),
			0 25px 60px -10px rgba(0, 0, 0, 0.5),
			0 0 100px -30px var(--copper-glow);
	}

	.cta-logo {
		width: 72px;
		height: 72px;
		margin: 0 auto 1.5rem;
		color: var(--copper);
	}

	.cta-logo svg {
		width: 100%;
		height: 100%;
	}

	.cta-title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		font-weight: 400;
		color: var(--cream-50);
		margin-bottom: 0.75rem;
	}

	.cta-subtitle {
		font-size: 1rem;
		line-height: 1.7;
		color: var(--charcoal-400);
		margin-bottom: 2rem;
		font-weight: 300;
	}

	.subscribe-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.75rem;
		padding: 1rem 2rem;
		font-size: 1.0625rem;
		font-weight: 500;
		font-family: var(--font-body);
		color: var(--charcoal-950);
		background: linear-gradient(135deg, var(--amber-500) 0%, var(--copper) 100%);
		border-radius: 12px;
		text-decoration: none;
		transition: all 0.4s var(--ease-out-expo);
		box-shadow: 0 4px 20px -4px rgba(245, 158, 11, 0.4);
	}

	.subscribe-btn:hover {
		transform: translateY(-3px);
		box-shadow: 0 8px 30px -4px rgba(245, 158, 11, 0.5);
	}

	.leaflet-icon {
		width: 20px;
		height: 20px;
	}

	.external-icon {
		width: 16px;
		height: 16px;
		opacity: 0.7;
	}

	.subscribe-hint {
		font-size: 0.875rem;
		color: var(--charcoal-500);
		margin-top: 1.25rem;
		font-weight: 300;
	}

	/* ===== Footer ===== */
	.site-footer {
		padding: 3rem 2rem;
		border-top: 1px solid rgba(168, 162, 158, 0.1);
	}

	.footer-inner {
		max-width: 1100px;
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1.5rem;
	}

	.footer-brand {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		color: var(--copper);
	}

	.footer-logo {
		width: 32px;
		height: 32px;
	}

	.footer-name {
		font-family: var(--font-display);
		font-size: 1.125rem;
		color: var(--cream-50);
	}

	.footer-nav {
		display: flex;
		gap: 2rem;
	}

	.footer-nav a {
		font-size: 0.875rem;
		color: var(--charcoal-400);
		text-decoration: none;
		transition: color 0.3s var(--ease-in-out);
	}

	.footer-nav a:hover {
		color: var(--cream-50);
	}

	.footer-copy {
		font-size: 0.8125rem;
		color: var(--charcoal-600);
	}

	/* ===== Responsive ===== */
	@media (max-width: 768px) {
		.nav {
			padding: 1rem 1.5rem;
		}

		.hero {
			padding: 5rem 1.5rem 3rem;
		}

		.hero-logo {
			width: 90px;
			height: 90px;
		}

		.hero-subtitle {
			font-size: 1.0625rem;
		}

		.section {
			padding: 5rem 1.5rem;
		}

		.pipeline {
			flex-direction: column;
			align-items: center;
		}

		.pipeline-connector {
			transform: rotate(90deg);
			padding: 1rem 0;
		}

		.pipeline-step {
			max-width: 100%;
		}

		.cta-card {
			padding: 2rem 1.5rem;
		}

		.footer-nav {
			gap: 1.5rem;
		}
	}

	@media (max-width: 480px) {
		.hero-cta,
		.subscribe-btn {
			width: 100%;
			justify-content: center;
		}
	}

	/* ===== Reduced Motion ===== */
	@media (prefers-reduced-motion: reduce) {
		.orb,
		.hero-content,
		.section,
		.logo-svg .ring,
		.logo-svg .core,
		.absorb-ring,
		.particle,
		.scroll-line,
		.cta-arrow,
		.title-word {
			animation: none;
		}

		.section {
			opacity: 1;
			transform: none;
		}

		.pipeline-step,
		.benefit,
		.testimonial,
		.hero-cta,
		.nav-login,
		.subscribe-btn {
			transition: none;
		}
	}
	/* #voices hidden until real testimonials are collected */
	#voices {
		display: none;
	}
</style>
