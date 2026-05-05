import { useEffect, useMemo, useState } from "react";
import "./CostPage.css";

type TreeNode = {
  name: string;
  value: number;
  children?: TreeNode[];
};

type DecompResponse = {
  tree: TreeNode;
  total_billed_usd: number;
  total_attributed_usd: number;
  days: number;
  computed_at: string;
};

type StreamDef = {
  key: string;
  label: string;
  cssVar: string;
  desc: string;
};

const STREAMS: StreamDef[] = [
  {
    key: "fresh_input",
    label: "Fresh input",
    cssVar: "--tok-input",
    desc: "first-time prompt + tool-use input",
  },
  {
    key: "cache_read",
    label: "Cache read",
    cssVar: "--tok-cr",
    desc: "context paid every later turn",
  },
  {
    key: "cache_create",
    label: "Cache create",
    cssVar: "--tok-cw",
    desc: "one-shot 5m / 1h cache writes",
  },
  {
    key: "output_write",
    label: "Output",
    cssVar: "--tok-out",
    desc: "model output tokens",
  },
];

const STREAM_BY_KEY: Record<string, StreamDef> = Object.fromEntries(
  STREAMS.map((s) => [s.key, s]),
);

type BucketMeta = { label: string; desc: string };

const BUCKET_LABELS: Record<string, BucketMeta> = {
  tool_result: {
    label: "Tool result reads",
    desc: "tokens spent re-reading tool outputs (Bash stdout, file reads, etc.) on every later turn",
  },
  asst_output_persisted: {
    label: "Assistant output (persisted)",
    desc: "model output that gets cached and re-read — text, thinking, tool-use JSON",
  },
  system_block_first_turn: {
    label: "System prompt (first turn)",
    desc: "the system prompt cached on the first turn of each session",
  },
  user_text: {
    label: "User messages",
    desc: "user-typed turns — first-turn (cached) and later turns (fresh)",
  },
  attachment: {
    label: "File attachments",
    desc: "files attached to user messages (paste / drag / @-mention)",
  },
  cache_bust: {
    label: "Cache invalidations",
    desc: "events that invalidated the cache and forced re-creation",
  },
  fresh_input: {
    label: "Other fresh input",
    desc: "uncategorized fresh-input tokens",
  },
  unaccounted: {
    label: "Unaccounted",
    desc: "the residual gap between billed and attributed cost",
  },
  other: { label: "Other", desc: "" },
};

const SUBCAT_LABELS: Record<string, string> = {
  asst_text: "Text reply",
  asst_thinking: "Thinking",
  asst_tool_use_json: "Tool-use JSON",
  user_text: "User text (later turns)",
  user_text_first_turn: "User text (first turn)",
  fresh_input_uncategorized: "Other fresh input",
};

function labelFor(name: string, depth: number): string {
  if (depth === 1 && BUCKET_LABELS[name]) return BUCKET_LABELS[name].label;
  if (SUBCAT_LABELS[name]) return SUBCAT_LABELS[name];
  if (name in STREAM_BY_KEY) return STREAM_BY_KEY[name].label;
  if (name === "main") return "Main chain";
  if (name === "subagent") return "Subagent";
  return name;
}

function descFor(name: string, depth: number): string {
  if (depth === 1 && BUCKET_LABELS[name]) return BUCKET_LABELS[name].desc;
  if (name in STREAM_BY_KEY) return STREAM_BY_KEY[name].desc;
  return "";
}

