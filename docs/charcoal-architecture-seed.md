# Charcoal: Predictive Threat Interception for Bluesky

## Executive Summary

Charcoal is a defensive agent that protects Bluesky users from harassment by **predicting who is likely to see their posts and proactively muting/blocking potential threats before interaction occurs**. Unlike reactive moderation tools, Charcoal builds behavioral profiles across the network and uses shared intelligence to protect users preemptively.

### Core Value Proposition
- **Predictive, not reactive**: Block harassers before they see your content
- **Shared intelligence**: When User A blocks someone, User B benefits from that signal
- **Evidence-based**: Every action includes reasoning the user can review
- **User-controlled**: Confirm, escalate, or reverse any automated action

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                         CHARCOAL SYSTEM ARCHITECTURE                            │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  EXTERNAL DATA SOURCES                                                          │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐              │
│  │   Microcosm      │  │   Microcosm      │  │   Bluesky API    │              │
│  │   Constellation  │  │   Spacedust      │  │   (OAuth)        │              │
│  │   (Graph Index)  │  │   (Real-time)    │  │                  │              │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘              │
│           │                     │                     │                         │
│           └─────────────────────┼─────────────────────┘                         │
│                                 ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    CLOUDFLARE WORKERS BACKEND                            │   │
│  │                                                                          │   │
│  │   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │   │
│  │   │   OAuth     │  │  Profile    │  │   Threat    │  │   Action    │    │   │
│  │   │   Handler   │  │  Builder    │  │  Assessor   │  │  Executor   │    │   │
│  │   └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘    │   │
│  │                                                                          │   │
│  │   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                     │   │
│  │   │  Exposure   │  │  Spacedust  │  │    Cron     │                     │   │
│  │   │  Graph      │  │  Listener   │  │  Scheduler  │                     │   │
│  │   │  Builder    │  │  (Durable   │  │             │                     │   │
│  │   │             │  │   Object)   │  │             │                     │   │
│  │   └─────────────┘  └─────────────┘  └─────────────┘                     │   │
│  │                                                                          │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                 │                                               │
│                                 ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                         DATA STORES                                      │   │
│  │                                                                          │   │
│  │   ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐         │   │
│  │   │   D1 Database   │  │   KV Cache      │  │  Perspective    │         │   │
│  │   │   (SQLite)      │  │   (Hot data)    │  │  API (External) │         │   │
│  │   │                 │  │                 │  │                 │         │   │
│  │   │ • profiles      │  │ • profile cache │  │ • toxicity      │         │   │
│  │   │ • users         │  │ • session cache │  │   scoring       │         │   │
│  │   │ • actions       │  │ • rate limits   │  │                 │         │   │
│  │   │ • network_flags │  │                 │  │                 │         │   │
│  │   └─────────────────┘  └─────────────────┘  └─────────────────┘         │   │
│  │                                                                          │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                 │                                               │
│                                 ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐   │
│  │                    FRONTEND (Cloudflare Pages)                           │   │
│  │                                                                          │   │
│  │   • Dashboard: View automated actions with evidence                      │   │
│  │   • Controls: Confirm / Escalate to Block / Deescalate / Remove          │   │
│  │   • Settings: Adjust thresholds, topic preferences                       │   │
│  │                                                                          │   │
│  └─────────────────────────────────────────────────────────────────────────┘   │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Core Concepts

### 1. Exposure Graph

The exposure graph models **who is likely to see a protected user's posts** based on Bluesky's feed distribution mechanics:

| Tier | Definition | Exposure Probability | Data Source |
|------|------------|---------------------|-------------|
| **Tier 1** | Direct followers | ~100% (Following feed) | Constellation API |
| **Tier 2** | Followers-of-followers with topic overlap | ~30-50% (Discover feed "near your graph") | Constellation API + topic analysis |
| **Tier 3** | Users in same topic spaces, no direct connection | ~10-20% (Custom feeds, trending) | Global profile store |

### 2. Threat Score

Each DID in the exposure graph receives a threat score (0-100) composed of:

| Component | Weight | Source |
|-----------|--------|--------|
| **Toxicity Score** | 35% | Perspective API analysis of recent posts |
| **Topic Overlap** | 20% | Embedding similarity or keyword Jaccard |
| **Behavioral Signals** | 20% | Reply ratio, quote ratio, pile-on patterns |
| **Network Flags** | 25% | Blocks/flags from other Charcoal users (SHARED INTELLIGENCE) |

### 3. Shared Intelligence

The key differentiator: **user actions contribute to a global threat database**.

- When User A blocks @troll123, that block is recorded in @troll123's profile
- When User B's exposure graph includes @troll123, the network flag increases their threat score
- Users "near" each other in the graph (shared followers, topic overlap) amplify signals
- This creates a **herd immunity** effect without requiring individual users to encounter threats

---

## External API Integrations

### Microcosm Constellation (Graph Index)

Base URL: `https://constellation.microcosm.blue`

**Key Endpoints:**

