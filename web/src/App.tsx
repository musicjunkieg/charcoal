import { useState, useMemo, useCallback } from "react";
import type { CharcoalExport, AccountScore, SortField, SortDir } from "./types";

const TIER_COLORS: Record<string, string> = {
  High: "#dc2626",
  Elevated: "#ea580c",
  Watch: "#ca8a04",
  Low: "#16a34a",
};

const TIERS = ["High", "Elevated", "Watch", "Low"] as const;

function TierBadge({ tier }: { tier: string | null }) {
  const t = tier ?? "?";
  return (
    <span
      style={{
        color: "#fff",
        backgroundColor: TIER_COLORS[t] ?? "#666",
        padding: "2px 8px",
        borderRadius: 4,
        fontSize: 12,
        fontWeight: 600,
      }}
    >
      {t}
    </span>
  );
}

function SortHeader({
  label,
  field,
  current,
  dir,
  onSort,
}: {
  label: string;
  field: SortField;
  current: SortField;
  dir: SortDir;
  onSort: (f: SortField) => void;
}) {
  const arrow = current === field ? (dir === "asc" ? " ▲" : " ▼") : "";
  return (
    <th
      style={{ cursor: "pointer", userSelect: "none", padding: "8px 12px", textAlign: "left" }}
      onClick={() => onSort(field)}
    >
      {label}{arrow}
    </th>
  );
}

