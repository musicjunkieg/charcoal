import { afterEach, describe, expect, it, vi } from 'vitest';
import { AccessRevokedError, AuthError, getStatus } from './api.js';

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
});