```
GET /links/distinct-dids
  ?target={did}
  &collection=app.bsky.graph.follow
  &path=.subject
→ Returns all DIDs who follow this user

GET /links/all/count
  ?target={did}
→ Returns breakdown of all link types pointing at this DID

GET /links
  ?target={post_uri}
  &collection=app.bsky.feed.post
  &path=.embed.record.uri
→ Returns all quote posts of this post

GET /links
  ?target={post_uri}
  &collection=app.bsky.feed.like
  &path=.subject.uri
→ Returns all likes on this post
```

**Rate Limits:** Community infrastructure, be respectful. Include User-Agent header: `Charcoal/1.0 (@your-handle.bsky.social)`

### Microcosm Spacedust (Real-time Firehose)

Base URL: `wss://spacedust.microcosm.blue`

**WebSocket Subscription:**

```
/subscribe
  ?wantedSubjects={did}           // Filter to interactions targeting this DID
  &wantedSources=app.bsky.graph.follow:subject  // Filter to follow events
```

**Use Cases:**
- Real-time notification when someone new follows a protected user
- Real-time detection of quote posts targeting protected users
- Real-time detection of replies

### Bluesky API (via OAuth)

Base URL: Resolved per-user from their PDS

**Required OAuth Scopes (Granular - August 2025+):**

```javascript
const CHARCOAL_SCOPES = [
  // Read operations
  'rpc:app.bsky.graph.getFollows',
  'rpc:app.bsky.graph.getBlocks', 
  'rpc:app.bsky.graph.getMutes',
  'rpc:app.bsky.notification.listNotifications',
  'rpc:app.bsky.feed.getAuthorFeed',
  'rpc:app.bsky.actor.getProfile',
  
  // Write operations (protective actions)
  'repo:app.bsky.graph.block?create=true',
  'repo:app.bsky.graph.mute?create=true',
];
```

**Key Endpoints:**

```javascript
// Mute a user
await agent.app.bsky.graph.muteActor({ actor: targetDid });

// Block a user (creates a record)
await agent.app.bsky.graph.block.create(
  { repo: protectedUserDid },
  { subject: targetDid, createdAt: new Date().toISOString() }
);

// Get user's posts for analysis
await agent.app.bsky.feed.getAuthorFeed({ actor: targetDid, limit: 50 });

// Get notifications
await agent.app.bsky.notification.listNotifications({ limit: 50 });
```

### Google Perspective API (Toxicity Analysis)

Base URL: `https://commentanalyzer.googleapis.com/v1alpha1`

**Endpoint:**

```
POST /comments:analyze?key={API_KEY}

Body:
{
  "comment": { "text": "post content here" },
  "languages": ["en"],
  "requestedAttributes": {
    "TOXICITY": {},
    "SEVERE_TOXICITY": {},
    "IDENTITY_ATTACK": {},
    "INSULT": {},
    "THREAT": {}
  }
}

Response:
{
  "attributeScores": {
    "TOXICITY": {
      "summaryScore": { "value": 0.72 }
    },
    ...
  }
}
```

**Rate Limits:** Default 1 QPS (86,400/day). Request quota increase for production.

**Cost:** Free

---

## Data Models

### D1 Database Schema

