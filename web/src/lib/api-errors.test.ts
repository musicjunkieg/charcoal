import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	AccessRevokedError,
	ActionsDisabledError,
	AuthError,
	CooldownError,
	NotConnectedError,
	NotFoundError,
	getStatus
} from './api.js';

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

	it('throws NotConnectedError on 409 with code not_connected', async () => {
		mockFetch(409, { error: 'Connect your Bluesky account', code: 'not_connected' });
		await expect(getStatus()).rejects.toBeInstanceOf(NotConnectedError);
	});

	it('throws ActionsDisabledError on 503 with code actions_disabled', async () => {
		mockFetch(503, { error: 'Actions are not enabled', code: 'actions_disabled' });
		await expect(getStatus()).rejects.toBeInstanceOf(ActionsDisabledError);
	});

	// Pages branch on this: a genuine 404 means "gone", anything else means
	// "try again" — conflating them latches a live page into a dead end.
	it('throws NotFoundError on 404, carrying the server message', async () => {
		mockFetch(404, { error: 'Not found', code: 'not_found' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(NotFoundError);
		expect((err as NotFoundError).message).toBe('Not found');
	});

	it('throws a plain Error on a 500, not NotFoundError', async () => {
		mockFetch(500, { error: 'Something went wrong' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect(err).not.toBeInstanceOf(NotFoundError);
	});
});
