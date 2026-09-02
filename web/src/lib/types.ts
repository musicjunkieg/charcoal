// TypeScript interfaces matching the Charcoal API JSON shapes.

export interface ToxicPost {
	text: string;
	toxicity: number;
	uri: string; // bsky.app URL (converted from AT-URI by server)
}

export interface BehavioralSignals {
	quote_ratio?: number;
	reply_ratio?: number;
	avg_engagement?: number;
	is_pile_on_participant?: boolean;
	benign_gate_applied?: boolean;
	hostile_multiplier?: number;
}

export interface Account {
	rank: number;
	did: string;
	handle: string;
	toxicity_score: number | null;
	topic_overlap: number | null;
	threat_score: number | null;
	threat_tier: string | null; // "High" | "Elevated" | "Watch" | "Low" | null
	posts_analyzed: number;
	top_toxic_posts: ToxicPost[];
	scored_at: string;
	behavioral_signals: BehavioralSignals | null;
}

export interface TierCounts {
	high: number;
	elevated: number;
	watch: number;
	low: number;
	// Accounts whose posts couldn't be scored — unsupported language (#222
	// language abstention). Excluded from `total`, so it's a distinct bucket
	// rather than a threat tier.
	not_assessed: number;
	total: number;
}

// Coarse scan stage from the backend. The setup stages come from the
// in-memory scan job; gathering/classifying/finalizing are refined from
// pipeline state while the heavy scoring stage runs.
export type ScanPhase =
	| 'idle'
	// Enqueued, waiting for a free scan slot — nothing is running yet (#257).
	| 'queued'
	| 'starting'
	| 'loading_models'
	| 'fingerprint'
	| 'discovering'
	| 'scoring'
	| 'gathering'
	| 'classifying'
	| 'finalizing'
	| 'done'
	| 'failed';

// Live progress counts; null while a stage hasn't recorded its denominator.
export interface ScanProgress {
	candidates_total: number | null;
	classifications_total: number | null;
	classifications_done: number | null;
}

// Where the user sits in the scan queue. `eta_seconds` is a rolling median of
// recent completed scans and is null when there is nothing to median from —
// an absent estimate, never a fabricated one (#257).
export interface QueuePosition {
	position: number;
	eta_seconds: number | null;
	enqueued_at: string;
}

export interface ScanStatus {
	scan_running: boolean;
	started_at: string | null;
	progress_message: string;
	last_error: string | null;
	phase: ScanPhase;
	progress: ScanProgress | null;
	tier_counts: TierCounts;
	/** Present only while queued (#257); the server omits it otherwise. */
	queue?: QueuePosition;
}

export interface AmplificationEvent {
	id: number;
	event_type: string;
	amplifier_did: string;
	amplifier_handle: string;
	original_post_uri: string;
	amplifier_post_uri: string | null; // bsky.app URL
	amplifier_text: string | null;
	detected_at: string;
}

export interface AccountsResponse {
	accounts: Account[];
	total: number;
	page: number;
	per_page: number;
}

export interface EventsResponse {
	events: AmplificationEvent[];
}

// Matches the serialized TopicFingerprint returned by GET /api/fingerprint.
export interface TopicCluster {
	label: string;
	keywords: string[];
	weight: number;
}

export interface FingerprintResponse {
	fingerprint: {
		clusters: TopicCluster[];
		post_count: number;
	} | null;
	post_count: number;
	updated_at: string;
}

export interface UserLabel {
	user_did: string;
	target_did: string;
	label: 'high' | 'elevated' | 'watch' | 'safe';
	labeled_at: string;
	notes: string | null;
	predicted_tier: string | null;
}

export interface AccuracyMetrics {
	total_labeled: number;
	exact_matches: number;
	overscored: number;
	underscored: number;
	accuracy: number;
}

export interface ReviewAccount {
	did: string;
	handle: string;
	toxicity_score: number | null;
	topic_overlap: number | null;
	threat_score: number | null;
	threat_tier: string | null;
	posts_analyzed: number;
	scored_at: string | null;
	context_score: number | null;
}

export interface ReviewResponse {
	accounts: ReviewAccount[];
	total: number;
}

/** A row of `scan_queue` as the admin surface sees it (#288). Durable — it
 *  survives a restart and sees other replicas, unlike the process-local
 *  `ScanManager` this replaced. */
