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
	PreSeedResponse,
	AccessListResponse,
	ApproveScanResponse,
	ActionsStatus,
	ActionBatchSummary,
	ActionBatchDetail,
	ActionRowView,
	CreateBatchResponse,
	ActionKind,
	ActiveActionRef
} from './types.js';

export class AuthError extends Error {
	constructor() {
		super('Authentication required');
		this.name = 'AuthError';
	}
}

/** 403 with code "access_revoked": the DID is no longer on the allowlist.
 *  Callers route to /waitlist instead of rendering a broken dashboard. */
export class AccessRevokedError extends Error {
	constructor() {
		super('Access is not currently active for this account');
		this.name = 'AccessRevokedError';
	}
}

/** 429 from POST /api/scan: the per-user cooldown (#258). Not an error state —
 *  the dashboard renders it as a calm notice with the retry instant. */
export class CooldownError extends Error {
	retry_at: string;
	constructor(message: string, retry_at: string) {
		super(message);
		this.name = 'CooldownError';
		this.retry_at = retry_at;
	}
}

/** 409 with code "not_connected": no write session for this account yet.
 *  Callers open the consent interstitial rather than showing an error. */
export class NotConnectedError extends Error {
	constructor() {
		super('Connect your Bluesky account before muting or blocking');
		this.name = 'NotConnectedError';
	}
}

/** 503 with code "actions_disabled": the server has no CHARCOAL_TOKEN_KEY.
 *  Buttons are hidden from status; this only fires if one slips through. */
export class ActionsDisabledError extends Error {
	constructor() {
		super('Mute and block actions are not enabled on this server');
		this.name = 'ActionsDisabledError';
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
		if (res.status === 403 && body.code === 'access_revoked') {
			throw new AccessRevokedError();
		}
		if (res.status === 429 && typeof body.retry_at === 'string') {
			throw new CooldownError(body.error ?? 'Scan cooldown active', body.retry_at);
		}
		if (res.status === 409 && body.code === 'not_connected') {
			throw new NotConnectedError();
		}
		if (res.status === 503 && body.code === 'actions_disabled') {
			throw new ActionsDisabledError();
		}
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

export interface HandleSuggestion {
	did: string;
	handle: string;
	display_name?: string;
	avatar?: string;
}

/**
 * Handle suggestions for the login screen (#227).
 *
 * Proxied through Charcoal's backend rather than called directly from the
 * browser, so the typeahead host never sees the user's IP or their
 * partially-typed handle on a pre-auth screen.
 *
 * Never throws: typeahead is an enhancement, and a failing one must not stop
 * anyone signing in. Errors — including an aborted request — resolve to [].
 */
export async function suggestHandles(
	query: string,
	signal?: AbortSignal
): Promise<HandleSuggestion[]> {
	try {
		const res = await fetch(`/api/typeahead?q=${encodeURIComponent(query)}`, { signal });
		if (!res.ok) return [];
		const data = await res.json();
		return Array.isArray(data) ? (data as HandleSuggestion[]) : [];
	} catch {
		return [];
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

// ---- Access (allowlist) ----

export async function getAccessRequests(): Promise<AccessListResponse> {
	return apiFetch<AccessListResponse>('/api/admin/access');
}

export async function grantAccess(
	handle: string
): Promise<{ did: string; handle: string; status: string }> {
	return apiFetch('/api/admin/access', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ handle })
	});
}

export async function approveAccess(did: string): Promise<void> {
	await apiFetch(`/api/admin/access/${encodeURIComponent(did)}/approve`, { method: 'POST' });
}

export async function approveAccessAndScan(did: string): Promise<ApproveScanResponse> {
	return apiFetch<ApproveScanResponse>(
		`/api/admin/access/${encodeURIComponent(did)}/approve-scan`,
		{ method: 'POST' }
	);
}

export async function denyAccess(did: string): Promise<void> {
	await apiFetch(`/api/admin/access/${encodeURIComponent(did)}/deny`, { method: 'POST' });
}

// ---- Mute / block actions (#315) ----

const JSON_HEADERS = { 'Content-Type': 'application/json' };

export async function getActionsStatus(): Promise<ActionsStatus> {
	return apiFetch<ActionsStatus>('/api/actions/status');
}

export interface ConnectOptions {
	tier?: string;
	handle?: string;
}

/** Begin the write-consent round-trip. Resolves to the PDS authorization URL;
 *  the caller navigates there with a full page load. */
export async function connectActions(
	kind: ActionKind | 'undo',
	opts: ConnectOptions = {}
): Promise<string> {
	const data = await apiFetch<{ redirect_url?: unknown }>('/api/actions/connect', {
		method: 'POST',
		headers: JSON_HEADERS,
		body: JSON.stringify({ kind, tier: opts.tier ?? null, handle: opts.handle ?? null })
	});
	if (typeof data.redirect_url !== 'string' || data.redirect_url.length === 0) {
		throw new Error('Could not start the Bluesky permission step — please try again');
	}
	return data.redirect_url;
}

/** Ask for consent and leave the page. Never resolves on success. */
export async function startConsent(kind: ActionKind | 'undo', opts: ConnectOptions = {}) {
	const url = await connectActions(kind, opts);
	window.location.assign(url);
}

export async function disconnectActions(): Promise<{ disconnected: boolean }> {
	return apiFetch('/api/actions/disconnect', { method: 'POST' });
}

export async function createActionBatch(
	kind: ActionKind,
	source: string,
	targets: string[]
): Promise<CreateBatchResponse> {
	return apiFetch<CreateBatchResponse>('/api/actions/batches', {
		method: 'POST',
		headers: JSON_HEADERS,
		body: JSON.stringify({ kind, source, targets })
	});
}

export async function listActionBatches(
	limit = 20,
	offset = 0
): Promise<{ batches: ActionBatchSummary[]; limit: number; offset: number }> {
	return apiFetch(`/api/actions/batches?limit=${limit}&offset=${offset}`);
}

export async function getActionBatch(id: number): Promise<ActionBatchDetail> {
	return apiFetch<ActionBatchDetail>(`/api/actions/batches/${id}`);
}

export async function undoBatch(id: number): Promise<{ batch_id: number }> {
	return apiFetch(`/api/actions/batches/${id}/undo`, { method: 'POST' });
}

export async function retryBatch(id: number): Promise<{ batch_id: number }> {
	return apiFetch(`/api/actions/batches/${id}/retry`, { method: 'POST' });
}

export async function undoAction(id: number): Promise<{ batch_id: number }> {
	return apiFetch(`/api/actions/${id}/undo`, { method: 'POST' });
}

export async function getAccountActions(
	handle: string
): Promise<{ did: string; actions: ActionRowView[] }> {
	return apiFetch(`/api/accounts/${encodeURIComponent(handle)}/actions`);
}

export async function getActiveActions(): Promise<{ active: ActiveActionRef[] }> {
	return apiFetch('/api/actions/active');
}
