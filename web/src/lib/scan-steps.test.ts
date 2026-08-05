import { describe, expect, it } from 'vitest';
import { STEPS, classificationPercent, phaseToStepIndex } from './scan-steps.js';

describe('phaseToStepIndex', () => {
	it('maps setup phases to step 0 (reading your posts)', () => {
		expect(phaseToStepIndex('starting')).toBe(0);
		expect(phaseToStepIndex('loading_models')).toBe(0);
		expect(phaseToStepIndex('fingerprint')).toBe(0);
	});

	it('maps discovering to step 1 (finding who is engaging)', () => {
		expect(phaseToStepIndex('discovering')).toBe(1);
	});

	it('maps all scoring-stage phases to step 2 (scoring accounts)', () => {
		expect(phaseToStepIndex('scoring')).toBe(2);
		expect(phaseToStepIndex('gathering')).toBe(2);
		expect(phaseToStepIndex('classifying')).toBe(2);
	});

	it('maps finalizing to step 3 (building your report)', () => {
		expect(phaseToStepIndex('finalizing')).toBe(3);
	});

	it('clamps phases outside the running set to step 0', () => {
		// idle/done/failed should never render the checklist, but if they do,
		// point at the first step rather than -1 (which would mark all done).
		expect(phaseToStepIndex('idle')).toBe(0);
		expect(phaseToStepIndex('done')).toBe(0);
		expect(phaseToStepIndex('failed')).toBe(0);
	});

	it('covers every step with at least one phase', () => {
		const covered = new Set(STEPS.flatMap((s) => s.phases).map(phaseToStepIndex));
		expect([...covered].sort()).toEqual([0, 1, 2, 3]);
	});
});

describe('classificationPercent', () => {
	it('computes the rounded percentage', () => {
		expect(classificationPercent(250, 400)).toBe(63);
		expect(classificationPercent(0, 400)).toBe(0);
		expect(classificationPercent(400, 400)).toBe(100);
	});

	it('returns null when either count is missing', () => {
		expect(classificationPercent(null, 400)).toBeNull();
		expect(classificationPercent(250, null)).toBeNull();
		expect(classificationPercent(null, null)).toBeNull();
	});

	it('returns null for a zero or negative total (no meaningful bar)', () => {
		expect(classificationPercent(0, 0)).toBeNull();
		expect(classificationPercent(5, -1)).toBeNull();
	});

	it('caps at 100 even if done exceeds total', () => {
		expect(classificationPercent(500, 400)).toBe(100);
	});
});