export interface AdminScanRow {
	user_did: string;
	/** null when the queue row outlived its user record — an orphaned row,
	 *  which is deliberately still listed because an operator needs to see it. */
	handle: string | null;
	status: 'queued' | 'running' | 'done' | 'failed';
	/** 1-based among queued rows; 0 for every other status. */
	position: number;
	enqueued_at: string;
	started_at: string | null;
	finished_at: string | null;
	last_error: string | null;
}

export interface AdminQueue {
	running: number;
	queued: number;
	concurrency_limit: number;
	/** queued + running only, oldest first — the order the queue drains in.
	 *  Empty array (never null) when nothing is active. */
	active: AdminScanRow[];
}

export interface AdminUser {
	did: string;
	handle: string;
	has_fingerprint: boolean;
	/** Still process-local, and correctly so: a fingerprint build is not a
	 *  queued job. */
	fingerprint_building: boolean;
	/** When the most recent scan STARTED. null while a scan is merely queued —
	 *  a queued scan has not started. Read `scan.status` for that case. */
	last_scan_at: string | null;
	scored_accounts: number;
	last_login_at: string | null;
	/** null when the user has never been enqueued — the distinction the old
	 *  dashboard could not draw. */
	scan: AdminScanRow | null;
}

export interface AdminUsersResponse {
	users: AdminUser[];
	queue: AdminQueue;
}

export interface Identity {
	did: string;
	handle: string;
	is_admin: boolean;
	/** False when CHARCOAL_ALLOWED_DID is unset (open access) — access-table
	 *  decisions are inert in that mode and the admin UI warns about it. */
	access_gate_active: boolean;
}

export interface PreSeedResponse {
	did: string;
	handle: string;
}

export interface AccessRequest {
	did: string;
	handle: string;
	status: 'pending' | 'allowed' | 'denied';
	requested_at: string;
	decided_at: string | null;
	decided_by: string | null;
}

export interface AccessListResponse {
	pending: AccessRequest[];
	allowed: AccessRequest[];
	denied: AccessRequest[];
}

export interface ApproveScanResponse {
	did: string;
	access: string;
	/** "queued" on full success; anything else is an honest partial failure. */
	scan: string;
}

// ---- Mute / block actions (#315) ----

export type ActionKind = 'mute' | 'block';
export type ActionBatchKind = ActionKind | 'undo';
export type ActionBatchStatus = 'queued' | 'running' | 'done' | 'partial' | 'failed';
export type ActionRowStatus =
	| 'pending'
	| 'applied'
	| 'skipped_already_done'
	| 'failed'
	| 'undone';

export interface ActionsStatus {
	enabled: boolean;
	connected: boolean;
	scope?: string;
	pds_url?: string;
	connected_at?: string;
}

export interface ActionBatchSummary {
	id: number;
	kind: ActionBatchKind;
	source: string;
	requested: number;
	status: ActionBatchStatus;
	error: string | null;
	created_at: string;
	started_at: string | null;
	finished_at: string | null;
	counts: Partial<Record<ActionRowStatus, number>>;
	drifted: boolean;
}

export interface ActionRowView {
	id: number;
	batch_id: number;
	target_did: string;
	handle: string | null;
	kind: ActionKind;
	status: ActionRowStatus;
	record_uri: string | null;
	undo_of: number | null;
	error: string | null;
	score_at_action: number | null;
	tier_at_action: string | null;
	current_tier: string | null;
	drifted: boolean;
	applied_at: string | null;
	undone_at: string | null;
}

export interface ActionBatchDetail {
	batch: ActionBatchSummary;
	actions: ActionRowView[];
}

export interface CreateBatchResponse {
	batch_id: number | null;
	requested: number;
	skipped_active: number;
}

/** One active mute/block Charcoal currently holds, from GET /api/actions/active. */
export interface ActiveActionRef {
	did: string;
	kind: ActionKind;
}

/** One row of the bulk confirm sheet's account list (spec §5.1). */
export interface SheetRow {
	did: string;
	handle: string;
	tier: string | null;
	/** Plain-language top signal — never a bare number (PRODUCT principle 1). */
	signal: string;
	/** Charcoal already holds this kind on this account: greyed, unchecked, not counted. */
	done: boolean;
}
