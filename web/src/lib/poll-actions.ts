// Decision logic for each dashboard poll tick, extracted so the falling-edge
// behavior (the fix for stale results after a scan completes, gap #108) is
// unit-testable.

export interface PollActions {
	/** Re-fetch events + top threats (partial results while running, final on finish). */
	refreshResults: boolean;
	/** Re-fetch accuracy metrics (only worth doing once, when the scan finishes). */
	refreshAccuracy: boolean;
}

export function pollActions(prevRunning: boolean, nowRunning: boolean): PollActions {
	const fallingEdge = prevRunning && !nowRunning;
	return {
		refreshResults: nowRunning || fallingEdge,
		refreshAccuracy: fallingEdge
	};
}
