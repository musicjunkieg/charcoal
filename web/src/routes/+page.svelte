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
THESIS: You are inside a thriving place whose border you control, and the page is seen from inside it. Refuses the dark shield-and-radar security hero and the soft wellness page.
OWN-WORLD: Daylight Afrofuturism. Whole fields of sun gold, royal purple, laterite red, leaf green and aubergine ink. Concentric rings built from dashed strokes (bead, block and checker rows), triangle-and-diamond band dividers, Tac One poster capitals, Ojuju headings, Atkinson Hyperlegible text.
STORY: Posting can be joyful again; the people who came to upset you are stopped at the edge and you decide; sign in.
FIRST VIEWPORT: Gold field. Four-line poster headline top left, sign-in block under it, a quarter dome of patterned rings rising from the bottom-right corner with posts breaking apart at its outer band.
FORM: Pinned by the user after two rejected rolls, 346abf3d and 798d6278.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md
-->`;

	// Who decides. Automated actions are a launch gate (PRODUCT.md, 2026-10-02)
	// but are not built yet, and no surface may promise them until they work.
	// Both wordings live here so that the day automation ships, the change is
	// this one constant. web/src/lib/landing.test.ts holds it at 'manual'.
	type ActionsMode = 'manual' | 'automatic';
	const ACTIONS_MODE: ActionsMode = 'manual';

	const EVIDENCE_POINT = {
		lead: 'You see who, and you see why.',
		body: 'Every account comes with its evidence: the posts, the pattern, and how they found you. Never act on a label alone.'
	};

	const AUTHORITY: Record<
		ActionsMode,
		{ title: string; points: { lead: string; body: string }[] }
	> = {
		manual: {
			title: 'You hold the authority.',
			points: [
				EVIDENCE_POINT,
				{
					lead: 'One move.',
					body: 'Mute or block one account, or everyone at a risk level at once, without leaving the page you are on.'
				},
				{
					lead: 'Nothing happens unless you say so.',
					body: 'Charcoal recommends. You decide. Change your mind and undo it.'
				}
			]
		},
		automatic: {
			title: 'You set the rule. Charcoal keeps it.',
			points: [
				EVIDENCE_POINT,
				{
					lead: 'It acts while you are busy living.',
					body: 'Choose the risk level you want handled, and Charcoal mutes or blocks those accounts automatically, before they reach you.'
				},
				{
					lead: 'Every action can be reviewed and undone.',
					body: 'Nothing it does is hidden and nothing is permanent. Read what it did, and reverse any of it.'
				}
			]
		}
	};
	const authority = AUTHORITY[ACTIONS_MODE];

	// The border: a quarter dome whose centre is the bottom-right corner of a
	// 600-unit square. Each ring is one stroked circle; the pattern rows are
	// dashed strokes laid over a solid band, so the geometry is original and
	// costs nothing to draw.
	const C = 600;
	type Ring = { r: number; w: number; color: string; dash?: string; offset?: number; round?: boolean };
	const RINGS: Ring[] = [
		{ r: 470, w: 22, color: 'var(--ink)', dash: '3 19', round: true },
		{ r: 428, w: 40, color: 'var(--earth)' },
		{ r: 428, w: 14, color: 'var(--gold)', dash: '22 22' },
		{ r: 372, w: 52, color: 'var(--purple)' },
		{ r: 385, w: 17, color: 'var(--bone)', dash: '17 17' },
		{ r: 359, w: 17, color: 'var(--bone)', dash: '17 17', offset: 17 },
		{ r: 318, w: 28, color: 'var(--leaf)' },
		{ r: 318, w: 7, color: 'var(--gold)', dash: '1 15', round: true },
		{ r: 280, w: 28, color: 'var(--ink)' },
		{ r: 280, w: 12, color: 'var(--earth)', dash: '30 10' }
	];

	// A post travelling toward the centre stops `stopR` units short of it.
	function toward(x: number, y: number, stopR: number) {
		const d = Math.hypot(C - x, C - y);
		const k = (d - stopR) / d;
		return { x, y, dx: Math.round((C - x) * k), dy: Math.round((C - y) * k) };
	}
	// Turned away at the outer band (radius 481 plus half a post).
	const TURNED_AWAY = [
		{ ...toward(30, 110, 540), delay: 0, tilt: -7 },
		{ ...toward(160, 24, 540), delay: 3, tilt: 5 },
		{ ...toward(4, 250, 540), delay: 6, tilt: -3 }
	];
	// Let through, all the way inside.
	const LET_IN = [
		{ ...toward(260, 14, 95), delay: 1.5, tilt: 4 },
		{ ...toward(14, 410, 200), delay: 5, tilt: -5 }
	];
	// Already inside: the timeline that is left.
	const INSIDE = [{ x: 532, y: 388, tilt: 6 }];
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
		href="https://fonts.googleapis.com/css2?family=Atkinson+Hyperlegible+Next:ital,wght@0,400;0,600;0,700;1,400&family=Ojuju:wght@200..800&family=Tac+One&display=swap"
		rel="stylesheet"
	/>
</svelte:head>

{#snippet band(id: string, ground: string, a: string, b: string, c: string)}
	<svg class="band" width="100%" height="48" aria-hidden="true" focusable="false">
		<defs>
			<pattern {id} width="48" height="48" patternUnits="userSpaceOnUse">
				<rect width="48" height="48" style:fill={ground} />
				<path d="M0 28 12 4l12 24ZM24 28 36 4l12 24Z" style:fill={a} />
				<rect y="28" width="48" height="6" style:fill={b} />
				<path d="m12 34 6 7-6 7-6-7ZM36 34l6 7-6 7-6-7Z" style:fill={c} />
			</pattern>
		</defs>
		<rect width="100%" height="48" fill="url(#{id})" />
	</svg>
{/snippet}

{#snippet post(kind: 'hostile' | 'friend' | 'own')}
	{#if kind === 'own'}
		<rect class="post-body" x="-66" y="-32" width="132" height="64" rx="8" />
		<rect class="post-line" x="-50" y="-16" width="100" height="7" rx="3.5" />
		<rect class="post-line" x="-50" y="-2" width="84" height="7" rx="3.5" />
		<rect class="post-line" x="-50" y="12" width="58" height="7" rx="3.5" />
	{:else}
		<rect class="post-body" x="-48" y="-22" width="96" height="44" rx="8" />
		<rect class="post-line" x="-34" y="-9" width="68" height="6" rx="3" />
		<rect class="post-line" x="-34" y="4" width={kind === 'hostile' ? 30 : 46} height="6" rx="3" />
	{/if}
{/snippet}

<div class="page">
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- a constant comment, no user input -->
	{@html CONTRACT}

	<nav class="nav" aria-label="Main">
		<a href="/" class="wordmark" aria-label="Charcoal home">Charcoal</a>
		<a href="/login" class="nav-signin">Sign in</a>
	</nav>

	<main>
		<section class="hero">
			<div class="hero-inner">
				<div class="hero-text">
					<h1>
						<span>Hard things,</span>
						<span class="yes">yes.</span>
						<span>Bullshit,</span>
						<span class="no">no.</span>
					</h1>
					<p class="hero-lead">
						Charcoal finds the people who show up only to upset you, before they reach you. You
						take them out of your timeline in one move. What is left is the Bluesky you came for.
					</p>
					<a href="/login" class="cta">
						<span>Sign in with Bluesky</span>
						<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
							<path d="M4 12h15M13 5l7 7-7 7" />
						</svg>
					</a>
					<p class="hero-note">
						No password. We are letting people in a few at a time, so signing in saves your place.
					</p>
				</div>

				<figure class="border-figure">
					<svg
						viewBox="0 0 600 600"
						role="img"
						aria-label="Posts moving toward a patterned border. Some pass through to the inside. Others break apart at the edge."
					>
						{#each RINGS as ring, i (i)}
							<circle
								cx={C}
								cy={C}
								r={ring.r}
								fill="none"
								stroke-width={ring.w}
								stroke-dasharray={ring.dash}
								stroke-dashoffset={ring.offset}
								stroke-linecap={ring.round ? 'round' : undefined}
								style:stroke={ring.color}
							/>
						{/each}
						<circle cx={C} cy={C} r="266" class="inside" />

						{#each INSIDE as p, i (i)}
							<g transform="translate({p.x} {p.y}) rotate({p.tilt})" class="friend">
								{@render post('friend')}
							</g>
						{/each}
						<g transform="translate(452 452) rotate(-3)" class="own">
							{@render post('own')}
						</g>

						{#each LET_IN as p, i (i)}
							<g transform="translate({p.x} {p.y})">
								<g
									class="travel let-in"
									style:--dx="{p.dx}px"
									style:--dy="{p.dy}px"
									style:--delay="{p.delay}s"
								>
									<g transform="rotate({p.tilt})" class="friend">{@render post('friend')}</g>
								</g>
							</g>
						{/each}

						{#each TURNED_AWAY as p, i (i)}
							<g transform="translate({p.x} {p.y})">
								<g
									class="travel turned-away"
									style:--dx="{p.dx}px"
									style:--dy="{p.dy}px"
									style:--delay="{p.delay}s"
								>
									<g transform="rotate({p.tilt})" class="hostile">{@render post('hostile')}</g>
								</g>
							</g>
						{/each}
					</svg>
					<figcaption>An illustration, not real posts.</figcaption>
				</figure>
			</div>
		</section>

		{@render band('band-now', 'var(--ink)', 'var(--gold)', 'var(--earth)', 'var(--purple-lit)')}

		<section id="now" class="field now">
			<div class="inner">
				<h2>You know this feeling.</h2>
				<div class="now-grid">
				<p class="voice">
					You write the post. Then you read it again the way someone who wants to hate it would. You
					sand off a line. You post it anyway, and spend the afternoon waiting to see who shows up.
				</p>
				<div>
				<p class="voice quiet">
					Not the people who disagree with you. Those you can handle. The other ones:
				</p>
				<ul class="the-ones">
					<li>The ones arguing in bad faith.</li>
					<li>The ones who find the worst possible reading, every time.</li>
					<li>The ones who come for you, not for what you said.</li>
				</ul>
				</div>
				</div>
			</div>
		</section>

		{@render band('band-could', 'var(--purple)', 'var(--gold)', 'var(--ink)', 'var(--bone)')}

		<section id="could" class="field could">
			<div class="inner">
				<h2>Now picture wanting to see who replied.</h2>
				<p class="voice">
					You still say the hard thing. People still push back, and sometimes they are right. The
					difference is that the ones with a track record of making it miserable were gone before
					you hit Post.
				</p>
				<p class="claim"><span>That is not comfort.</span> <span>It is joy.</span></p>
				<p class="body">
					The kind you keep while you organise, argue, grieve and fight in public. Charcoal does not
					take the pressure away. It takes away the people who came only to make it worse.
				</p>
			</div>
		</section>

		{@render band('band-how', 'var(--earth)', 'var(--bone)', 'var(--ink)', 'var(--gold)')}

		<section id="how" class="field how">
			<div class="inner">
				<h2>How the border works.</h2>
				<p class="body intro">
					A blocklist acts after someone has already got to you. Charcoal works earlier, in four
					ways a list cannot.
				</p>
				<dl class="claims">
					<div>
						<dt>It sees them coming.</dt>
						<dd>
							Charcoal scores accounts that have never spoken to you. By the time a blocklist would
							hear about them, you have already decided what to do.
						</dd>
					</div>
					<div>
						<dt>It watches the door they use.</dt>
						<dd>
							Trouble rarely starts with your followers; they chose to be here. It starts when a
							quote-post or repost carries your words to people who did not. That audience is the
							one Charcoal looks at.
						</dd>
					</div>
					<div>
						<dt>It needs two things, not one.</dt>
						<dd>
							Someone who swears a lot may be your ally. Someone who posts about what you post about
							is probably your community. Charcoal flags only the overlap: in your spaces, with a
							pattern of hostility.
						</dd>
					</div>
					<div>
						<dt>It learns your ground from you.</dt>
						<dd>
							You never fill in a list of topics. Charcoal reads your own posts and works out what
							you talk about, including the things you would not think to name.
						</dd>
					</div>
				</dl>
			</div>
		</section>

		{@render band('band-authority', 'var(--gold)', 'var(--purple)', 'var(--ink)', 'var(--earth)')}

		<section id="authority" class="field authority">
			<div class="inner split">
				<div>
					<h2>{authority.title}</h2>
					<p class="body">
						Charcoal only ever acts on your own timeline. It never publishes a list, a label or a
						reason about anyone.
					</p>
				</div>
				<ul class="points">
					{#each authority.points as point (point.lead)}
						<li>
							<h3>{point.lead}</h3>
							<p class="body">{point.body}</p>
						</li>
					{/each}
				</ul>
			</div>
		</section>

		{@render band('band-expect', 'var(--leaf)', 'var(--gold)', 'var(--ink)', 'var(--bone)')}

		<section id="expect" class="field expect">
			<div class="inner">
				<h2>What to expect.</h2>
				<ul class="facts">
					<li>
						<h3>No password.</h3>
						<p class="body">You sign in with your Bluesky account. Charcoal never sees your password.</p>
					</li>
					<li>
						<h3>A waitlist, for now.</h3>
						<p class="body">We are letting people in a few at a time. Signing in saves your place.</p>
					</li>
					<li>
						<h3>A long first scan.</h3>
						<p class="body">
							The first one is real work, not an instant answer. A recent scan took about 22 minutes
							to score 595 accounts, and wider reach takes longer. You can close the tab and come
							back.
						</p>
					</li>
					<li>
						<h3>Sometimes, no verdict.</h3>
						<p class="body">
							When there is not enough to go on, or Charcoal cannot read the language well, it says
							so instead of guessing.
						</p>
					</li>
				</ul>
			</div>
		</section>

		{@render band('band-close', 'var(--ink)', 'var(--purple-lit)', 'var(--gold)', 'var(--earth)')}

		<section id="close" class="field close">
			<div class="inner">
				<h2>Come in.</h2>
				<p class="voice">Say the hard thing. Look forward to who answers.</p>
				<a href="/login" class="cta on-dark">
					<span>Sign in with Bluesky</span>
					<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
						<path d="M4 12h15M13 5l7 7-7 7" />
					</svg>
				</a>
			</div>
		</section>
	</main>

	<footer class="site-footer">
		<div class="inner footer-row">
			<span class="wordmark">Charcoal</span>
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
		/* The palette is scoped to this page on purpose. The signed-in app still
		   wears the earlier look until it is moved over, and a :root declaration
		   here would follow a visitor into it. */
		--ink: #1b0b22;
		--purple: #45189c;
		--purple-lit: #a98bff;
		--gold: #f7b41c;
		--earth: #a53a1a;
		--leaf: #0c6b4a;
		--bone: #fff3d6;

		--font-poster: 'Tac One', 'Arial Narrow', sans-serif;
		--font-head: 'Ojuju', 'Atkinson Hyperlegible Next', sans-serif;
		--font-text: 'Atkinson Hyperlegible Next', system-ui, sans-serif;
		--ease-out: cubic-bezier(0.16, 1, 0.3, 1);

		min-height: 100dvh;
		overflow-x: clip;
		background: var(--gold);
		color: var(--ink);
		font-family: var(--font-text);
		font-size: var(--text-subtitle);
		line-height: 1.55;
	}

	/* The landing has no layout of its own to reset the page margin. */
	:global(body) {
		margin: 0;
	}

	.page :global(::selection) {
		background: var(--purple);
		color: var(--bone);
	}

	.page a {
		color: inherit;
		text-underline-offset: 0.2em;
		text-decoration-thickness: 2px;
	}

	.page a:focus-visible {
		outline: 3px solid currentColor;
		outline-offset: 4px;
		border-radius: var(--radius-3);
	}

	h1,
	h2,
	h3,
	p,
	ul,
	dl,
	dd,
	figure {
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

	.wordmark {
		font-family: var(--font-poster);
		font-size: var(--text-section-title);
		line-height: 1;
		text-transform: uppercase;
		text-decoration: none;
	}

	.nav-signin {
		padding: var(--space-10) var(--space-20);
		border: 2px solid var(--ink);
		border-radius: var(--radius-6);
		font-weight: 700;
		text-decoration: none;
		transition:
			background-color 0.25s var(--ease-out),
			color 0.25s var(--ease-out);
	}

	.nav-signin:hover {
		background: var(--ink);
		color: var(--gold);
	}

	/* ---------- Hero ---------- */
	.hero {
		position: relative;
		padding: var(--space-128) var(--space-32) 0;
	}

	.hero-inner {
		display: grid;
		grid-template-columns: minmax(0, 0.85fr) minmax(0, 1.15fr);
		align-items: end;
		gap: var(--space-32);
		min-height: calc(100dvh - var(--space-128));
	}

	.hero-text {
		/* On very wide screens, bring the text in line with the 1160px column
		   the rest of the page uses, while the dome stays on the viewport edge. */
		position: relative;
		left: max(0px, calc((100vw - 1224px) / 2));
		align-self: center;
		padding-bottom: var(--space-80);
	}

	h1 {
		font-family: var(--font-poster);
		font-size: var(--text-poster);
		font-weight: 400;
		line-height: 0.92;
		text-transform: uppercase;
	}

	h1 span {
		display: block;
	}

	h1 .yes {
		color: var(--purple);
	}

	/* 3.57:1 on gold: passes for display type, which is the only place it is used. */
	h1 .no {
		color: var(--earth);
	}

	.hero-lead {
		max-width: 34ch;
		margin-top: var(--space-32);
		font-size: var(--text-title);
		line-height: 1.45;
	}

	.cta {
		box-sizing: border-box;
		display: inline-flex;
		align-items: center;
		gap: var(--space-14);
		margin-top: var(--space-32);
		padding: var(--space-20) var(--space-32);
		border-radius: var(--radius-6);
		background: var(--purple);
		color: var(--bone);
		font-size: var(--text-title);
		font-weight: 700;
		text-decoration: none;
		transition:
			background-color 0.3s var(--ease-out),
			transform 0.3s var(--ease-out);
	}

	.page a.cta {
		color: var(--bone);
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
		background: var(--ink);
		transform: translateY(-2px);
	}

	.cta:hover svg {
		transform: translateX(4px);
	}

	.cta:active {
		transform: translateY(0);
	}

	.cta.on-dark {
		background: var(--gold);
	}

	.page a.cta.on-dark {
		color: var(--ink);
	}

	/* The ring must contrast with the ground behind the button, not with the
	   button's own text: bone on gold and ink on ink both disappear. */
	.page a.cta:focus-visible {
		outline-color: var(--ink);
	}

	.page a.cta.on-dark:focus-visible {
		outline-color: var(--gold);
	}

	.cta.on-dark:hover {
		background: var(--bone);
	}

	.hero-note {
		max-width: 40ch;
		margin-top: var(--space-20);
		font-size: var(--text-body);
	}

	/* ---------- The border ---------- */
	.border-figure {
		align-self: end;
		justify-self: end;
		width: 100%;
		/* Never taller than the viewport it rises into. */
		max-width: 92dvh;
		/* Bleed off the right edge so the dome reads as part of the ground. */
		margin-right: calc(var(--space-32) * -1);
		/* The rings are whole circles centred on the corner. Cut them along the
		   bottom and right, and leave the top and left open for arriving posts. */
		clip-path: inset(-50vh 0 0 -50vw);
	}

	.border-figure svg {
		display: block;
		width: 100%;
		height: auto;
		overflow: visible;
	}

	.border-figure figcaption {
		position: absolute;
		right: var(--space-32);
		bottom: var(--space-12);
		padding: var(--space-4) var(--space-10);
		border-radius: var(--radius-3);
		background: var(--ink);
		color: var(--bone);
		font-size: var(--text-caption);
	}

	.inside {
		fill: var(--purple);
	}

	.friend :global(.post-body) {
		fill: var(--bone);
	}

	.friend :global(.post-line) {
		fill: var(--purple);
	}

	.own :global(.post-body) {
		fill: var(--gold);
	}

	.own :global(.post-line) {
		fill: var(--ink);
	}

	.hostile :global(.post-body) {
		fill: var(--ink);
		stroke: var(--ink);
		stroke-width: 3;
		stroke-dasharray: 300 0;
	}

	.hostile :global(.post-line) {
		fill: var(--earth);
	}

	/* The one authored motion on the page. A post travels to the border; a
	   friend carries on inside, the other breaks into the same beads the outer
	   band is made of and is gone. */
	.travel {
		animation: 9s linear infinite both;
		animation-delay: var(--delay);
	}

	.let-in {
		animation-name: let-in;
	}

	.turned-away {
		animation-name: turned-away;
	}

	.turned-away :global(.post-body) {
		animation: break-body 9s linear infinite both;
		animation-delay: var(--delay);
	}

	.turned-away :global(.post-line) {
		animation: break-lines 9s linear infinite both;
		animation-delay: var(--delay);
	}

	@keyframes let-in {
		0% {
			transform: translate(0, 0);
			opacity: 0;
		}
		6% {
			opacity: 1;
		}
		70% {
			transform: translate(var(--dx), var(--dy));
			opacity: 1;
			animation-timing-function: var(--ease-out);
		}
		92% {
			transform: translate(var(--dx), var(--dy));
			opacity: 1;
		}
		100% {
			transform: translate(var(--dx), var(--dy));
			opacity: 0;
		}
	}

	@keyframes turned-away {
		0% {
			transform: translate(0, 0);
			opacity: 0;
		}
		6% {
			opacity: 1;
		}
		40% {
			transform: translate(var(--dx), var(--dy));
			opacity: 1;
		}
		68% {
			transform: translate(var(--dx), var(--dy));
			opacity: 1;
		}
		82%,
		100% {
			transform: translate(var(--dx), var(--dy));
			opacity: 0;
		}
	}

	@keyframes break-body {
		0%,
		40% {
			fill-opacity: 1;
			stroke-dasharray: 300 0;
		}
		60%,
		100% {
			fill-opacity: 0;
			stroke-dasharray: 3 12;
		}
	}

	@keyframes break-lines {
		0%,
		40% {
			opacity: 1;
		}
		55%,
		100% {
			opacity: 0;
		}
	}

	/* ---------- Bands ---------- */
	.band {
		display: block;
	}

	/* ---------- Fields ---------- */
	.field {
		padding: var(--space-96) var(--space-32) var(--space-128);
	}

	h2 {
		max-width: 16ch;
		margin-bottom: var(--space-40);
		font-family: var(--font-poster);
		font-size: var(--text-shout);
		font-weight: 400;
		line-height: 0.98;
		text-transform: uppercase;
		text-wrap: balance;
	}

	h3 {
		font-family: var(--font-head);
		font-size: var(--text-section-title);
		font-weight: 700;
		line-height: 1.15;
	}

	.voice {
		max-width: 32ch;
		font-family: var(--font-head);
		font-size: var(--text-voice);
		font-weight: 600;
		line-height: 1.28;
	}

	.body {
		max-width: 62ch;
	}

	/* Now: the braced state. Ink ground, the only section with no bright field. */
	.now {
		background: var(--ink);
		color: var(--bone);
	}

	.now h2 {
		color: var(--gold);
	}

	.now-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--space-64);
		align-items: start;
	}

	.voice.quiet {
		color: var(--purple-lit);
	}

	.the-ones {
		display: grid;
		gap: var(--space-16);
		margin-top: var(--space-32);
	}

	.the-ones li {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		align-items: baseline;
		gap: var(--space-20);
		max-width: 30ch;
		font-family: var(--font-head);
		font-size: var(--text-voice);
		font-weight: 700;
		line-height: 1.2;
	}

	/* A diamond from the band, standing where a bullet would. */
	.the-ones li::before {
		content: '';
		width: 0.5em;
		height: 0.5em;
		background: var(--earth);
		transform: translateY(-0.1em) rotate(45deg);
	}

	/* Could be: the same person, in full colour. */
	.could {
		background: var(--purple);
		color: var(--bone);
	}

	.could h2 {
		max-width: 18ch;
		color: var(--gold);
	}

	.claim span {
		display: block;
	}

	.claim {
		margin-top: var(--space-64);
		margin-bottom: var(--space-24);
		color: var(--gold);
		font-family: var(--font-poster);
		font-size: var(--text-poster);
		line-height: 0.92;
		text-transform: uppercase;
		text-wrap: balance;
	}

	/* How it works: four claims as a ruled list, not four cards. */
	.how {
		background: var(--earth);
		color: var(--bone);
	}

	.how .intro {
		font-size: var(--text-title);
	}

	.claims {
		margin-top: var(--space-48);
		border-bottom: 2px solid var(--bone);
	}

	.claims > div {
		display: grid;
		grid-template-columns: minmax(0, 5fr) minmax(0, 7fr);
		gap: var(--space-32);
		padding: var(--space-28) 0;
		border-top: 2px solid var(--bone);
	}

	.claims dt {
		font-family: var(--font-head);
		font-size: var(--text-page-title);
		font-weight: 700;
		line-height: 1.12;
	}

	.claims dd {
		max-width: 56ch;
	}

	/* Authority: back out into the daylight. */
	.authority {
		background: var(--gold);
		color: var(--ink);
	}

	.split {
		display: grid;
		grid-template-columns: minmax(0, 5fr) minmax(0, 6fr);
		gap: var(--space-64);
		align-items: start;
	}

	.authority h2 {
		max-width: 12ch;
	}

	.points {
		display: grid;
		gap: var(--space-40);
	}

	.points h3 {
		margin-bottom: var(--space-8);
		color: var(--purple);
	}

	/* Expect: plain facts, set plainly. */
	.expect {
		background: var(--leaf);
		color: var(--bone);
	}

	.facts {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--space-48) var(--space-64);
	}

	.facts h3 {
		margin-bottom: var(--space-8);
	}

	/* Close */
	.close {
		background: var(--ink);
		color: var(--bone);
	}

	.close h2 {
		margin-bottom: var(--space-24);
		color: var(--gold);
		font-size: var(--text-poster);
		line-height: 0.92;
	}

	/* ---------- Footer ---------- */
	.site-footer {
		padding: var(--space-32);
		background: var(--ink);
		color: var(--bone);
		border-top: 2px solid var(--purple-lit);
	}

	.footer-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-16) var(--space-40);
	}

	.site-footer .wordmark {
		color: var(--gold);
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
			padding: var(--space-96) var(--space-20) 0;
		}

		.hero-inner {
			grid-template-columns: minmax(0, 1fr);
			min-height: 0;
		}

		.hero-text {
			padding-bottom: 0;
		}

		.border-figure {
			margin-right: calc(var(--space-20) * -1);
			margin-left: var(--space-48);
		}

		.border-figure figcaption {
			right: var(--space-20);
		}

		.field {
			padding: var(--space-64) var(--space-20) var(--space-80);
		}

		.claims > div,
		.split,
		.facts,
		.now-grid {
			grid-template-columns: minmax(0, 1fr);
		}

		.now-grid {
			gap: var(--space-32);
		}

		.border-figure {
			max-width: none;
		}

		.claims > div {
			gap: var(--space-10);
		}

		.split {
			gap: var(--space-40);
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

	/* The still plate: every post where its journey ends. People reading this
	   are often upset already, so motion is something they opt into. */
	@media (prefers-reduced-motion: reduce) {
		.travel,
		.turned-away :global(.post-body),
		.turned-away :global(.post-line) {
			animation: none;
		}

		.travel {
			transform: translate(var(--dx), var(--dy));
		}

		.turned-away :global(.post-body) {
			fill-opacity: 0;
			stroke-dasharray: 3 12;
		}

		.turned-away :global(.post-line) {
			opacity: 0.35;
		}

		.cta,
		.cta svg,
		.nav-signin {
			transition: none;
		}
	}
</style>
