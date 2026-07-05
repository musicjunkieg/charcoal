// Pure logic behind the ScanProgress checklist: mapping backend scan phases
// onto the four user-facing steps, and the classification progress bar math.
// Kept out of the component so it can be unit-tested with vitest.

import type { ScanPhase } from './types.js';

// Four user-meaningful steps mapped from the backend's finer-grained
// phases, so the checklist advances steadily instead of churning.
export const STEPS: Array<{ label: string; phases: ScanPhase[] }> = [
	{ label: 'Reading your posts', phases: ['starting', 'loading_models', 'fingerprint'] },
	{ label: "Finding who's engaging", phases: ['discovering'] },
	{ label: 'Scoring accounts', phases: ['scoring', 'gathering', 'classifying'] },
	{ label: 'Building your report', phases: ['finalizing'] }
];

// Which checklist step a phase belongs to. Phases outside the running set
// (idle/done/failed) clamp to the first step rather than -1, which would
// render every step as already completed.
export function phaseToStepIndex(phase: ScanPhase): number {
	return Math.max(
		0,
		STEPS.findIndex((s) => s.phases.includes(phase))
	);
}

// Rounded percentage for the "X of Y posts classified" bar, or null when
// there is no meaningful denominator. Capped at 100 so a count glitch never
// overflows the bar.
export function classificationPercent(done: number | null, total: number | null): number | null {
	if (done === null || total === null || total <= 0) return null;
	return Math.min(100, Math.round((done / total) * 100));
}
