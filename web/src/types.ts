export interface ToxicPost {
  text: string;
  toxicity: number;
  uri: string;
}

export interface AccountScore {
  did: string;
  handle: string;
  toxicity_score: number | null;
  topic_overlap: number | null;
  threat_score: number | null;
  threat_tier: string | null;
  posts_analyzed: number;
  top_toxic_posts: ToxicPost[];
  scored_at: string;
}

export interface AmplificationEvent {
  id: number;
  event_type: string;
  amplifier_did: string;
  amplifier_handle: string;
  original_post_uri: string;
  amplifier_post_uri: string | null;
  amplifier_text: string | null;
  detected_at: string;
  followers_fetched: boolean;
  followers_scored: boolean;
}

export interface CharcoalExport {
  accounts: AccountScore[];
  events: AmplificationEvent[];
  exported_at: string;
  total_accounts: number;
  total_events: number;
}

export type SortField = "handle" | "threat_score" | "toxicity_score" | "topic_overlap" | "posts_analyzed";
export type SortDir = "asc" | "desc";