```sql
-- Protected users (Charcoal subscribers)
CREATE TABLE protected_users (
  did TEXT PRIMARY KEY,
  handle TEXT,
  
  -- OAuth tokens (encrypted)
  access_token_encrypted TEXT NOT NULL,
  refresh_token_encrypted TEXT NOT NULL,
  token_expires_at INTEGER NOT NULL,
  pds_url TEXT NOT NULL,
  
  -- User's topic profile (for matching)
  topics TEXT, -- JSON array: ["fatlib", "acappella", "atlassian"]
  topic_embedding BLOB, -- Optional: vector for similarity search
  
  -- Thresholds (user-configurable)
  auto_mute_threshold INTEGER DEFAULT 60,
  auto_block_threshold INTEGER DEFAULT 85,
  
  -- Metadata
  created_at INTEGER NOT NULL,
  last_scan_at INTEGER,
  last_action_at INTEGER
);

-- Global profile store (all DIDs we've analyzed)
CREATE TABLE profiles (
  did TEXT PRIMARY KEY,
  handle TEXT,
  
  -- Toxicity analysis
  toxicity_score REAL, -- 0.0 to 1.0
  toxicity_breakdown TEXT, -- JSON: {toxicity: 0.7, identity_attack: 0.3, ...}
  toxicity_sample_count INTEGER DEFAULT 0,
  
  -- Topic analysis
  topics TEXT, -- JSON array of extracted topics
  topic_embedding BLOB, -- Optional: vector
  
  -- Behavioral signals
  reply_ratio REAL, -- replies / total posts
  quote_ratio REAL, -- quotes / total posts
  avg_thread_depth REAL,
  account_age_days INTEGER,
  follower_count INTEGER,
  following_count INTEGER,
  
  -- Network flags (THE SHARED INTELLIGENCE)
  blocked_by TEXT, -- JSON array of protected_user DIDs who blocked this person
  muted_by TEXT, -- JSON array of protected_user DIDs who muted this person
  flagged_by TEXT, -- JSON array of protected_user DIDs who flagged for review
  
  -- Metadata
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  post_sample_count INTEGER DEFAULT 0
);

CREATE INDEX idx_profiles_toxicity ON profiles(toxicity_score);
CREATE INDEX idx_profiles_updated ON profiles(updated_at);

-- Actions taken by Charcoal
CREATE TABLE actions (
  id TEXT PRIMARY KEY, -- UUID
  protected_user_did TEXT NOT NULL,
  target_did TEXT NOT NULL,
  
  -- Action details
  action_type TEXT NOT NULL, -- 'muted', 'blocked'
  trigger TEXT NOT NULL, -- 'auto', 'user_escalated', 'user_initiated'
  
  -- Scoring at time of action
  threat_score INTEGER NOT NULL,
  score_breakdown TEXT, -- JSON: {toxicity: 25, topic: 15, behavior: 10, network: 20}
  
  -- Evidence
  evidence TEXT, -- JSON: {sample_posts: [...], flags: [...]}
  
  -- User verdict (after review)
  user_verdict TEXT, -- 'confirmed', 'escalated', 'deescalated', 'removed', null
  user_verdict_at INTEGER,
  
  -- Metadata
  created_at INTEGER NOT NULL,
  executed_at INTEGER, -- null if pending
  
  FOREIGN KEY (protected_user_did) REFERENCES protected_users(did),
  FOREIGN KEY (target_did) REFERENCES profiles(did)
);

CREATE INDEX idx_actions_user ON actions(protected_user_did);
CREATE INDEX idx_actions_target ON actions(target_did);
CREATE INDEX idx_actions_pending ON actions(executed_at) WHERE executed_at IS NULL;

-- Exposure graph cache (rebuilt periodically)
CREATE TABLE exposure_graph (
  protected_user_did TEXT NOT NULL,
  exposed_did TEXT NOT NULL,
  
  tier INTEGER NOT NULL, -- 1, 2, or 3
  exposure_probability REAL NOT NULL,
  path TEXT, -- How they're connected: 'direct_follow', '2degree_via_did:plc:xxx', 'topic_overlap'
  
  -- Cached threat score
  threat_score INTEGER,
  score_calculated_at INTEGER,
  
  -- Metadata
  created_at INTEGER NOT NULL,
  
  PRIMARY KEY (protected_user_did, exposed_did)
);

CREATE INDEX idx_exposure_user ON exposure_graph(protected_user_did);
CREATE INDEX idx_exposure_threat ON exposure_graph(protected_user_did, threat_score DESC);
```

### KV Cache Schema

```
Key Pattern                          | Value                      | TTL
-------------------------------------|----------------------------|--------
profile:{did}                        | JSON profile object        | 24h
exposure:{protected_did}:built_at    | timestamp                  | 6h
session:{session_id}                 | JSON session data          | 7d
rate_limit:perspective:{minute}      | request count              | 60s
rate_limit:constellation:{minute}    | request count              | 60s
```

---

## Core Algorithms

### 1. Build Exposure Graph

```javascript
/**
 * Builds the exposure graph for a protected user.
 * This identifies everyone likely to see their posts.
 * 
 * @param protectedUserDid - The DID of the protected user
 * @returns ExposureGraph with tier1, tier2, tier3 arrays
 */
async function buildExposureGraph(protectedUserDid: string): Promise<ExposureGraph> {
  const protectedUser = await db.get('protected_users', protectedUserDid);
  const protectedTopics = JSON.parse(protectedUser.topics || '[]');
  
  const exposureGraph: ExposureGraph = {
    tier1: [],
    tier2: [],
    tier3: [],
    built_at: Date.now()
  };

  // TIER 1: Direct followers via Constellation
  const followers = await constellation.getDistinctDids({
    target: protectedUserDid,
    collection: 'app.bsky.graph.follow',
    path: '.subject'
  });
  
  for (const followerDid of followers) {
    exposureGraph.tier1.push({
      did: followerDid,
      exposure_probability: 1.0,
      path: 'direct_follow'
    });
  }

  // TIER 2: 2° network with topic overlap
  // Limit to top N followers by activity/relevance for POC
  const followerSample = exposureGraph.tier1.slice(0, 100);
  const tier2Seen = new Set(exposureGraph.tier1.map(t => t.did));
  tier2Seen.add(protectedUserDid);
  
  for (const follower of followerSample) {
    const secondDegree = await constellation.getDistinctDids({
      target: follower.did,
      collection: 'app.bsky.graph.follow',
      path: '.subject',
      limit: 200
    });
    
    for (const connectionDid of secondDegree) {
      if (tier2Seen.has(connectionDid)) continue;
      tier2Seen.add(connectionDid);
      
      // Get or build profile to check topic overlap
      const profile = await getOrBuildProfile(connectionDid);
      const connectionTopics = JSON.parse(profile.topics || '[]');
      const topicOverlap = calculateJaccardSimilarity(connectionTopics, protectedTopics);
      
      // Only include if meaningful topic overlap (likely to see via Discover)
      if (topicOverlap > 0.2) {
        exposureGraph.tier2.push({
          did: connectionDid,
          exposure_probability: 0.4 * topicOverlap,
          path: `2degree_via_${follower.did}`,
          topic_overlap: topicOverlap
        });
      }
    }
  }

  // TIER 3: Known hostile accounts with topic overlap (from global profiles)
  const hostileInTopics = await db.query(`
    SELECT did, toxicity_score, topics 
    FROM profiles 
    WHERE toxicity_score > 0.5 
    AND did NOT IN (${[...tier2Seen].map(() => '?').join(',')})
    ORDER BY toxicity_score DESC
    LIMIT 200
  `, [...tier2Seen]);
  
  for (const hostile of hostileInTopics) {
    const hostileTopics = JSON.parse(hostile.topics || '[]');
    const topicOverlap = calculateJaccardSimilarity(hostileTopics, protectedTopics);
    
    if (topicOverlap > 0.3) {
      exposureGraph.tier3.push({
        did: hostile.did,
        exposure_probability: 0.15 * topicOverlap,
        path: 'topic_overlap_hostile',
        precomputed_toxicity: hostile.toxicity_score
      });
    }
  }

  return exposureGraph;
}
```

