// Toast store for in-place action feedback (#332, spec §3.2). Pure TS on
// `svelte/store` so vitest can drive it without a DOM; `Toast.svelte` is the
// only renderer. Newest toast is LAST in the array — the renderer stacks
// bottom-up, so the newest sits nearest the viewport edge.
import { writable, type Readable } from 'svelte/store';

export type ToastTone = 'working' | 'ok' | 'error';

export interface ToastAction {
	label: string;
	onclick: () => void;
}

export interface Toast {
	id: number;
	tone: ToastTone;
	text: string;
	actions: ToastAction[];
	/** A plain link rendered after the actions, e.g. "Record" → the batch page. */
	href?: { label: string; url: string };
	/** Auto-dismiss delay. The timer (re)starts whenever a raise or update
	 *  carries this field, so a settled update — not the working raise —
	 *  is what starts the clock. */
	ttlMs?: number;
}

export type ToastInput = Omit<Toast, 'id'>;

/** More than this and the stack hides content; the oldest goes first. */
export const MAX_TOASTS = 3;

const store = writable<Toast[]>([]);
export const toasts: Readable<Toast[]> = { subscribe: store.subscribe };

let nextId = 1;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function clearTimer(id: number) {
	const t = timers.get(id);
	if (t !== undefined) {
		clearTimeout(t);
		timers.delete(id);
	}
}

function armTimer(id: number, ttlMs: number | undefined) {
	clearTimer(id);
	if (ttlMs === undefined) return;
	timers.set(
		id,
		setTimeout(() => {
			timers.delete(id);
			dismiss(id);
		}, ttlMs)
	);
}

export function raise(input: ToastInput): number {
	const id = nextId++;
	store.update((list) => {
		const next = [...list, { ...input, id }];
		// Drop from the front: the oldest toasts are the least relevant.
		while (next.length > MAX_TOASTS) {
			const dropped = next.shift()!;
			clearTimer(dropped.id);
		}
		return next;
	});
	armTimer(id, input.ttlMs);
	return id;
}

export function update(id: number, patch: Partial<ToastInput>): void {
	let found = false;
	store.update((list) =>
		list.map((t) => {
			if (t.id !== id) return t;
			found = true;
			return { ...t, ...patch };
		})
	);
	if (found && patch.ttlMs !== undefined) armTimer(id, patch.ttlMs);
}

export function dismiss(id: number): void {
	clearTimer(id);
	store.update((list) => list.filter((t) => t.id !== id));
}