const fmtUsdFull = (cents: number) =>
  `$${(cents / 100).toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;

const fmtPct = (frac: number) => `${(frac * 100).toFixed(1)}%`;

function findNode(
  root: TreeNode,
  path: string[],
): { node: TreeNode; ancestors: TreeNode[] } | null {
  let cur = root;
  const ancestors: TreeNode[] = [root];
  for (const name of path) {
    const child = cur.children?.find((c) => c.name === name);
    if (!child) return null;
    cur = child;
    ancestors.push(child);
  }
  return { node: cur, ancestors };
}

type StreamMap = Record<string, number>;

function emptyStreams(): StreamMap {
  return { fresh_input: 0, cache_read: 0, cache_create: 0, output_write: 0 };
}

// For a node, walk its descendants and accumulate cost split by stream
// (the stream is determined by the first ancestor whose name is a stream key,
// scanning from the leaf toward the node — and falling back to scanning
// outward through the supplied prefix path if no inner ancestor matched).
function aggregateByStream(
  node: TreeNode,
  prefixPath: string[],
): { total: number; streams: StreamMap; leafCount: number } {
  const streams = emptyStreams();
  let total = 0;
  let leafCount = 0;

  // Determine if the prefix already pinned a stream
  let prefixStream: string | null = null;
  for (const name of prefixPath) {
    if (name in STREAM_BY_KEY) {
      prefixStream = name;
      break;
    }
  }

  function walk(n: TreeNode, innerStream: string | null) {
    const here = n.name in STREAM_BY_KEY ? n.name : innerStream;
    if (!n.children || n.children.length === 0) {
      const stream = here ?? prefixStream;
      const cents = n.value;
      total += cents;
      leafCount += 1;
      if (stream) streams[stream] += cents;
      return;
    }
    for (const c of n.children) walk(c, here);
  }

  walk(node, null);
  return { total, streams, leafCount };
}

type Row = {
  name: string;
  label: string;
  desc: string;
  total: number;
  streams: StreamMap;
  hasChildren: boolean;
  childCount: number;
  leafCount: number;
};

export function CostPage() {
  const [data, setData] = useState<DecompResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [zoomPath, setZoomPath] = useState<string[]>([]);
  const [streamFilter, setStreamFilter] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);

  const TOP_N_DEFAULT = 12;

  useEffect(() => {
    let cancelled = false;
    const attempt = (tries: number) => {
      fetch("/api/dashboard/cost-decomposition")
        .then((res) => {
          if (res.status === 503) {
            if (tries > 0 && !cancelled) {
              setTimeout(() => attempt(tries - 1), 2000);
            } else if (!cancelled) {
              setError("Cost decomposition not yet computed.");
            }
            return null;
          }
          if (!res.ok) throw new Error(`HTTP ${res.status}`);
          return res.json();
        })
        .then((json) => {
          if (json && !cancelled) setData(json);
        })
        .catch((e) => {
          if (!cancelled) setError(String(e));
        });
    };
    attempt(15);
    return () => {
      cancelled = true;
    };
  }, []);

  const located = useMemo(() => {
    if (!data) return null;
    return findNode(data.tree, zoomPath);
  }, [data, zoomPath]);

  // Stream totals across the whole tree (for the top stacked bar — always global)
  const { totals: globalStreamTotals, grand: globalGrand } = useMemo(() => {
    if (!data) return { totals: emptyStreams(), grand: 0 };
    const agg = aggregateByStream(data.tree, []);
    return { totals: agg.streams, grand: agg.total };
  }, [data]);

  // Rows = direct children of zoomed node. For each child, aggregate its
  // descendants by stream so the user sees the orthogonal split inline.
  const rows: Row[] = useMemo(() => {
    if (!located) return [];
    const { node } = located;
    const children = node.children ?? [];
    const depth = zoomPath.length + 1;
    const out: Row[] = children.map((c) => {
      const agg = aggregateByStream(c, zoomPath);
      return {
        name: c.name,
        label: labelFor(c.name, depth),
        desc: descFor(c.name, depth),
        total: agg.total,
        streams: agg.streams,
        hasChildren: !!(c.children && c.children.length > 0),
        childCount: c.children?.length ?? 0,
        leafCount: agg.leafCount,
      };
    });
    out.sort((a, b) => b.total - a.total);
    return out;
  }, [located, zoomPath]);

  const zoomTotal = located?.node.value ?? 0;

  // Filter rows by search + stream filter
  const filteredRows = useMemo(() => {
    const q = search.trim().toLowerCase();
    let r = rows;
    if (q) {
      r = r.filter(
        (row) =>
          row.label.toLowerCase().includes(q) ||
          row.name.toLowerCase().includes(q),
      );
    }
    if (streamFilter) {
      r = r.filter((row) => (row.streams[streamFilter] ?? 0) > 0);
    }
    return r;
  }, [rows, search, streamFilter]);

  const visibleRows = expanded
    ? filteredRows
    : filteredRows.slice(0, TOP_N_DEFAULT);
  const hiddenCount = filteredRows.length - visibleRows.length;
  const hiddenTotal = filteredRows
    .slice(TOP_N_DEFAULT)
    .reduce((s, r) => s + r.total, 0);

  // Max row total — used to scale bar fill width so the largest row uses 100%
  // of the bar track.
  const maxRowTotal = visibleRows[0]?.total ?? 0;

  if (error) {
    return (
      <div className="cost-page">
        <div className="cost-error">Error: {error}</div>
      </div>
    );
  }

  if (!data || !located) {
    return (
      <div className="cost-page">
        <div className="cost-loading">
          <span className="spinner" />
          Computing decomposition (~10s)…
        </div>
      </div>
    );
  }

  const billed = data.total_billed_usd;
  const attributed = data.total_attributed_usd;
  const coverage = billed > 0 ? attributed / billed : 0;

  // Crumbs: ["all", ...zoomPath]
  const crumbs = ["all", ...zoomPath];

  // The bucket-description caption that appears when zoomed inside a known
  // bucket. Show the description of the current zoom root (top-level bucket
  // if zoomed at depth ≥ 1).
  const currentBucketKey = zoomPath[0];
  const currentBucketDesc =
    currentBucketKey && BUCKET_LABELS[currentBucketKey]
      ? BUCKET_LABELS[currentBucketKey].desc
      : null;

  return (
    <>
      <div className="cost-subhead">
        <div className="days-badge">
          last <strong>{data.days}d</strong>
        </div>
        <div className="computed">
          computed {new Date(data.computed_at).toLocaleString()}
        </div>
        <input
          type="search"
          className="cost-search"
          placeholder="search rows…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </div>

      <div className="cost-page">
        <div className="cost-hero">
          <div className="panel">
            <div className="panel-title">
              Total billed
              <span className="panel-meta">last {data.days} days</span>
            </div>
            <div className="hero-headline">
              <div className="hero-amount">
                $
                {billed.toLocaleString(undefined, {
                  minimumFractionDigits: 2,
                  maximumFractionDigits: 2,
                })}
              </div>
              <div className="hero-label">USD</div>
            </div>
            <div className="hero-coverage">
              <span className="cov-detail">attributed</span>
              <span className="cov-pct">{(coverage * 100).toFixed(1)}%</span>
              <div className="cov-bar">
                <div
                  className="cov-fill"
                  style={{ width: `${(coverage * 100).toFixed(2)}%` }}
                />
              </div>
              <span className="cov-detail">
                $
                {attributed.toLocaleString(undefined, {
                  minimumFractionDigits: 2,
                  maximumFractionDigits: 2,
                })}{" "}
                / $
                {billed.toLocaleString(undefined, {
                  minimumFractionDigits: 2,
                  maximumFractionDigits: 2,
                })}
              </span>
            </div>
          </div>

          <div className="panel stream-bar-panel">
            <div className="panel-title">
              Stream split
              <span className="panel-meta">click to filter rows</span>
            </div>
            <div className="stream-bar">
              {STREAMS.map((s) => {
                const v = globalStreamTotals[s.key] ?? 0;
                const pct = globalGrand > 0 ? v / globalGrand : 0;
                if (pct < 0.001) return null;
                const dimmed = streamFilter && streamFilter !== s.key;
                return (
                  <div
                    key={s.key}
                    className={`stream-seg ${dimmed ? "dim" : ""}`}
                    style={{
                      width: `${pct * 100}%`,
                      background: `var(${s.cssVar})`,
                    }}
                    onClick={() =>
                      setStreamFilter(streamFilter === s.key ? null : s.key)
                    }
                    title={`${s.label} — ${fmtUsdFull(v)} (${fmtPct(pct)})`}
                  >
                    {pct > 0.07 ? `${s.label} ${fmtPct(pct)}` : ""}
                  </div>
                );
              })}
            </div>
            <div className="stream-bar-hint">
              {streamFilter
                ? `filtered to ${STREAM_BY_KEY[streamFilter].label} — click again to clear`
                : "click a segment to filter the rows below"}
            </div>
          </div>
        </div>

        <div className="stream-cards">
          {STREAMS.map((s) => {
            const v = globalStreamTotals[s.key] ?? 0;
            const pct = globalGrand > 0 ? v / globalGrand : 0;
            const isActive = streamFilter === s.key;
            const isDim = streamFilter && !isActive;
            return (
              <div
                key={s.key}
                className={`stream-card ${isActive ? "active" : ""} ${
                  isDim ? "dim" : ""
                }`}
                style={
                  {
                    ["--card-accent" as string]: `var(${s.cssVar})`,
                  } as React.CSSProperties
                }
                onClick={() =>
                  setStreamFilter(streamFilter === s.key ? null : s.key)
                }
              >
                <div className="sc-label">
                  <span className="sc-dot" /> {s.label}
                </div>
                <div className="sc-amount">{fmtUsdFull(v)}</div>
                <div className="sc-meta">
                  <span className="sc-pct">{fmtPct(pct)}</span>
                  <span>of attributed</span>
                </div>
                <div className="sc-desc">{s.desc}</div>
              </div>
            );
          })}
        </div>

        <div className="panel cost-rows-panel">
          <div className="rows-toolbar">
            <div className="cost-crumbs">
              {crumbs.map((c, i) => {
                const last = i === crumbs.length - 1;
                const label =
                  i === 0 ? "all" : labelFor(c, i);
                return (
                  <span key={i} className="crumb-wrap">
                    <span
                      className={`crumb ${last ? "current" : ""}`}
                      onClick={() =>
                        !last && setZoomPath(zoomPath.slice(0, i))
                      }
                    >
                      {label}
                    </span>
                    {!last && <span className="crumb-sep">›</span>}
                  </span>
                );
              })}
            </div>
            <button
              className="cost-reset"
              onClick={() => setZoomPath([])}
              disabled={zoomPath.length === 0}
            >
              reset
            </button>
            <div className="rows-meta">
              showing <strong>{filteredRows.length}</strong>{" "}
              {filteredRows.length === 1 ? "row" : "rows"} · zoom total{" "}
              <strong>{fmtUsdFull(zoomTotal)}</strong>
            </div>
          </div>

          {currentBucketDesc && (
            <div className="rows-caption">{currentBucketDesc}</div>
          )}

          <div className="rows-header">
            <div className="rh-rank">#</div>
            <div className="rh-label">name</div>
            <div className="rh-bar">cost · split by stream</div>
            <div className="rh-amount">$</div>
            <div className="rh-pct">%</div>
          </div>

          <div className="cost-rows">
            {visibleRows.length === 0 && (
              <div className="rows-empty">no rows match</div>
            )}
            {visibleRows.map((row, idx) => {
              const pct = zoomTotal > 0 ? row.total / zoomTotal : 0;
              const fillWidth =
                maxRowTotal > 0
                  ? (row.total / maxRowTotal) * 100
                  : 0;
              return (
                <div
                  key={row.name}
                  className={`row ${row.hasChildren ? "drillable" : ""}`}
                  onClick={() =>
                    row.hasChildren &&
                    setZoomPath([...zoomPath, row.name])
                  }
                  title={
                    row.hasChildren
                      ? `click to drill into ${row.label} (${row.childCount} ${row.childCount === 1 ? "child" : "children"})`
                      : `${row.label} — leaf`
                  }
                >
                  <div className="r-rank">{idx + 1}</div>
                  <div className="r-label">
                    <div className="r-name">{row.label}</div>
                    {row.desc && <div className="r-desc">{row.desc}</div>}
                    {!row.desc && row.leafCount > 1 && (
                      <div className="r-desc">
                        {row.leafCount.toLocaleString()} leaves
                        {row.hasChildren ? ` · ${row.childCount} children` : ""}
                      </div>
                    )}
                  </div>
                  <div className="r-bar">
                    <div className="r-bar-track">
                      <div
                        className="r-bar-fill"
                        style={{ width: `${fillWidth.toFixed(2)}%` }}
                      >
                        {STREAMS.map((s) => {
                          const sv = row.streams[s.key] ?? 0;
                          if (sv <= 0) return null;
                          const segPct = (sv / row.total) * 100;
                          return (
                            <div
                              key={s.key}
                              className="r-bar-seg"
                              style={{
                                width: `${segPct.toFixed(2)}%`,
                                background: `var(${s.cssVar})`,
                              }}
                              title={`${s.label} ${fmtUsdFull(sv)} (${fmtPct(sv / row.total)})`}
                            />
                          );
                        })}
                      </div>
                    </div>
                  </div>
                  <div className="r-amount">{fmtUsdFull(row.total)}</div>
                  <div className="r-pct">{fmtPct(pct)}</div>
                  <div className="r-chev">
                    {row.hasChildren ? "›" : ""}
                  </div>
                </div>
              );
            })}
          </div>

          {hiddenCount > 0 && !expanded && (
            <button
              className="rows-expand"
              onClick={() => setExpanded(true)}
            >
              show {hiddenCount} more · {fmtUsdFull(hiddenTotal)} ·{" "}
              {fmtPct(zoomTotal > 0 ? hiddenTotal / zoomTotal : 0)}
            </button>
          )}
          {expanded && filteredRows.length > TOP_N_DEFAULT && (
            <button
              className="rows-expand"
              onClick={() => setExpanded(false)}
            >
              collapse
            </button>
          )}
        </div>
      </div>
    </>
  );
}
