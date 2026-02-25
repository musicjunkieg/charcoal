// Typed API client for the Charcoal backend.
//
// All functions return the typed response or throw on network/auth errors.
// A 401 response throws AuthError — the caller should redirect to /login.

import type {
	ScanStatus,
	AccountsResponse,
	Account,
	EventsResponse,
	FingerprintResponse
} from './types.js';

export class AuthError extends Error {
	constructor() {
		super('Authentication required');
		this.name = 'AuthError';
	}
}

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
	const res = await fetch(path, {
		credentials: 'include', // send session cookie
		...options
	});
	if (res.status === 401) {
		throw new AuthError();
	}
	if (!res.ok) {
		const body = await res.json().catch(() => ({}));
		throw new Error(body.error ?? `HTTP ${res.status}`);
	}
	return res.json() as Promise<T>;
}

// ---- Auth ----

export async function login(password: string): Promise<void> {
	const res = await fetch('/api/login', {
		method: 'POST',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ password })
	});
	if (res.status === 401) {
		throw new Error('Invalid password');
	}
	if (!res.ok) {
		throw new Error('Login failed');
	}
}

export async function logout(): Promise<void> {
	await fetch('/api/logout', { method: 'POST', credentials: 'include' });
}

// ---- Status ----

export async function getStatus(): Promise<ScanStatus> {
	return apiFetch<ScanStatus>('/api/status');
}

// ---- Scan ----

export async function triggerScan(): Promise<void> {
	await apiFetch('/api/scan', { method: 'POST' });
}

// ---- Accounts ----

export async function getAccounts(params?: {
	tier?: string;
	q?: string;
	page?: number;
	per_page?: number;
}): Promise<AccountsResponse> {
	const qs = new URLSearchParams();
	if (params?.tier) qs.set('tier', params.tier);
	if (params?.q) qs.set('q', params.q);
	if (params?.page) qs.set('page', String(params.page));
	if (params?.per_page) qs.set('per_page', String(params.per_page));
	const query = qs.toString() ? `?${qs}` : '';
	return apiFetch<AccountsResponse>(`/api/accounts${query}`);
}

export async function getAccount(handle: string): Promise<Account> {
	return apiFetch<Account>(`/api/accounts/${encodeURIComponent(handle)}`);
}

// ---- Events ----

export async function getEvents(limit = 20): Promise<EventsResponse> {
	return apiFetch<EventsResponse>(`/api/events?limit=${limit}`);
}

// ---- Fingerprint ----

export async function getFingerprint(): Promise<FingerprintResponse> {
	return apiFetch<FingerprintResponse>('/api/fingerprint');
}
