<script lang="ts">
	// Design tokens (#250): this component styles itself from the shared
	// palette rather than another set of literal hex values. The import is
	// side-effectful CSS — it defines the :root custom properties the styles
	// below reference.
	import '$lib/website/styles/tokens.css';
	import { STEPS, classificationPercent, phaseToStepIndex } from '$lib/scan-steps.js';
	import { isQueued, queueMessage } from '$lib/dashboard-state.js';
	import type { ScanStatus } from '$lib/types.js';

	let { status, elapsed }: { status: ScanStatus; elapsed: string } = $props();

	// Queued is NOT running (#257): the scan has been accepted but nothing has
	// started, so the checklist, the bar and the counters are all withheld
	// rather than shown at zero. A progress UI over an idle server is a lie,
	// and this one is told to people waiting on their own safety.
	let waiting = $derived(isQueued(status));
	// The server sends `queue` only while queued; if the phase says queued and
	// the block is missing (an older server, or a row that moved between the
	// two reads), say so without a position rather than guessing one.
	let queue = $derived(status.queue ?? null);

	let stepIndex = $derived(phaseToStepIndex(status.phase));

	let clsTotal = $derived(status.progress?.classifications_total ?? null);
	let clsDone = $derived(status.progress?.classifications_done ?? null);
	let candidates = $derived(status.progress?.candidates_total ?? null);
	let barPercent = $derived(classificationPercent(clsDone, clsTotal));
</script>