### 2. Calculate Threat Score

```javascript
/**
 * Calculates the threat score for a DID relative to a protected user.
 * Incorporates toxicity, topic overlap, behavior, and SHARED NETWORK INTELLIGENCE.
 * 
 * @param targetDid - The DID being assessed
 * @param protectedUserDid - The protected user's DID
 * @param exposureEntry - Optional pre-computed exposure data
 * @returns ThreatAssessment with score and breakdown
 */
async function calculateThreatScore(
  targetDid: string,
  protectedUserDid: string,
  exposureEntry?: ExposureEntry
): Promise<ThreatAssessment> {
  const profile = await getOrBuildProfile(targetDid);
  const protectedUser = await db.get('protected_users', protectedUserDid);
  const protectedTopics = JSON.parse(protectedUser.topics || '[]');
  const targetTopics = JSON.parse(profile.topics || '[]');
  
  const breakdown: ScoreBreakdown = {
    toxicity: 0,
    topic_overlap: 0,
    behavior: 0,
    network: 0
  };

  // 1. TOXICITY SCORE (35% weight, max 35 points)
  breakdown.toxicity = Math.round((profile.toxicity_score || 0) * 35);

  // 2. TOPIC OVERLAP (20% weight, max 20 points)
  // Higher overlap = more likely to encounter content AND have opinions
  const topicOverlap = exposureEntry?.topic_overlap 
    ?? calculateJaccardSimilarity(targetTopics, protectedTopics);
  breakdown.topic_overlap = Math.round(topicOverlap * 20);

  // 3. BEHAVIORAL SIGNALS (20% weight, max 20 points)
  let behaviorScore = 0;
  
  // Reply guy pattern: >70% of posts are replies
  if ((profile.reply_ratio || 0) > 0.7) behaviorScore += 5;
  
  // Quote dunker pattern: >30% of posts are quotes
  if ((profile.quote_ratio || 0) > 0.3) behaviorScore += 5;
  
  // Pile-on participant: deep thread engagement
  if ((profile.avg_thread_depth || 0) > 5) behaviorScore += 4;
  
  // New account (< 30 days)
  if ((profile.account_age_days || 365) < 30) behaviorScore += 3;
  
  // Suspicious follower ratio (follows many, few follow back)
  const followerRatio = (profile.follower_count || 1) / (profile.following_count || 1);
  if (followerRatio < 0.1 && (profile.following_count || 0) > 100) behaviorScore += 3;
  
  breakdown.behavior = Math.min(behaviorScore, 20);

  // 4. NETWORK FLAGS - SHARED INTELLIGENCE (25% weight, max 25 points)
  let networkScore = 0;
  
  const blockedBy = JSON.parse(profile.blocked_by || '[]');
  const mutedBy = JSON.parse(profile.muted_by || '[]');
  const flaggedBy = JSON.parse(profile.flagged_by || '[]');
  
  // Base score from any Charcoal user blocking/muting
  networkScore += Math.min(blockedBy.length * 3, 10);
  networkScore += Math.min(mutedBy.length * 1.5, 5);
  networkScore += Math.min(flaggedBy.length * 2, 5);
  
  // BONUS: Extra weight if blocked by users NEAR this protected user
  const nearbyBlockers = await countNearbyBlockers(blockedBy, protectedUserDid);
  networkScore += nearbyBlockers * 2;
  
  breakdown.network = Math.min(Math.round(networkScore), 25);

  const totalScore = Math.min(
    breakdown.toxicity + breakdown.topic_overlap + breakdown.behavior + breakdown.network,
    100
  );

  return {
    score: totalScore,
    breakdown,
    profile_snapshot: {
      did: targetDid,
      handle: profile.handle,
      toxicity_score: profile.toxicity_score,
      topics: targetTopics,
      blocked_by_count: blockedBy.length,
      muted_by_count: mutedBy.length
    }
  };
}

/**
 * Determines if two users are "near" each other in the social graph.
 * Used to amplify network signals from nearby users.
 */
async function countNearbyBlockers(
  blockerDids: string[],
  protectedUserDid: string
): Promise<number> {
  if (blockerDids.length === 0) return 0;
  
  // Get protected user's followers
  const protectedFollowers = await constellation.getDistinctDids({
    target: protectedUserDid,
    collection: 'app.bsky.graph.follow',
    path: '.subject'
  });
  const protectedFollowerSet = new Set(protectedFollowers);
  
  let nearbyCount = 0;
  
  for (const blockerDid of blockerDids) {
    // Check if this blocker follows or is followed by the protected user
    if (protectedFollowerSet.has(blockerDid)) {
      nearbyCount++;
      continue;
    }
    
    // Check for shared followers (expensive, sample)
    const blockerFollowers = await constellation.getDistinctDids({
      target: blockerDid,
      collection: 'app.bsky.graph.follow',
      path: '.subject',
      limit: 100
    });
    
    const sharedFollowers = blockerFollowers.filter(f => protectedFollowerSet.has(f));
    if (sharedFollowers.length >= 5) {
      nearbyCount++;
    }
  }
  
  return nearbyCount;
}
```

