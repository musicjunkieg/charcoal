import { afterEach, describe, expect, it, vi } from 'vitest';
import { AccessRevokedError, AuthError, CooldownError, getStatus } from './api.js';

function mockFetch(status: number, body: unknown) {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify(body), { status }))
	);
}

afterEach(() => vi.unstubAllGlobals());

describe('apiFetch error classification', () => {
	it('throws AuthError on 401', async () => {
		mockFetch(401, {});
		await expect(getStatus()).rejects.toBeInstanceOf(AuthError);
	});

	it('throws AccessRevokedError on 403 with code access_revoked', async () => {
		mockFetch(403, { error: 'Access is not currently active', code: 'access_revoked' });
		await expect(getStatus()).rejects.toBeInstanceOf(AccessRevokedError);
	});

	it('throws plain Error on a 403 without the code', async () => {
		mockFetch(403, { error: 'Admin required' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect(err).not.toBeInstanceOf(AccessRevokedError);
	});

	it('throws CooldownError on 429 with retry_at, preserving the instant', async () => {
		mockFetch(429, { error: 'Scan cooldown active', retry_at: '2026-08-26T12:00:00Z' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(CooldownError);
		expect((err as CooldownError).retry_at).toBe('2026-08-26T12:00:00Z');
	});

	it('throws plain Error on a 429 without retry_at', async () => {
		mockFetch(429, { error: 'Too many requests' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect(err).not.toBeInstanceOf(CooldownError);
	});
});
