/** Latest-wins guard for overlapping async refreshes.
 *
 *  A page that polls *and* refreshes on user action (the actions list: a
 *  3 s timer plus `disconnect()`) can have two loads in flight. Whichever
 *  resolves last writes state — and that can be the older one, putting a
 *  pre-disconnect "connected" status back on screen and re-arming the poll
 *  from stale rows. Each load takes a ticket with `next()`; after every
 *  `await` it checks `isCurrent(ticket)` and drops its result if a newer
 *  load has started since. */
export function generation() {
	let current = 0;
	return {
		next(): number {
			current += 1;
			return current;
		},
		isCurrent(ticket: number): boolean {
			return ticket === current;
		}
	};
}

export type Generation = ReturnType<typeof generation>;