{#if waiting}
	<div class="scan-progress">
		<div class="progress-header">
			<h2 class="progress-title">Waiting to start</h2>
		</div>

		<div class="progress-detail">
			<!-- Only the position line is a live region: it is the one thing that
			     changes as the queue moves, and re-announcing the whole panel on
			     every 5s poll would be unusable with a screen reader. Polite,
			     never assertive — a queue position does not interrupt. -->
			<p class="queue-position" aria-live="polite">
				<span class="queue-dot" aria-hidden="true"></span>
				{queue ? queueMessage(queue) : "You're in line for a scan slot"}
			</p>

			<p class="queue-note">
				Your scan hasn't started yet. It begins as soon as a slot frees up — nothing is running
				until then.
			</p>

			{#if queue && queue.eta_seconds !== null}
				<p class="queue-note">
					The time is an estimate from how long recent scans took, so it can move in either
					direction.
				</p>
			{:else}
				<p class="queue-note">
					There's no time estimate yet — no recent scan has finished to estimate from.
				</p>
			{/if}

			<p class="queue-note">
				You can close this page. The scan runs on the server, and your results will be here when it
				finishes.
			</p>
		</div>
	</div>
{:else}
	<div class="scan-progress" aria-live="polite">
		<div class="progress-header">
			<h2 class="progress-title">Scan in progress</h2>
			<span class="progress-elapsed">{elapsed}</span>
		</div>

		<ol class="steps">
			{#each STEPS as step, i (step.label)}
				<li class="step" class:done={i < stepIndex} class:active={i === stepIndex}>
					{#if i < stepIndex}
						<span class="step-icon step-check" aria-hidden="true">✓</span>
					{:else if i === stepIndex}
						<span class="step-icon step-spinner" aria-hidden="true"></span>
					{:else}
						<span class="step-icon step-dot" aria-hidden="true"></span>
					{/if}
					<span class="step-label">{step.label}</span>
				</li>
			{/each}
		</ol>

		<div class="progress-detail">
			{#if barPercent !== null}
				<div class="bar-row">
					<div
						class="bar-track"
						role="progressbar"
						aria-valuenow={barPercent}
						aria-valuemin={0}
						aria-valuemax={100}
						aria-label="{clsDone} of {clsTotal} posts classified"
					>
						<!-- scaleX, not width: animating width relayouts every frame of
						     the transition. transform-origin is the left edge so the bar
						     grows rightward the way width did. -->
						<div class="bar-fill" style="transform: scaleX({barPercent / 100})"></div>
					</div>
					<span class="bar-text">{clsDone} of {clsTotal} posts classified</span>
				</div>
			{/if}
			<div class="counters">
				{#if candidates !== null}
					<span class="counter">{candidates} accounts queued for scoring</span>
				{/if}
				{#if status.tier_counts.total > 0}
					<span class="counter">{status.tier_counts.total} accounts scored so far</span>
				{/if}
			</div>
			{#if status.progress_message}
				<p class="progress-message">{status.progress_message}</p>
			{/if}
			<p class="expectation">
				First scans usually take 5–15 minutes depending on how many accounts engage with your posts.
				Results fill in below as they're scored — you can browse them now.
			</p>
		</div>
	</div>
{/if}

<style>
	.scan-progress {
		padding: var(--space-20) var(--space-24);
		background: rgb(var(--charcoal-900-rgb) / 0.6);
		border: 1px solid rgb(var(--copper-rgb) / 0.2);
		border-radius: var(--radius-14);
		margin-bottom: var(--space-32);
	}

	.progress-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin-bottom: var(--space-16);
	}

	.progress-title {
		font-size: var(--text-body);
		font-weight: 500;
		color: var(--copper-light);
		letter-spacing: 0.01em;
	}

	/* Contrast (#249): was --charcoal-500, which lands at 3.6:1 on this panel
	   and fails AA for body text. --charcoal-400 is 6.9:1. */
	.progress-elapsed {
		font-size: var(--text-label);
		color: var(--charcoal-400);
		font-variant-numeric: tabular-nums;
	}

	.steps {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-8) var(--space-24);
		list-style: none;
		padding: 0;
		margin: 0 0 var(--space-16) 0;
	}

	/* All three step states carry readable text (#249); the state is conveyed
	   by the icon and the weight, not by fading the label below AA. */
	.step {
		display: flex;
		align-items: center;
		gap: var(--space-8);
		font-size: var(--text-small);
		color: var(--charcoal-400);
	}

	.step.done {
		color: var(--charcoal-400);
	}
	.step.active {
		color: var(--cream-100);
		font-weight: 500;
	}

	.step-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 16px;
		height: 16px;
		flex-shrink: 0;
	}

	.step-check {
		color: var(--status-ok);
		font-size: var(--text-label);
	}

	.step-dot::before {
		content: '';
		width: 6px;
		height: 6px;
		border-radius: var(--radius-circle);
		background: var(--charcoal-700);
	}

	.step-spinner {
		border: 2px solid rgb(var(--copper-rgb) / 0.2);
		border-top-color: var(--copper);
		border-radius: var(--radius-circle);
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	.progress-detail {
		display: flex;
		flex-direction: column;
		gap: var(--space-8);
	}

	.bar-row {
		display: flex;
		align-items: center;
		gap: var(--space-12);
	}

	.bar-track {
		flex: 1;
		height: 6px;
		background: rgb(var(--charcoal-950-rgb) / 0.6);
		border-radius: var(--radius-3);
		overflow: hidden;
	}

	.bar-fill {
		/* Full width, scaled down — so the transition drives `transform`, which
		   the compositor handles, instead of `width`, which relayouts on every
		   frame. The gradient looks the same either way: it spans the element,
		   so compressing it by scale matches compressing it by width. The one
		   real difference is that scaleX squashes this radius horizontally on
		   the growing edge — sub-pixel on a 6px bar. */
		width: 100%;
		height: 100%;
		background: linear-gradient(90deg, var(--amber-500) 0%, var(--copper) 100%);
		border-radius: var(--radius-3);
		transform-origin: left;
		transition: transform 0.5s ease;
	}

	.bar-text {
		font-size: var(--text-label);
		color: var(--copper);
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}

	.counters {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-6) var(--space-20);
	}

	.counter {
		font-size: var(--text-label);
		color: var(--charcoal-400);
		font-variant-numeric: tabular-nums;
	}

	/* Both were below AA on this background before (#249). */
	.progress-message {
		font-size: var(--text-label);
		color: var(--charcoal-400);
	}

	.expectation {
		font-size: var(--text-label);
		color: var(--charcoal-400);
		line-height: 1.5;
		margin-top: var(--space-4);
	}

	/* Waiting state (#257). Quiet by design: this is the system working, not a
	   failure, and not progress either. */
	.queue-position {
		display: flex;
		align-items: center;
		gap: var(--space-8);
		/* typography.body / colors.body-text-bright from DESIGN.md. */
		font-size: var(--text-body);
		color: var(--charcoal-300);
	}

	/* A still dot, deliberately: anything that moves here would imply work is
	   happening, and none is. */
	.queue-dot {
		width: 8px;
		height: 8px;
		border-radius: var(--radius-circle);
		background: var(--charcoal-400);
		flex-shrink: 0;
	}

	.queue-note {
		font-size: var(--text-label);
		color: var(--charcoal-400);
		line-height: 1.5;
	}

	/* Motion is opt-out (#248). The spinner keeps its copper arc so the active
	   step stays identifiable while standing still, and the bar jumps to its
	   new width instead of sliding. */
	@media (prefers-reduced-motion: reduce) {
		.step-spinner {
			animation: none;
		}

		.bar-fill {
			transition: none;
		}
	}
</style>
