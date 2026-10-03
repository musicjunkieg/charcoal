<script lang="ts">
	// The scale tokens (--text-*, --space-*, --radius-*) live here. This page
	// carries its own palette on .page, so without this import every size and
	// gap below falls back to the browser default (#292/#293/#380).
	import '$lib/website/styles/tokens.css';

	const LEAFLET_URL = 'https://charcoal.leaflet.pub';

	// The direction contract (#395). Emitted as a real HTML comment through
	// {@html} because Svelte strips template comments from the production build,
	// and a contract the build erased is one nobody can audit.
	const CONTRACT = `<!--
THESIS: Joy practised under pressure, in the light of a crowded afternoon: coloured shade turning to sun. Refuses the dark security console, the soft wellness page, and any borrowed cultural motif.
OWN-WORLD: The tones of Bryan's reference photograph, kept as colour rather than darkness. Rust, umber shade, sandstone sun and placard card as whole fields; faded pink and dusty blue as the only cool notes; film grain over everything. Archivo set heavy and narrow for headlines, Atkinson Hyperlegible for reading.
STORY: Posting can feel good again; the people who came only to upset you are gone first; you decide; sign in.
FIRST VIEWPORT: Rust field lit by a copper sun from the top right. A two-line headline across most of the width, the lead and the sign-in button beneath.
FORM: Pinned by the user from a reference photograph, after the first build was rejected as too literal (rolls 346abf3d and 798d6278 rejected earlier).
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md
-->`;

	// Who decides. Automated actions are a launch gate (PRODUCT.md, 2026-10-02)
	// but are not built yet, and no surface may promise them until they work.
	// Both wordings live here so that the day automation ships, the change is
	// this one constant. web/src/lib/landing.test.ts holds it at 'manual'.
	type ActionsMode = 'manual' | 'automatic';
	const ACTIONS_MODE: ActionsMode = 'manual';

	const AUTHORITY: Record<ActionsMode, { title: string; points: string[] }> = {
		manual: {
			title: 'You hold the authority.',
			points: [
				'Every account comes with its evidence: the posts, the pattern, and how they found you.',
				'Mute or block one account, or everyone at a risk level at once.',
				'Nothing happens unless you say so, and anything you do can be undone.'
			]
		},
		automatic: {
			title: 'You set the rule. Charcoal keeps it.',
			points: [
				'Every account comes with its evidence: the posts, the pattern, and how they found you.',
				'Choose the risk level you want handled, and Charcoal mutes or blocks those accounts automatically, before they reach you.',
				'Every action can be reviewed and undone.'
			]
		}
	};
	const authority = AUTHORITY[ACTIONS_MODE];
</script>

<svelte:head>
	<title>Charcoal · Hard things, yes. Bullshit, no.</title>
	<meta
		name="description"
		content="Charcoal finds the people who show up only to upset you before they reach you, so posting on Bluesky can be joyful again."
	/>
	<link rel="preconnect" href="https://fonts.googleapis.com" />
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
	<link
		href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,100..900&family=Atkinson+Hyperlegible+Next:ital,wght@0,400;0,600;0,700;1,400&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

