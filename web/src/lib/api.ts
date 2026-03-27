// Typed API client for the Charcoal backend.
//
// All functions return the typed response or throw on network/auth errors.
// A 401 response throws AuthError — the caller should redirect to /login.

import type {
	ScanStatus,
	AccountsResponse,
	Account,
	EventsResponse,
	FingerprintResponse,
	UserLabel,
	AccuracyMetrics,
	ReviewResponse,
	Identity,
	AdminUsersResponse,
	PreSeedResponse
} from './types.js';

export class AuthError extends Error {
	constructor() {
		super('Authentication required');
		this.name = 'AuthError';
	}
}

function getAsUser(): string | null {
	if (typeof window === 'undefined') return null;
	return new URLSearchParams(window.location.search).get('as_user');
}

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
	const asUser = getAsUser();
	if (asUser) {
		const separator = path.includes('?') ? '&' : '?';
		path = `${path}${separator}as_user=${encodeURIComponent(asUser)}`;
	}
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

export async function initiateAuth(handle: string): Promise<string> {
	const res = await fetch('/api/auth/initiate', {
		method: 'POST',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ handle })
	});
	if (!res.ok) {
		const body = await res.json().catch(() => ({}));
		throw new Error(body.error ?? 'Sign-in failed — please try again');
	}
	const data = (await res.json()) as { redirect_url?: unknown };
	if (typeof data.redirect_url !== 'string' || data.redirect_url.length === 0) {
		throw new Error('Sign-in failed — invalid OAuth redirect response');
	}
	return data.redirect_url;
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

// ---- Labels ----

export async function labelAccount(
	did: string,
	label: string,
	notes?: string
): Promise<UserLabel> {
	return apiFetch<UserLabel>(`/api/accounts/${encodeURIComponent(did)}/label`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ label, notes: notes ?? null })
	});
}

// ---- Review Queue ----

export async function getReviewQueue(limit = 20): Promise<ReviewResponse> {
	return apiFetch<ReviewResponse>(`/api/review?limit=${limit}`);
}

// ---- Accuracy ----

export async function getAccuracy(): Promise<AccuracyMetrics> {
	return apiFetch<AccuracyMetrics>('/api/accuracy');
}

// ---- Identity ----

export async function getIdentity(): Promise<Identity> {
	return apiFetch<Identity>('/api/me');
}

// ---- Admin ----

export async function getAdminUsers(): Promise<AdminUsersResponse> {
	return apiFetch<AdminUsersResponse>('/api/admin/users');
}

export async function preSeedUser(handle: string): Promise<PreSeedResponse> {
	return apiFetch<PreSeedResponse>('/api/admin/users', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ handle }),
	});
}

export async function triggerAdminScan(did: string): Promise<void> {
	await apiFetch(`/api/admin/users/${encodeURIComponent(did)}/scan`, {
		method: 'POST',
	});
}

export async function deleteAdminUser(did: string): Promise<void> {
	await apiFetch(`/api/admin/users/${encodeURIComponent(did)}`, {
		method: 'DELETE',
	});
}
