<script lang="ts">
	import type { ScanStatus } from '$lib/types.js';

	let { status, elapsed }: { status: ScanStatus; elapsed: string } = $props();

	// Four user-meaningful steps mapped from the backend's finer-grained
	// phases, so the checklist advances steadily instead of churning.
	const STEPS = [
		{ label: 'Reading your posts', phases: ['starting', 'loading_models', 'fingerprint'] },
		{ label: "Finding who's engaging", phases: ['discovering'] },
		{ label: 'Scoring accounts', phases: ['scoring', 'gathering', 'classifying'] },
		{ label: 'Building your report', phases: ['finalizing'] }
	];

	let stepIndex = $derived(
		Math.max(
			0,
			STEPS.findIndex((s) => s.phases.includes(status.phase))
		)
	);

	let clsTotal = $derived(status.progress?.classifications_total ?? null);
	let clsDone = $derived(status.progress?.classifications_done ?? null);
	let candidates = $derived(status.progress?.candidates_total ?? null);
	let barPercent = $derived(
		clsTotal !== null && clsDone !== null && clsTotal > 0
			? Math.min(100, Math.round((clsDone / clsTotal) * 100))
			: null
	);
</script>

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
				<div class="bar-track">
					<div class="bar-fill" style="width: {barPercent}%"></div>
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

<style>
	.scan-progress {
		padding: 1.25rem 1.5rem;
		background: rgba(28, 25, 23, 0.6);
		border: 1px solid rgba(201, 149, 108, 0.2);
		border-radius: 14px;
		margin-bottom: 2rem;
	}

	.progress-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin-bottom: 1rem;
	}

	.progress-title {
		font-size: 1rem;
		font-weight: 500;
		color: #e8b48a;
		letter-spacing: 0.01em;
	}

	.progress-elapsed {
		font-size: 0.8125rem;
		color: #78716c;
		font-variant-numeric: tabular-nums;
	}

	.steps {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem 1.5rem;
		list-style: none;
		padding: 0;
		margin: 0 0 1rem 0;
	}

	.step {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.875rem;
		color: #57534e;
	}

	.step.done {
		color: #a8a29e;
	}
	.step.active {
		color: #fef3c7;
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
		color: #86efac;
		font-size: 0.8125rem;
	}

	.step-dot::before {
		content: '';
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: #44403c;
	}

	.step-spinner {
		border: 2px solid rgba(201, 149, 108, 0.2);
		border-top-color: #c9956c;
		border-radius: 50%;
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
		gap: 0.5rem;
	}

	.bar-row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.bar-track {
		flex: 1;
		height: 6px;
		background: rgba(12, 10, 9, 0.6);
		border-radius: 3px;
		overflow: hidden;
	}

	.bar-fill {
		height: 100%;
		background: linear-gradient(90deg, #f59e0b 0%, #c9956c 100%);
		border-radius: 3px;
		transition: width 0.5s ease;
	}

	.bar-text {
		font-size: 0.8125rem;
		color: #c9956c;
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}

	.counters {
		display: flex;
		flex-wrap: wrap;
		gap: 0.375rem 1.25rem;
	}

	.counter {
		font-size: 0.8125rem;
		color: #a8a29e;
		font-variant-numeric: tabular-nums;
	}

	.progress-message {
		font-size: 0.8125rem;
		color: #78716c;
	}

	.expectation {
		font-size: 0.8125rem;
		color: #57534e;
		line-height: 1.5;
		margin-top: 0.25rem;
	}
</style>
