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
	total: number;
}

export interface ScanStatus {
	scan_running: boolean;
	started_at: string | null;
	progress_message: string;
	last_error: string | null;
	tier_counts: TierCounts;
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

export interface FingerprintResponse {
	fingerprint: {
		keywords: Array<{ term: string; weight: number }>;
		post_count: number;
	} | null;
	post_count: number;
	updated_at: string;
}
