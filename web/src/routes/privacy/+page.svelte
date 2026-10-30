<!--
	/privacy — what Charcoal holds, about whom, and for how long (#259).

	Content contract: every claim here is something the code does today. The
	numbers come from $lib/privacy-facts, which a test checks against the
	backend, so a change in retention or limits fails CI until this page is
	updated to match. Plain language; Hearth Watch register, same backdrop as
	/waitlist, but a left-aligned reading column because this is long-form.
-->
<script lang="ts">
	import '$lib/website/styles/tokens.css';
	import { PRIVACY_FACTS as f } from '$lib/privacy-facts';
</script>

<svelte:head>
	<title>Privacy — Charcoal</title>
	<meta
		name="description"
		content="What Charcoal stores about the people who use it and the accounts it scores, who sees it, and how long it is kept."
	/>
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

	<main class="container">
		<article class="content">
			<header>
				<h1 class="title">Privacy at Charcoal</h1>
				<p class="meta">Last updated {f.lastUpdated}. Charcoal is an invite-only alpha.</p>
			</header>

			<p class="lede">
				Charcoal looks at public Bluesky activity around a person and estimates which accounts
				are likely to engage with them in bad faith. That means it handles two kinds of people:
				<strong>people who use Charcoal</strong>, and <strong>accounts Charcoal scores</strong>,
				who never signed up. This page covers both, and says only what the software actually
				does today.
			</p>

			<section>
				<h2>Who runs it</h2>
				<p>
					Charcoal is run by one person, Bryan Guffey
					(<a href="https://bsky.app/profile/chaosgreml.in">@chaosgreml.in</a>), not a company.
					For questions, or to see or delete data, email
					<a href="mailto:{f.contactEmail}">{f.contactEmail}</a>.
				</p>
			</section>

			<section>
				<h2>If you use Charcoal</h2>
				<p>When you sign in with Bluesky, Charcoal stores:</p>
				<ul>
					<li>your Bluesky account ID and handle, and when you signed up and last signed in;</li>
					<li>
						a <em>topic fingerprint</em> built from your own public posts, so it can tell which
						accounts post about the same things you do;
					</li>
					<li>
						the accounts Charcoal found and scored for you (see the next section), and any labels
						or notes you add to them;
					</li>
					<li>your request for access, and whether it was approved.</li>
				</ul>
				<p>
					Signing in on its own asks Bluesky only to confirm who you are. With just that, Charcoal
					can't post, follow, or read anything private.
				</p>
				<p>
					<strong>If you turn on mute and block,</strong> Charcoal asks Bluesky for exactly these
					permissions and nothing more: create and delete blocks, mute and unmute accounts, and
					list your existing mutes and blocks. It stores that permission encrypted, so it can act
					when you ask, along with a log of every mute and block it made for you (who, when, and
					the score at the time) so you can undo it. <strong>Disconnect</strong> removes the stored
					permission immediately.
				</p>
			</section>

			<section>
				<h2>If Charcoal scored your account</h2>
				<p>
					If you post on Bluesky, Charcoal may have scored your account for a user who can see
					your public activity. It also notes any account that quoted or reposted a user's posts
					(the handle and the text of the quote), whether or not it went on to score that
					account. For each scored account it keeps:
				</p>
				<ul>
					<li>your account ID and handle;</li>
					<li>
						scores: how often your posts read as hostile, how much you post about the same topics
						as the user, and a few behavioral signals;
					</li>
					<li>a threat tier (High, Elevated, Watch, or Low);</li>
					<li>
						<strong>up to {f.examplePostsPerAccount} of your public posts</strong> as examples behind
						the score, plus the text of any post where you quoted or replied to the user.
					</li>
				</ul>
				<p>
					To score you, Charcoal reads your recent public posts and keeps up to
					{f.postsCachedPerAccount} of them in a cache for <strong>{f.postCacheDays} days</strong>.
				</p>
				<p>
					<strong>Who sees this:</strong> only the user it was scored for, and the operator. It is
					never published, shared between users, sold, or turned into a list or label. Charcoal
					doesn't contact you. The {f.postCacheDays}-day cache of your public posts is reused when
					another user's scan needs the same account, so Charcoal doesn't fetch them twice, but
					no user is ever shown it.
				</p>
				<p>
					<strong>If someone blocks or mutes you through Charcoal:</strong> a mute is private. A
					block is visible on Bluesky, as every block is, but it carries nothing from Charcoal: no
					reason, score, label, list, or mention of Charcoal.
				</p>
				<p><strong>What you can ask for:</strong></p>
				<ul>
					<li>
						<strong>To see it.</strong> Email the operator and you'll get what Charcoal holds about
						your account.
					</li>
					<li>
						<strong>A review.</strong> If you think a score is wrong (say, a heated post about a
						bigot was read as hostility toward everyone), ask, and the operator will look at it
						and correct real mistakes.
					</li>
				</ul>
				<p>
					<strong>What Charcoal won't do is delete a score just because it's asked to.</strong>
					Charcoal exists to protect people from harassment, and that protection would mean little
					if the accounts it flags could remove themselves on request. Charcoal relies on that
					protective purpose, its legitimate interest, to process public posts, and weighs each
					objection against it. The safeguards that make this fair: scores are visible only to the
					person they protect, are never published, and don't follow you anywhere else on
					Bluesky.
				</p>
			</section>

			<section>
				<h2>Services that receive data</h2>
				<ul>
					<li><strong>Railway</strong> hosts Charcoal and its database, in the US.</li>
					<li>
						<strong>RunPod</strong> runs the AI model that reads posts for hostility. Only post text
						that a first, local check flags as possibly hostile is sent, along with the post it
						replied to.
					</li>
					<li>
						<strong>Bluesky</strong> (its public API, and the account directory at plc.directory),
						to read public posts and look up accounts, and <strong>Constellation</strong>
						(constellation.microcosm.blue), to find quotes and reposts of a user's posts.
					</li>
					<li>
						<strong>Your Bluesky server</strong>, for sign-in and for any mutes or blocks you ask
						for.
					</li>
					<li>
						<strong>typeahead.waow.tech</strong> suggests handles as you type on the sign-in page,
						so it sees what you type there.
					</li>
					<li>
						<strong>Google Fonts</strong> supplies the site's typefaces, so it sees your IP address
						when a page loads.
					</li>
				</ul>
			</section>

			<section>
				<h2>Cookies and tracking</h2>
				<p>
					One cookie keeps you signed in for {f.cookieHours} hours; signing out removes it. No
					analytics, no ads, no tracking pixels.
				</p>
			</section>

			<section>
				<h2>How long it's kept</h2>
				<ul>
					<li>
						Your account data, and the scores made for you, are kept until your account is
						deleted. A score counts as current for {f.scoreFreshDays.min} to
						{f.scoreFreshDays.max} days and is then refreshed, but older scores aren't yet deleted
						automatically. Limiting that is on the roadmap, and this page will change when it
						lands.
					</li>
					<li>
						The post cache is deleted after {f.postCacheDays} days; internal model results, which
						are stored without the post text, after {f.modelResultDays} days.
					</li>
					<li>Server logs record account IDs and handles for troubleshooting.</li>
					<li>
						Before each upgrade, the operator takes a backup of the database and keeps a few recent
						backups on his own computer.
					</li>
				</ul>
			</section>

			<section>
				<h2>Deleting your account</h2>
				<p>
					Email the operator and your account's data is deleted: your fingerprint, the scores made
					for you, your labels, your action log, and your stored permission.
					<strong>Mutes and blocks already made stay on Bluesky</strong>, because they're yours, so
					undo any you want reversed in Charcoal first.
				</p>
				<p>
					Not removed at that moment: the record that you asked for access and the decision on it;
					the shared cache of public posts, which is gone within {f.postCacheDays} days; server logs,
					which age out; and the few recent backups the operator keeps.
				</p>
			</section>

			<section>
				<h2>Changes</h2>
				<p>
					If this page changes, the date at the top changes. Anything significant will be
					announced to alpha users.
				</p>
			</section>

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
		</article>
	</main>
</div>

<style>
	.ground {
		min-height: 100vh;
		min-height: 100dvh;
		position: relative;
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
		background: radial-gradient(ellipse 80% 50% at 50% 0%, rgb(var(--copper-rgb) / 0.06) 0%, transparent 60%);
		pointer-events: none;
	}

	.container {
		position: relative;
		display: flex;
		justify-content: center;
		padding: var(--space-64) var(--space-32);
	}

	.content {
		width: 100%;
		max-width: 640px;
	}

	.title {
		font-family: var(--font-display);
		font-size: var(--text-headline);
		font-weight: 400;
		line-height: 1.2;
		letter-spacing: -0.01em;
		color: var(--cream-50);
		margin-bottom: var(--space-8);
	}

	.meta {
		font-size: var(--text-small);
		color: var(--charcoal-400);
		margin-bottom: var(--space-32);
	}

	.lede,
	p,
	li {
		font-size: var(--text-body);
		font-weight: 300;
		line-height: 1.7;
		color: var(--charcoal-300);
	}

	p {
		margin-bottom: var(--space-16);
	}

	ul {
		margin: 0 0 var(--space-16) var(--space-20);
		padding: 0;
	}

	li {
		margin-bottom: var(--space-8);
	}

	strong {
		font-weight: 500;
		color: var(--cream-100);
	}

	section {
		margin-top: var(--space-40);
	}

	h2 {
		font-family: var(--font-display);
		font-size: var(--text-section-title);
		font-weight: 400;
		line-height: 1.3;
		color: var(--cream-50);
		margin-bottom: var(--space-16);
	}

	a {
		color: var(--copper);
		text-underline-offset: 3px;
	}

	a:focus-visible {
		outline: 2px solid var(--copper);
		outline-offset: 2px;
		border-radius: var(--radius-2);
	}

	.home-link {
		display: inline-flex;
		align-items: center;
		gap: var(--space-8);
		margin-top: var(--space-48);
		padding: var(--space-10) var(--space-20);
		font-size: var(--text-body);
		font-weight: 500;
		color: var(--charcoal-300);
		text-decoration: none;
		border: 1px solid rgb(var(--charcoal-400-rgb) / 0.2);
		border-radius: var(--radius-8);
		transition: color 0.3s var(--ease-in-out), border-color 0.3s var(--ease-in-out),
			background 0.3s var(--ease-in-out);
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
	}

	@media (max-width: 480px) {
		.container {
			padding: var(--space-40) var(--space-16);
		}
		.title {
			font-size: var(--text-page-title);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.home-link {
			transition: none;
		}
	}
</style>
