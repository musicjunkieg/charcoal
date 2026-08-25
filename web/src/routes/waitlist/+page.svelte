<script lang="ts">
	import '$lib/website/styles/tokens.css';
</script>

<svelte:head>
	<title>Charcoal — You're on the list</title>
</svelte:head>

<main class="waitlist">
	<h1>You're on the list</h1>
	<p>
		Charcoal is onboarding a small number of people at a time. Your request has
		been recorded — if you're granted access, signing in with your Bluesky
		account will just work.
	</p>
	<a href="/">Back to the front page</a>
</main>

<style>
	.waitlist {
		max-width: 32rem;
		margin: 4rem auto;
		padding: 0 1rem;
		font-family: var(--font-body);
		color: var(--cream-100);
	}
	h1 {
		font-family: var(--font-display);
	}
</style>