### 3. Build Profile (Toxicity + Topics)

```javascript
/**
 * Builds or updates a profile for a DID.
 * Fetches recent posts, analyzes toxicity, extracts topics.
 * 
 * @param did - The DID to profile
 * @param forceRefresh - Skip cache and rebuild
 * @returns Profile object
 */
async function getOrBuildProfile(did: string, forceRefresh = false): Promise<Profile> {
  // Check cache first
  if (!forceRefresh) {
    const cached = await kv.get(`profile:${did}`);
    if (cached) return JSON.parse(cached);
    
    const stored = await db.get('profiles', did);
    if (stored && (Date.now() - stored.updated_at) < 24 * 60 * 60 * 1000) {
      await kv.put(`profile:${did}`, JSON.stringify(stored), { expirationTtl: 86400 });
      return stored;
    }
  }

  // Fetch recent posts via Bluesky API (unauthenticated for public data)
  const posts = await fetchPublicPosts(did, 50);
  
  if (posts.length === 0) {
    // No public posts, create minimal profile
    const minimalProfile: Profile = {
      did,
      handle: null,
      toxicity_score: null,
      topics: '[]',
      reply_ratio: null,
      quote_ratio: null,
      created_at: Date.now(),
      updated_at: Date.now()
    };
    await db.put('profiles', did, minimalProfile);
    return minimalProfile;
  }

  // Analyze toxicity via Perspective API (sample if too many)
  const textsToAnalyze = posts
    .slice(0, 20)
    .map(p => p.record.text)
    .filter(t => t && t.length > 10);
  
  const toxicityScores = await analyzeToxicityBatch(textsToAnalyze);
  const avgToxicity = toxicityScores.reduce((a, b) => a + b, 0) / toxicityScores.length;

  // Extract topics (simple keyword extraction for POC)
  const allText = posts.map(p => p.record.text).join(' ');
  const topics = extractTopics(allText);

  // Calculate behavioral signals
  const replyCount = posts.filter(p => p.record.reply).length;
  const quoteCount = posts.filter(p => p.record.embed?.record).length;
  
  // Get account metadata
  const profile = await fetchPublicProfile(did);

  const fullProfile: Profile = {
    did,
    handle: profile?.handle || null,
    toxicity_score: avgToxicity,
    toxicity_sample_count: textsToAnalyze.length,
    topics: JSON.stringify(topics),
    reply_ratio: replyCount / posts.length,
    quote_ratio: quoteCount / posts.length,
    account_age_days: profile ? daysSince(profile.createdAt) : null,
    follower_count: profile?.followersCount || null,
    following_count: profile?.followsCount || null,
    blocked_by: '[]',
    muted_by: '[]',
    flagged_by: '[]',
    created_at: Date.now(),
    updated_at: Date.now(),
    post_sample_count: posts.length
  };

  // Store and cache
  await db.put('profiles', did, fullProfile);
  await kv.put(`profile:${did}`, JSON.stringify(fullProfile), { expirationTtl: 86400 });

  return fullProfile;
}

/**
 * Batch toxicity analysis via Perspective API.
 * Respects 1 QPS rate limit.
 */
async function analyzeToxicityBatch(texts: string[]): Promise<number[]> {
  const scores: number[] = [];
  
  for (const text of texts) {
    await rateLimiter.wait('perspective');
    
    const response = await fetch(
      `https://commentanalyzer.googleapis.com/v1alpha1/comments:analyze?key=${PERSPECTIVE_API_KEY}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          comment: { text },
          languages: ['en'],
          requestedAttributes: {
            TOXICITY: {},
            SEVERE_TOXICITY: {},
            IDENTITY_ATTACK: {}
          }
        })
      }
    );
    
    const data = await response.json();
    const toxicity = data.attributeScores?.TOXICITY?.summaryScore?.value || 0;
    scores.push(toxicity);
  }
  
  return scores;
}