{#snippet mark(cls: string)}
	<svg viewBox="0 0 64 64" fill="none" class="mark {cls}" aria-hidden="true" focusable="false">
		<circle class="ring ring-1" cx="32" cy="32" r="30" stroke="currentColor" stroke-width="1.5" opacity="0.25" />
		<circle class="ring ring-2" cx="32" cy="32" r="25" stroke="currentColor" stroke-width="1.5" opacity="0.4" />
		<circle class="ring ring-3" cx="32" cy="32" r="20" stroke="currentColor" stroke-width="2" opacity="0.55" />
		<circle class="ring ring-4" cx="32" cy="32" r="15" stroke="currentColor" stroke-width="2" opacity="0.75" />
		<circle class="ring ring-5" cx="32" cy="32" r="10" stroke="currentColor" stroke-width="2.5" opacity="0.9" />
		<circle class="core" cx="32" cy="32" r="5" fill="currentColor" />
	</svg>
{/snippet}

<div class="page">
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- a constant comment, no user input -->
	{@html CONTRACT}
	<div class="grain" aria-hidden="true"></div>

	<nav class="nav" aria-label="Main">
		<a href="/" class="brand" aria-label="Charcoal home">
			{@render mark('mark-nav')}
			<span class="wordmark">Charcoal</span>
		</a>
		<a href="/login" class="nav-signin">Sign in</a>
	</nav>

	<main>
		<section class="hero">
			<div class="hero-mark">{@render mark('mark-hero')}</div>
			<div class="inner">
				<h1>
					<span>Hard things, <em>yes.</em></span>
					<span>Bullshit, no.</span>
				</h1>
				<div class="hero-foot">
					<p class="lead">
						Charcoal finds the people who show up only to upset you, before they reach you. You
						take them out of your timeline in one move. What is left is the Bluesky you came for.
					</p>
					<div>
						<a href="/login" class="cta">
							<span>Sign in with Bluesky</span>
							<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
								<path d="M4 12h15M13 5l7 7-7 7" />
							</svg>
						</a>
						<p class="note">
							No password. We are letting people in a few at a time, so signing in saves your place.
						</p>
					</div>
				</div>
			</div>
		</section>

		<section class="turn" aria-label="Before and after">
			<div class="shade">
				<div class="panel">
					<h2>You know this feeling.</h2>
					<p>
						You write the post, then read it the way someone who wants to hate it would. You sand
						off a line. You post it anyway and spend the afternoon waiting to see who shows up.
					</p>
					<p>
						Not the people who disagree with you. Those you can handle. The ones arguing in bad
						faith, finding the worst possible reading every time, coming for you instead of what you
						said.
					</p>
				</div>
			</div>
			<div class="sun">
				<div class="panel">
					<h2>Now picture wanting to see who replied.</h2>
					<p>
						You still say the hard thing, and people still push back. The ones with a track record
						of making it miserable were gone before you hit Post.
					</p>
					<p class="claim">That is not comfort. It is joy.</p>
					<p class="small">
						Charcoal does not take the pressure away. It takes away the people who came only to make
						it worse.
					</p>
				</div>
			</div>
		</section>

		<section class="how">
			<div class="inner">
				<h2>How it works.</h2>
				<dl class="claims">
					<div>
						<dt>It sees them coming.</dt>
						<dd>It scores accounts that have never spoken to you, before a blocklist would hear of them.</dd>
					</div>
					<div>
						<dt>It watches the door they use.</dt>
						<dd>
							Quote-posts and reposts carry your words to people who never chose to see them. That
							audience is where it looks; your followers chose to be here.
						</dd>
					</div>
					<div>
						<dt>It needs two things, not one.</dt>
						<dd>
							Swearing is not enough, and neither is posting about what you post about. It flags the
							overlap: your spaces plus a pattern of hostility.
						</dd>
					</div>
					<div>
						<dt>It learns your ground from you.</dt>
						<dd>It reads your own posts to work out what you talk about. No checklist to fill in.</dd>
					</div>
				</dl>
			</div>
		</section>

		<section class="close">
			<div class="inner close-grid">
				<div>
					<h2>{authority.title}</h2>
					<ul class="points">
						{#each authority.points as point (point)}
							<li>{point}</li>
						{/each}
					</ul>
					<p class="small">
						It only ever acts on your own timeline. It never publishes a list, a label or a reason
						about anyone.
					</p>
				</div>
				<div class="come-in">
					<p class="come-in-title">Come in.</p>
					<a href="/login" class="cta">
						<span>Sign in with Bluesky</span>
						<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
							<path d="M4 12h15M13 5l7 7-7 7" />
						</svg>
					</a>
					<ul class="expect">
						<li><strong>No password.</strong> You sign in with your Bluesky account.</li>
						<li><strong>A waitlist, for now.</strong> Signing in saves your place.</li>
						<li>
							<strong>A long first scan.</strong> A recent one took about 22 minutes for 595 accounts.
							You can close the tab and come back.
						</li>
						<li>
							<strong>Sometimes, no verdict.</strong> When there is not enough to go on, it says so
							instead of guessing.
						</li>
					</ul>
				</div>
			</div>
		</section>
	</main>

	<footer class="site-footer">
		<div class="inner footer-row">
			<span class="brand">
				{@render mark('mark-nav')}
				<span class="wordmark">Charcoal</span>
			</span>
			<nav class="footer-nav" aria-label="Footer">
				<a href="/login">Sign in</a>
				<a href={LEAFLET_URL} target="_blank" rel="noopener noreferrer">Follow the work on Leaflet</a>
			</nav>
			<p class="footer-copy">&copy; 2026 Charcoal</p>
		</div>
	</footer>
</div>

<style>
	.page {
		/* Read from Bryan's reference photograph (#395) and kept as COLOUR: the
		   darkest field is umber, never black. Scoped to this page because the
		   signed-in app still wears the earlier look. */
		--rust: #713418;
		--umber: #3b2217;
		--copper: #b65624;
		--sandstone: #b37832;
		--placard: #d7b999;
		--headband: #eae6e5;
		--pink: #e7a1b4;
		/* The sunlit edge of the sandstone in the reference, pushed brighter so the
		   ring mark reads as gold on rust (4.9:1). Used for the mark only. */
		--gold: #e8b04f;
		--blue: #4d7c92;
		--blue-deep: #2c4f60;
		--ink: #24140d;

		--font-display: 'Archivo', 'Arial Narrow', sans-serif;
		--font-text: 'Atkinson Hyperlegible Next', system-ui, sans-serif;
		--ease-out: cubic-bezier(0.16, 1, 0.3, 1);

		position: relative;
		min-height: 100dvh;
		overflow-x: clip;
		background: var(--rust);
		color: var(--headband);
		font-family: var(--font-text);
		font-size: var(--text-subtitle);
		line-height: 1.55;
	}

	/* The landing has no layout of its own to reset the page margin. */
	:global(body) {
		margin: 0;
	}

	.page :global(::selection) {
		background: var(--pink);
		color: var(--ink);
	}

	/* Film grain over the whole page: the texture of the reference, not a
	   motif. Static, so it costs nothing after first paint. */
	.grain {
		position: fixed;
		inset: 0;
		z-index: 5;
		pointer-events: none;
		opacity: 0.45;
		mix-blend-mode: soft-light;
		background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='180' height='180'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='.85' numOctaves='2' stitchTiles='stitch'/%3E%3CfeColorMatrix values='0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 .6 0'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
	}

	.page a {
		color: inherit;
		text-underline-offset: 0.2em;
		text-decoration-thickness: 2px;
	}

	.page a:focus-visible {
		outline: 3px solid var(--pink);
		outline-offset: 4px;
		border-radius: var(--radius-3);
	}

	h1,
	h2,
	p,
	ul,
	dl,
	dd {
		margin: 0;
	}

	ul {
		padding: 0;
		list-style: none;
	}

	.inner {
		max-width: 1160px;
		margin: 0 auto;
	}

	h2 {
		margin-bottom: var(--space-24);
		font-family: var(--font-display);
		font-size: var(--text-shout);
		font-weight: 800;
		font-stretch: 75%;
		line-height: 0.98;
		letter-spacing: -0.01em;
		text-wrap: balance;
	}

	.small {
		max-width: 46ch;
		font-size: var(--text-body);
	}

	/* ---------- Navigation ---------- */
	.nav {
		position: absolute;
		inset: 0 0 auto 0;
		z-index: 2;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--space-24) var(--space-32);
	}

	.brand {
		display: inline-flex;
		align-items: center;
		gap: var(--space-12);
		text-decoration: none;
	}

	.mark-nav {
		width: 56px;
		height: 56px;
		color: var(--gold);
	}

	.wordmark {
		font-family: var(--font-display);
		font-size: var(--text-display-sm);
		font-weight: 800;
		font-stretch: 75%;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		text-decoration: none;
	}

	.nav-signin {
		padding: var(--space-10) var(--space-20);
		border: 2px solid var(--headband);
		border-radius: var(--radius-pill);
		font-weight: 700;
		text-decoration: none;
		transition:
			background-color 0.25s var(--ease-out),
			color 0.25s var(--ease-out);
	}

	.nav-signin:hover {
		background: var(--headband);
		color: var(--rust);
	}

	/* ---------- Hero: rust, lit by a copper sun ---------- */
	.hero {
		display: flex;
		align-items: center;
		min-height: 100dvh;
		box-sizing: border-box;
		padding: var(--space-128) var(--space-32) var(--space-80);
		background:
			radial-gradient(ellipse 70% 80% at 92% 0%, rgb(182 86 36 / 0.95), transparent 70%),
			radial-gradient(ellipse 75% 75% at 0% 100%, rgb(42 22 14 / 0.95), transparent 72%),
			var(--rust);
	}

	.hero {
		position: relative;
	}

	.hero .inner {
		position: relative;
		width: 100%;
	}

	.hero-mark {
		position: absolute;
		top: 14%;
		right: 7%;
		width: min(30vw, 380px);
		color: var(--gold);
	}

	.mark-hero {
		display: block;
		width: 100%;
		height: auto;
		overflow: visible;
	}

	.mark .ring,
	.mark .core {
		transform-box: fill-box;
		transform-origin: center;
	}

	.mark-hero .ring {
		animation: ring-swell 3.2s cubic-bezier(0.34, 1.56, 0.64, 1) infinite;
	}

	.mark-hero .ring-2 {
		animation-delay: 0.12s;
	}

	.mark-hero .ring-3 {
		animation-delay: 0.24s;
	}

	.mark-hero .ring-4 {
		animation-delay: 0.36s;
	}

	.mark-hero .ring-5 {
		animation-delay: 0.48s;
	}

	.mark-hero .core {
		animation: core-bounce 3.2s cubic-bezier(0.34, 1.56, 0.64, 1) infinite;
	}

	@keyframes ring-swell {
		0%,
		60%,
		100% {
			transform: scale(1);
		}
		30% {
			transform: scale(1.07);
		}
	}

	@keyframes core-bounce {
		0%,
		60%,
		100% {
			transform: scale(1);
		}
		22% {
			transform: scale(0.72);
		}
		40% {
			transform: scale(1.12);
		}
	}

	h1 {
		font-family: var(--font-display);
		font-size: var(--text-poster);
		font-weight: 900;
		font-stretch: 70%;
		line-height: 0.92;
		letter-spacing: -0.015em;
		text-transform: uppercase;
	}

	h1 span {
		display: block;
	}

	h1 em {
		font-style: normal;
		color: var(--pink);
	}

	.hero-foot {
		display: grid;
		grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
		gap: var(--space-48);
		align-items: end;
		/* Room under the headline so the ring mark has space to move and the
		   sign-in row sits clear of it. */
		margin-top: var(--space-128);
	}

	.lead {
		max-width: 38ch;
		font-size: var(--text-title);
		line-height: 1.45;
	}

	.cta {
		box-sizing: border-box;
		display: inline-flex;
		align-items: center;
		gap: var(--space-14);
		padding: var(--space-20) var(--space-32);
		border-radius: var(--radius-pill);
		background: var(--pink);
		font-size: var(--text-title);
		font-weight: 700;
		text-decoration: none;
		transition:
			background-color 0.3s var(--ease-out),
			transform 0.3s var(--ease-out);
	}

	.page a.cta {
		color: var(--ink);
	}

	.cta svg {
		width: 1.4em;
		height: 1.4em;
		fill: none;
		stroke: currentColor;
		stroke-width: 2.5;
		stroke-linecap: round;
		stroke-linejoin: round;
		transition: transform 0.3s var(--ease-out);
	}

	.cta:hover {
		background: var(--headband);
		transform: translateY(-2px);
	}

	.cta:hover svg {
		transform: translateX(4px);
	}

	.cta:active {
		transform: translateY(0);
	}

	.page a.cta:focus-visible {
		outline-color: var(--headband);
	}

	.note {
		max-width: 40ch;
		margin-top: var(--space-16);
		color: var(--placard);
		font-size: var(--text-body);
	}

	/* ---------- The turn: shade, then sun ---------- */
	.turn {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}

	.shade,
	.sun {
		display: flex;
		padding: var(--space-96) var(--space-48);
	}

	.panel {
		max-width: 34rem;
	}

	.shade {
		justify-content: flex-end;
		background: var(--umber);
		color: var(--placard);
	}

	.shade h2 {
		color: var(--headband);
	}

	.sun {
		background:
			radial-gradient(ellipse 80% 70% at 100% 0%, rgb(215 185 153 / 0.45), transparent 70%),
			radial-gradient(ellipse 90% 60% at 0% 100%, rgb(113 52 24 / 0.45), transparent 70%),
			var(--sandstone);
		color: var(--ink);
	}

	.shade p + p,
	.sun p + p {
		margin-top: var(--space-20);
	}

	.claim {
		font-family: var(--font-display);
		font-size: var(--text-shout);
		font-weight: 900;
		font-stretch: 70%;
		line-height: 0.98;
		text-transform: uppercase;
		text-wrap: balance;
	}

	.sun p.claim {
		margin-top: var(--space-40);
	}

	/* ---------- How it works: placard card ---------- */
	.how {
		padding: var(--space-96) var(--space-32);
		background: var(--placard);
		color: var(--ink);
	}

	.how h2 {
		color: var(--rust);
	}

	.claims {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--space-40) var(--space-64);
	}

	.claims > div {
		padding-top: var(--space-16);
		border-top: 3px solid var(--blue-deep);
	}

	.claims dt {
		margin-bottom: var(--space-6);
		font-family: var(--font-display);
		font-size: var(--text-section-title);
		font-weight: 800;
		font-stretch: 80%;
		line-height: 1.15;
		color: var(--blue-deep);
		text-wrap: balance;
	}

	.claims dd {
		max-width: 46ch;
	}

	/* ---------- Close: authority and the door ---------- */
	.close {
		padding: var(--space-96) var(--space-32);
		background:
			radial-gradient(ellipse 60% 70% at 100% 100%, rgb(182 86 36 / 0.8), transparent 70%),
			radial-gradient(ellipse 70% 80% at 0% 0%, rgb(42 22 14 / 0.85), transparent 70%),
			var(--rust);
	}

	.close-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		gap: var(--space-64);
	}

	.points {
		display: grid;
		gap: var(--space-12);
		margin-bottom: var(--space-24);
	}

	.points li {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--space-14);
		max-width: 44ch;
	}

	.points li::before {
		content: '';
		width: 0.55em;
		height: 0.55em;
		translate: 0 0.45em;
		border-radius: var(--radius-circle);
		background: var(--pink);
	}

	.close .small {
		color: var(--placard);
	}

	.come-in-title {
		margin-bottom: var(--space-24);
		font-family: var(--font-display);
		font-size: var(--text-poster);
		font-weight: 900;
		font-stretch: 70%;
		line-height: 0.92;
		text-transform: uppercase;
	}

	.expect {
		display: grid;
		gap: var(--space-8);
		margin-top: var(--space-32);
		color: var(--placard);
		font-size: var(--text-body);
	}

	.expect strong {
		color: var(--headband);
	}

	/* ---------- Footer ---------- */
	.site-footer {
		padding: var(--space-32);
		background: var(--umber);
		color: var(--placard);
	}

	.footer-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-16) var(--space-40);
	}

	.site-footer .wordmark {
		color: var(--headband);
	}

	.footer-nav {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-12) var(--space-28);
		font-size: var(--text-body);
	}

	.footer-copy {
		margin-left: auto;
		font-size: var(--text-small);
	}

	/* ---------- Narrow screens ---------- */
	@media (max-width: 860px) {
		.nav {
			padding: var(--space-16) var(--space-20);
		}

		.hero {
			flex-direction: column;
			align-items: stretch;
			padding: var(--space-96) var(--space-20) var(--space-64);
		}

		.hero-mark {
			position: static;
			width: 120px;
			margin-bottom: var(--space-32);
		}

		.mark-nav {
			width: 40px;
			height: 40px;
		}

		.wordmark {
			font-size: var(--text-page-title);
		}

		.hero-foot,
		.turn,
		.claims,
		.close-grid {
			grid-template-columns: minmax(0, 1fr);
		}

		.hero-foot {
			gap: var(--space-32);
		}

		.shade,
		.sun {
			justify-content: flex-start;
			padding: var(--space-64) var(--space-20);
		}

		.how,
		.close {
			padding: var(--space-64) var(--space-20);
		}

		.close-grid {
			gap: var(--space-48);
		}

		.site-footer {
			padding: var(--space-28) var(--space-20);
		}

		.footer-copy {
			margin-left: 0;
		}
	}

	@media (max-width: 480px) {
		.cta {
			justify-content: center;
			width: 100%;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.mark-hero .ring,
		.mark-hero .core {
			animation: none;
		}

		.cta,
		.cta svg,
		.nav-signin {
			transition: none;
		}
	}
</style>
