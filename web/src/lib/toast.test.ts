import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import { toasts, raise, update, dismiss, MAX_TOASTS } from './toast.js';

// The store is module-level state; every test starts from empty.
function clearAll() {
	for (const t of get(toasts)) dismiss(t.id);
}

describe('toast store', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		clearAll();
	});
	afterEach(() => {
		clearAll();
		vi.useRealTimers();
	});

	it('raise appends newest last and returns a unique id', () => {
		const a = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		const b = raise({ tone: 'working', text: 'Muting @b…', actions: [] });
		expect(a).not.toBe(b);
		expect(get(toasts).map((t) => t.id)).toEqual([a, b]);
		expect(get(toasts)[1].text).toBe('Muting @b…');
	});

	it('update patches in place and keeps the id', () => {
		const id = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		update(id, { tone: 'ok', text: 'Muted @a', href: { label: 'Record', url: '/actions/7' } });
		const t = get(toasts)[0];
		expect(t.id).toBe(id);
		expect(t.tone).toBe('ok');
		expect(t.text).toBe('Muted @a');
		expect(t.href).toEqual({ label: 'Record', url: '/actions/7' });
	});

	it('update of an unknown id is a no-op', () => {
		raise({ tone: 'ok', text: 'x', actions: [] });
		update(999, { text: 'y' });
		expect(get(toasts).map((t) => t.text)).toEqual(['x']);
	});

	it('dismiss removes only that toast', () => {
		const a = raise({ tone: 'ok', text: 'a', actions: [] });
		const b = raise({ tone: 'ok', text: 'b', actions: [] });
		dismiss(a);
		expect(get(toasts).map((t) => t.id)).toEqual([b]);
	});

	it('a raise without ttlMs never auto-dismisses', () => {
		raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		vi.advanceTimersByTime(600_000);
		expect(get(toasts)).toHaveLength(1);
	});

	it('ttl starts when ttlMs is set on the settled update, not on raise', () => {
		const id = raise({ tone: 'working', text: 'Muting @a…', actions: [] });
		vi.advanceTimersByTime(30_000);
		update(id, { tone: 'ok', text: 'Muted @a', ttlMs: 6000 });
		vi.advanceTimersByTime(5999);
		expect(get(toasts)).toHaveLength(1);
		vi.advanceTimersByTime(1);
		expect(get(toasts)).toHaveLength(0);
	});

	it('ttlMs on raise dismisses after the ttl', () => {
		raise({ tone: 'ok', text: 'Muted @a', actions: [], ttlMs: 1000 });
		vi.advanceTimersByTime(1000);
		expect(get(toasts)).toHaveLength(0);
	});

	it('a second update with ttlMs restarts the timer', () => {
		const id = raise({ tone: 'ok', text: 'a', actions: [], ttlMs: 1000 });
		vi.advanceTimersByTime(900);
		update(id, { text: 'b', ttlMs: 1000 });
		vi.advanceTimersByTime(900);
		expect(get(toasts)).toHaveLength(1);
		vi.advanceTimersByTime(100);
		expect(get(toasts)).toHaveLength(0);
	});

	it('dismiss cancels a pending ttl timer (no double removal of a reused id)', () => {
		const id = raise({ tone: 'ok', text: 'a', actions: [], ttlMs: 1000 });
		dismiss(id);
		const b = raise({ tone: 'ok', text: 'b', actions: [] });
		vi.advanceTimersByTime(1000);
		expect(get(toasts).map((t) => t.id)).toEqual([b]);
	});

	it(`keeps at most ${MAX_TOASTS} toasts, dropping the oldest`, () => {
		const ids = [1, 2, 3, 4].map((i) => raise({ tone: 'ok', text: `t${i}`, actions: [] }));
		expect(get(toasts)).toHaveLength(MAX_TOASTS);
		expect(get(toasts).map((t) => t.id)).toEqual(ids.slice(1));
	});

	it('a toast evicted for capacity does not leave a stale ttl timer running', () => {
		// The evicted toast carried a ttl; `raise` must clear its timer on
		// eviction (not just on `dismiss`), or the timer fires later and
		// touches a store that no longer has that id.
		raise({ tone: 'ok', text: 'evicted', actions: [], ttlMs: 1000 });
		const survivors = [1, 2, 3].map((i) => raise({ tone: 'ok', text: `t${i}`, actions: [] }));
		expect(get(toasts).map((t) => t.id)).toEqual(survivors);
		// Only the evicted toast had a ttl, so a leaked timer is the ONLY
		// thing that could still be pending here. Asserting on store
		// contents alone can't catch the leak: ids are never reused and
		// `dismiss` of an unknown id is a no-op, so the store looks the
		// same either way.
		expect(vi.getTimerCount()).toBe(0);
		vi.advanceTimersByTime(1000);
		expect(get(toasts)).toHaveLength(MAX_TOASTS);
		expect(get(toasts).map((t) => t.id)).toEqual(survivors);
	});
});