/**
 * Simple topic extraction via keyword frequency.
 * For POC - could be replaced with embeddings later.
 */
function extractTopics(text: string): string[] {
  const words = text.toLowerCase()
    .replace(/[^a-z0-9\s#]/g, '')
    .split(/\s+/)
    .filter(w => w.length > 3);
  
  // Extract hashtags
  const hashtags = text.match(/#\w+/g)?.map(h => h.toLowerCase()) || [];
  
  // Count word frequency
  const freq: Record<string, number> = {};
  for (const word of words) {
    if (STOP_WORDS.has(word)) continue;
    freq[word] = (freq[word] || 0) + 1;
  }
  
  // Get top keywords
  const topKeywords = Object.entries(freq)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 10)
    .map(([word]) => word);
  
  return [...new Set([...hashtags, ...topKeywords])].slice(0, 15);
}
```

### 4. Execute Protective Action

```javascript
/**
 * Executes a mute or block action on behalf of a protected user.
 * Records the action with evidence for user review.
 * 
 * @param protectedUserDid - The protected user
 * @param targetDid - The user to mute/block
 * @param actionType - 'mute' or 'block'
 * @param threatAssessment - The scoring that triggered this action
 */
async function executeProtectiveAction(
  protectedUserDid: string,
  targetDid: string,
  actionType: 'mute' | 'block',
  threatAssessment: ThreatAssessment
): Promise<void> {
  const protectedUser = await db.get('protected_users', protectedUserDid);
  
  // Create action record first (for audit trail)
  const actionId = crypto.randomUUID();
  const action: Action = {
    id: actionId,
    protected_user_did: protectedUserDid,
    target_did: targetDid,
    action_type: actionType === 'mute' ? 'muted' : 'blocked',
    trigger: 'auto',
    threat_score: threatAssessment.score,
    score_breakdown: JSON.stringify(threatAssessment.breakdown),
    evidence: JSON.stringify({
      profile_snapshot: threatAssessment.profile_snapshot,
      calculated_at: Date.now()
    }),
    user_verdict: null,
    user_verdict_at: null,
    created_at: Date.now(),
    executed_at: null
  };
  
  await db.put('actions', actionId, action);

  // Execute via OAuth
  const agent = await getAuthenticatedAgent(protectedUser);
  
  try {
    if (actionType === 'mute') {
      await agent.app.bsky.graph.muteActor({ actor: targetDid });
    } else {
      await agent.app.bsky.graph.block.create(
        { repo: protectedUserDid },
        { 
          subject: targetDid,
          createdAt: new Date().toISOString()
        }
      );
    }
    
    // Update action as executed
    await db.update('actions', actionId, { executed_at: Date.now() });
    
    // Update target's profile with network flag
    await addNetworkFlag(targetDid, protectedUserDid, actionType === 'mute' ? 'muted_by' : 'blocked_by');
    
  } catch (error) {
    // Log error but don't fail - action record exists for retry
    console.error(`Failed to execute ${actionType} for ${protectedUserDid} -> ${targetDid}:`, error);
    throw error;
  }
}

/**
 * Adds a network flag to a profile (the shared intelligence).
 */
async function addNetworkFlag(
  targetDid: string,
  flaggingUserDid: string,
  flagType: 'blocked_by' | 'muted_by' | 'flagged_by'
): Promise<void> {
  const profile = await db.get('profiles', targetDid);
  if (!profile) return;
  
  const flags = JSON.parse(profile[flagType] || '[]');
  if (!flags.includes(flaggingUserDid)) {
    flags.push(flaggingUserDid);
    await db.update('profiles', targetDid, { 
      [flagType]: JSON.stringify(flags),
      updated_at: Date.now()
    });
    
    // Invalidate cache
    await kv.delete(`profile:${targetDid}`);
  }
}
```

---

## API Routes

### Authentication

```
POST /api/auth/login
  → Initiates OAuth flow with Bluesky
  → Redirects to user's PDS authorization page
  → Callback stores tokens encrypted in D1

GET /api/auth/callback
  → Handles OAuth callback
  → Exchanges code for tokens
  → Creates/updates protected_user record

POST /api/auth/logout
  → Revokes tokens
  → Clears session
```

### User Management

```
GET /api/user/profile
  → Returns protected user's settings and stats

PATCH /api/user/settings
  → Updates thresholds, topics, preferences
  Body: { auto_mute_threshold: 60, auto_block_threshold: 85, topics: [...] }

POST /api/user/rescan
  → Triggers immediate exposure graph rebuild + threat scan
```

### Actions

```
GET /api/actions
  → Lists actions for the authenticated user
  Query: ?status=pending|executed&limit=50&cursor=...

GET /api/actions/:id
  → Get single action with full evidence

POST /api/actions/:id/verdict
  → User provides verdict on an action
  Body: { verdict: 'confirmed' | 'escalated' | 'deescalated' | 'removed' }
  
  - confirmed: Keeps action, strengthens network signal
  - escalated: If mute, upgrades to block
  - deescalated: If block, downgrades to mute
  - removed: Undoes action, removes network flag
```

### Dashboard Data

```
GET /api/dashboard/stats
  → Returns summary stats
  Response: {
    total_actions: 142,
    pending_review: 5,
    confirmed_rate: 0.89,
    threats_in_exposure: 23
  }

GET /api/dashboard/exposure
  → Returns exposure graph summary
  Response: {
    tier1_count: 456,
    tier2_count: 2341,
    tier3_count: 89,
    high_threat_count: 12,
    last_built_at: 1234567890
  }
```

---

## Worker Architecture

### Main Worker (API + Scheduled)

```javascript
// src/index.ts
export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext) {
    const url = new URL(request.url);
    
    // Route to appropriate handler
    if (url.pathname.startsWith('/api/auth')) {
      return handleAuth(request, env);
    }
    if (url.pathname.startsWith('/api/user')) {
      return handleUser(request, env);
    }
    if (url.pathname.startsWith('/api/actions')) {
      return handleActions(request, env);
    }
    if (url.pathname.startsWith('/api/dashboard')) {
      return handleDashboard(request, env);
    }
    
    // Serve static dashboard
    return env.ASSETS.fetch(request);
  },
  
  async scheduled(event: ScheduledEvent, env: Env, ctx: ExecutionContext) {
    switch (event.cron) {
      case '0 * * * *': // Every hour
        await rebuildExposureGraphs(env);
        break;
      case '*/15 * * * *': // Every 15 minutes
        await processThreats(env);
        break;
      case '0 0 * * *': // Daily
        await cleanupOldProfiles(env);
        break;
    }
  }
};
```

### Spacedust Listener (Durable Object)

```javascript
// src/spacedust-listener.ts
export class SpacedustListener implements DurableObject {
  private websocket: WebSocket | null = null;
  private protectedUsers: Set<string> = new Set();
  