function AccountRow({
  account,
  expanded,
  onToggle,
}: {
  account: AccountScore;
  expanded: boolean;
  onToggle: () => void;
}) {
  return (
    <>
      <tr
        onClick={onToggle}
        style={{ cursor: "pointer", borderBottom: expanded ? "none" : "1px solid #e5e7eb" }}
      >
        <td style={{ padding: "8px 12px" }}>@{account.handle}</td>
        <td style={{ padding: "8px 12px", textAlign: "right" }}>
          {account.threat_score?.toFixed(1) ?? "—"}
        </td>
        <td style={{ padding: "8px 12px" }}>
          <TierBadge tier={account.threat_tier} />
        </td>
        <td style={{ padding: "8px 12px", textAlign: "right" }}>
          {account.toxicity_score?.toFixed(3) ?? "—"}
        </td>
        <td style={{ padding: "8px 12px", textAlign: "right" }}>
          {account.topic_overlap?.toFixed(3) ?? "—"}
        </td>
        <td style={{ padding: "8px 12px", textAlign: "right" }}>{account.posts_analyzed}</td>
      </tr>
      {expanded && (
        <tr style={{ backgroundColor: "#f9fafb" }}>
          <td colSpan={6} style={{ padding: "12px 24px" }}>
            <div style={{ fontSize: 13 }}>
              <strong>DID:</strong> {account.did}
              <br />
              <strong>Scored:</strong> {account.scored_at}
            </div>
            {account.top_toxic_posts.length > 0 && (
              <div style={{ marginTop: 8 }}>
                <strong>Most toxic posts (evidence):</strong>
                <ul style={{ margin: "4px 0", paddingLeft: 20 }}>
                  {account.top_toxic_posts.map((post, i) => (
                    <li key={i} style={{ marginBottom: 4, fontSize: 13 }}>
                      <span style={{ color: "#dc2626", fontWeight: 600 }}>
                        [{post.toxicity.toFixed(3)}]
                      </span>{" "}
                      {post.text.length > 200 ? post.text.slice(0, 200) + "..." : post.text}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {account.top_toxic_posts.length === 0 && (
              <div style={{ marginTop: 8, color: "#888", fontSize: 13 }}>
                No toxic posts recorded.
              </div>
            )}
          </td>
        </tr>
      )}
    </>
  );
}

export default function App() {
  const [data, setData] = useState<CharcoalExport | null>(null);
  const [search, setSearch] = useState("");
  const [tierFilter, setTierFilter] = useState<Set<string>>(new Set(TIERS));
  const [sortField, setSortField] = useState<SortField>("threat_score");
  const [sortDir, setSortDir] = useState<SortDir>("desc");
  const [expandedDid, setExpandedDid] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleFile = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setError(null);
    const reader = new FileReader();
    reader.onload = (ev) => {
      try {
        const parsed = JSON.parse(ev.target?.result as string) as CharcoalExport;
        if (!parsed.accounts || !Array.isArray(parsed.accounts)) {
          setError("Invalid export file: missing 'accounts' array");
          return;
        }
        setData(parsed);
      } catch {
        setError("Failed to parse JSON file");
      }
    };
    reader.readAsText(file);
  }, []);

  const handleSort = useCallback(
    (field: SortField) => {
      if (field === sortField) {
        setSortDir((d) => (d === "asc" ? "desc" : "asc"));
      } else {
        setSortField(field);
        setSortDir("desc");
      }
    },
    [sortField]
  );

  const toggleTier = useCallback((tier: string) => {
    setTierFilter((prev) => {
      const next = new Set(prev);
      if (next.has(tier)) next.delete(tier);
      else next.add(tier);
      return next;
    });
  }, []);

  const filtered = useMemo(() => {
    if (!data) return [];
    let accounts = data.accounts;

    // Search
    if (search) {
      const q = search.toLowerCase();
      accounts = accounts.filter((a) => a.handle.toLowerCase().includes(q));
    }

    // Tier filter
    accounts = accounts.filter((a) => tierFilter.has(a.threat_tier ?? "Low"));

    // Sort
    accounts = [...accounts].sort((a, b) => {
      let av: number | string;
      let bv: number | string;
      if (sortField === "handle") {
        av = a.handle.toLowerCase();
        bv = b.handle.toLowerCase();
      } else {
        av = a[sortField] ?? 0;
        bv = b[sortField] ?? 0;
      }
      if (av < bv) return sortDir === "asc" ? -1 : 1;
      if (av > bv) return sortDir === "asc" ? 1 : -1;
      return 0;
    });

    return accounts;
  }, [data, search, tierFilter, sortField, sortDir]);

  const tierCounts = useMemo(() => {
    if (!data) return {};
    const counts: Record<string, number> = {};
    for (const a of data.accounts) {
      const t = a.threat_tier ?? "Low";
      counts[t] = (counts[t] || 0) + 1;
    }
    return counts;
  }, [data]);

  // Landing screen
  if (!data) {
    return (
      <div style={{ fontFamily: "system-ui, sans-serif", maxWidth: 600, margin: "80px auto", textAlign: "center" }}>
        <h1 style={{ fontSize: 28, marginBottom: 8 }}>Charcoal Threat Viewer</h1>
        <p style={{ color: "#666", marginBottom: 24 }}>
          Load an export file to view scored accounts.
          <br />
          Generate one with: <code style={{ backgroundColor: "#f3f4f6", padding: "2px 6px", borderRadius: 4 }}>make export</code>
        </p>
        <label
          style={{
            display: "inline-block",
            padding: "12px 24px",
            backgroundColor: "#2563eb",
            color: "#fff",
            borderRadius: 8,
            cursor: "pointer",
            fontSize: 16,
            fontWeight: 600,
          }}
        >
          Load charcoal-export.json
          <input type="file" accept=".json" onChange={handleFile} style={{ display: "none" }} />
        </label>
        {error && <p style={{ color: "#dc2626", marginTop: 16 }}>{error}</p>}
      </div>
    );
  }

  return (
    <div style={{ fontFamily: "system-ui, sans-serif", maxWidth: 1100, margin: "0 auto", padding: 24 }}>
      <h1 style={{ fontSize: 24, marginBottom: 4 }}>Charcoal Threat Viewer</h1>
      <p style={{ color: "#666", fontSize: 14, marginBottom: 16 }}>
        Exported: {data.exported_at} &middot; {data.total_accounts} accounts &middot; {data.total_events} events
      </p>

      {/* Summary */}
      <div style={{ display: "flex", gap: 12, marginBottom: 20 }}>
        {TIERS.map((tier) => (
          <div
            key={tier}
            style={{
              padding: "8px 16px",
              borderRadius: 8,
              backgroundColor: tierFilter.has(tier) ? TIER_COLORS[tier] + "18" : "#f3f4f6",
              border: `2px solid ${tierFilter.has(tier) ? TIER_COLORS[tier] : "#e5e7eb"}`,
              cursor: "pointer",
              textAlign: "center",
              minWidth: 80,
            }}
            onClick={() => toggleTier(tier)}
          >
            <div style={{ fontSize: 22, fontWeight: 700, color: TIER_COLORS[tier] }}>
              {tierCounts[tier] || 0}
            </div>
            <div style={{ fontSize: 12, color: "#666" }}>{tier}</div>
          </div>
        ))}
      </div>

      {/* Search */}
      <input
        type="text"
        placeholder="Search by handle..."
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{
          width: "100%",
          padding: "8px 12px",
          border: "1px solid #d1d5db",
          borderRadius: 6,
          fontSize: 14,
          marginBottom: 16,
          boxSizing: "border-box",
        }}
      />

      {/* Table */}
      <div style={{ overflowX: "auto" }}>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
          <thead style={{ backgroundColor: "#f9fafb", borderBottom: "2px solid #e5e7eb" }}>
            <tr>
              <SortHeader label="Handle" field="handle" current={sortField} dir={sortDir} onSort={handleSort} />
              <SortHeader label="Score" field="threat_score" current={sortField} dir={sortDir} onSort={handleSort} />
              <th style={{ padding: "8px 12px", textAlign: "left" }}>Tier</th>
              <SortHeader label="Toxicity" field="toxicity_score" current={sortField} dir={sortDir} onSort={handleSort} />
              <SortHeader label="Overlap" field="topic_overlap" current={sortField} dir={sortDir} onSort={handleSort} />
              <SortHeader label="Posts" field="posts_analyzed" current={sortField} dir={sortDir} onSort={handleSort} />
            </tr>
          </thead>
          <tbody>
            {filtered.map((account) => (
              <AccountRow
                key={account.did}
                account={account}
                expanded={expandedDid === account.did}
                onToggle={() => setExpandedDid(expandedDid === account.did ? null : account.did)}
              />
            ))}
          </tbody>
        </table>
      </div>

      {filtered.length === 0 && (
        <p style={{ textAlign: "center", color: "#888", marginTop: 24 }}>
          No accounts match your filters.
        </p>
      )}

      <p style={{ textAlign: "center", color: "#aaa", fontSize: 12, marginTop: 32 }}>
        Showing {filtered.length} of {data.total_accounts} accounts
      </p>
    </div>
  );
}
