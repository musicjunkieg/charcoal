<script lang="ts">
	import type { ActionKind } from '$lib/types.js';
	interface Props {
		kind: ActionKind;
		count: number;
		label: string;
		connected: boolean;
		onconfirm: () => void;
		oncancel: () => void;
	}
	let { kind, count, label, connected, onconfirm, oncancel }: Props = $props();
</script>

<div class="sheet" role="dialog" aria-modal="true">
	<p>{kind === 'mute' ? 'Mute' : 'Block'} {count === 1 ? label : `${count} accounts`}?</p>
	{#if !connected}
		<p>Charcoal needs permission to mute or block on your behalf. You'll approve exactly these two abilities on Bluesky — nothing else.</p>
	{/if}
	<button onclick={oncancel}>Cancel</button>
	<button onclick={onconfirm}>{connected ? 'Confirm' : 'Continue to Bluesky'}</button>
</div>