  constructor(private state: DurableObjectState, private env: Env) {}
  
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    
    if (url.pathname === '/subscribe') {
      const did = url.searchParams.get('did');
      if (did) {
        this.protectedUsers.add(did);
        await this.ensureConnected();
      }
      return new Response('subscribed');
    }
    
    if (url.pathname === '/unsubscribe') {
      const did = url.searchParams.get('did');
      if (did) {
        this.protectedUsers.delete(did);
      }
      return new Response('unsubscribed');
    }
    
    return new Response('not found', { status: 404 });
  }
  
  private async ensureConnected() {
    if (this.websocket?.readyState === WebSocket.OPEN) return;
    
    const subjects = [...this.protectedUsers].join(',');
    const wsUrl = `wss://spacedust.microcosm.blue/subscribe?wantedSubjects=${subjects}`;
    
    this.websocket = new WebSocket(wsUrl);
    
    this.websocket.onmessage = async (event) => {
      const data = JSON.parse(event.data);
      await this.handleInteraction(data);
    };
    
    this.websocket.onclose = () => {
      // Reconnect after delay
      setTimeout(() => this.ensureConnected(), 5000);
    };
  }
  
  private async handleInteraction(data: SpacedustEvent) {
    // New follower detected
    if (data.collection === 'app.bsky.graph.follow') {
      const followerDid = data.source_did;
      const targetDid = data.target_did;
      
      if (this.protectedUsers.has(targetDid)) {
        // Queue profile analysis for new follower
        await this.env.PROFILE_QUEUE.send({
          type: 'new_follower',
          protected_user_did: targetDid,
          new_follower_did: followerDid
        });
      }
    }
    
    // Quote post detected
    if (data.collection === 'app.bsky.feed.post' && data.path === '.embed.record.uri') {
      const quoterDid = data.source_did;
      const quotedPostUri = data.target;
      
      // Check if quoted post belongs to a protected user
      const quotedDid = extractDidFromUri(quotedPostUri);
      if (this.protectedUsers.has(quotedDid)) {
        await this.env.PROFILE_QUEUE.send({
          type: 'quote_detected',
          protected_user_did: quotedDid,
          quoter_did: quoterDid,
          quoted_post_uri: quotedPostUri
        });
      }
    }
  }
}
```

---

## Configuration

### wrangler.toml

```toml
name = "charcoal"
main = "src/index.ts"
compatibility_date = "2024-01-01"

[vars]
PERSPECTIVE_API_KEY = "" # Set via secrets

[[d1_databases]]
binding = "DB"
database_name = "charcoal"
database_id = "xxx"

[[kv_namespaces]]
binding = "KV"
id = "xxx"

[[queues.producers]]
binding = "PROFILE_QUEUE"
queue = "profile-analysis"

[[queues.consumers]]
queue = "profile-analysis"
max_batch_size = 10
max_batch_timeout = 30

[[durable_objects.bindings]]
name = "SPACEDUST_LISTENER"
class_name = "SpacedustListener"

[[durable_objects.migrations]]
tag = "v1"
new_classes = ["SpacedustListener"]

[triggers]
crons = [
  "0 * * * *",      # Hourly: rebuild exposure graphs
  "*/15 * * * *",   # Every 15 min: process threat queue
  "0 0 * * *"       # Daily: cleanup old profiles
]
```

### Environment Variables / Secrets

```
PERSPECTIVE_API_KEY     - Google Perspective API key
ENCRYPTION_KEY          - For encrypting OAuth tokens at rest
SESSION_SECRET          - For signing session cookies
OAUTH_CLIENT_ID         - Bluesky OAuth client ID (from client-metadata.json)
```

---

## Implementation Phases

### Phase 1: Core Infrastructure (Days 1-3)

- [ ] Set up Cloudflare Workers project with D1 and KV
- [ ] Implement D1 schema migrations
- [ ] Create Constellation API client
- [ ] Create Perspective API client with rate limiting
- [ ] Implement basic profile building pipeline

### Phase 2: OAuth + User Management (Days 4-5)

- [ ] Implement OAuth flow with granular scopes
- [ ] Secure token storage (encryption at rest)
- [ ] User onboarding: fetch their posts, extract topics
- [ ] User settings API

### Phase 3: Exposure Graph + Threat Scoring (Days 6-8)

- [ ] Implement buildExposureGraph algorithm
- [ ] Implement calculateThreatScore algorithm
- [ ] Implement getOrBuildProfile with caching
- [ ] Add network flag tracking (shared intelligence)

### Phase 4: Action Execution (Days 9-10)

- [ ] Implement executeProtectiveAction
- [ ] Implement action reversal for user verdicts
- [ ] Set up cron jobs for periodic scanning
- [ ] Add action queue for reliability

### Phase 5: Real-time Monitoring (Day 11)

- [ ] Implement Spacedust Durable Object listener
- [ ] Handle new follower events
- [ ] Handle quote post events
- [ ] Connect to threat assessment pipeline

### Phase 6: Dashboard (Days 12-14)

- [ ] Build React/Svelte frontend on Cloudflare Pages
- [ ] Actions list with evidence display
- [ ] Verdict submission UI
- [ ] Settings panel
- [ ] Stats and exposure graph visualization

---

## Open Questions for Implementation

1. **Profile staleness**: How aggressively should we refresh profiles? Current plan is 24h TTL, but high-threat profiles might need more frequent updates.

2. **Tier 2 depth**: Should we go beyond followers-of-followers? 3° network gets exponentially larger but may catch more threats.

3. **Topic extraction**: Simple keyword extraction vs. embeddings? Embeddings are more accurate but require Workers AI or external API.

4. **Quote alert priority**: Quotes are high-signal harassment vectors. Should quote detection trigger immediate assessment rather than waiting for scheduled scan?

5. **Cold start**: For new users, should we pre-seed their exposure graph from existing Charcoal users with similar topics?

6. **Network flag decay**: Should old blocks/mutes decay in weight over time? A 2-year-old block may be less relevant.

7. **False positive handling**: When users mark an action as incorrect (removed verdict), should that negatively impact the network flag weight for other users?

---

## Testing Strategy

### Unit Tests
- Topic extraction accuracy
- Threat score calculation
- Jaccard similarity
- OAuth token refresh logic

### Integration Tests
- Constellation API responses
- Perspective API responses
- D1 queries
- Full profile build pipeline

### End-to-End Tests
- User signup flow
- Action execution and reversal
- Dashboard interactions

### Load Testing
- Profile building at scale (1000 DIDs)
- Exposure graph building for user with 10K followers
- Concurrent threat assessments

---

## Security Considerations

1. **Token Storage**: OAuth tokens encrypted at rest using AES-256-GCM
2. **Token Refresh**: Background job refreshes tokens before expiry
3. **Rate Limiting**: Per-user and global rate limits on all APIs
4. **Input Validation**: All DIDs validated against DID regex
5. **CORS**: Strict origin checking for dashboard
6. **Audit Logging**: All actions logged with timestamps and evidence

---

## Monitoring & Observability

- Cloudflare Analytics for request metrics
- Custom logging for action execution success/failure rates
- Alert on high error rates in profile building
- Track false positive rate from user verdicts
- Monitor Constellation/Spacedust availability

---

## Future Enhancements (Post-POC)

1. **Embeddings**: Replace keyword topics with proper embeddings for better matching
2. **Harassment cluster detection**: Graph analysis to identify coordinated harassment groups
3. **Custom feed analysis**: Determine which custom feeds a user subscribes to for better Tier 3 modeling
4. **Cross-platform intelligence**: Accept block lists from other platforms
5. **Community lists**: Let users share curated block lists with consent
6. **Appeal system**: Let blocked users request review
7. **Browser extension**: Show threat scores inline on Bluesky
