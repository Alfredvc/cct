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

function aggregateByStream(
  node: TreeNode,
  prefixPath: string[],
): { total: number; streams: StreamMap; leafCount: number } {
  const streams = emptyStreams();
  let total = 0;
  let leafCount = 0;

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

  const { totals: globalStreamTotals, grand: globalGrand } = useMemo(() => {
    if (!data) return { totals: emptyStreams(), grand: 0 };
    const agg = aggregateByStream(data.tree, []);
    return { totals: agg.streams, grand: agg.total };
  }, [data]);

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

  // Format the headline number into integer + decimal parts so we can render
  // them at different visual weights (decimals lighter / smaller).
  const billedParts = billed
    .toLocaleString(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    })
    .split(".");
  const billedInt = billedParts[0];
  const billedFrac = billedParts[1] ?? "00";

  // Top stream by share — fuels the "headline finding" narrative line.
  const sortedStreams = [...STREAMS]
    .map((s) => ({
      ...s,
      v: globalStreamTotals[s.key] ?? 0,
      pct: globalGrand > 0 ? (globalStreamTotals[s.key] ?? 0) / globalGrand : 0,
    }))
    .sort((a, b) => b.v - a.v);
  const topStream = sortedStreams[0];

  const crumbs = ["all", ...zoomPath];

  const currentBucketKey = zoomPath[0];
  const currentBucketDesc =
    currentBucketKey && BUCKET_LABELS[currentBucketKey]
      ? BUCKET_LABELS[currentBucketKey].desc
      : null;

  // Top-row insight: when at the top level, surface a callout if the leader
  // dominates the zoomed total.
  const topRow = rows[0];
  const topRowShare =
    topRow && zoomTotal > 0 ? topRow.total / zoomTotal : 0;

  return (
    <>
      <div className="cost-subhead">
        <div className="days-badge">
          <span className="db-dot" />
          last <strong>{data.days}d</strong>
        </div>
        <div className="computed">
          updated {new Date(data.computed_at).toLocaleString()}
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
        {/* ─── HERO ─────────────────────────────────────────────── */}
        <section className="cost-hero-v2">
          <div className="hero-grid-bg" aria-hidden />

          <div className="hero-left">
            <div className="hero-eyebrow">
              <span className="eb-rule" />
              <span>Total billed · last {data.days} days</span>
            </div>

            <div className="hero-figure">
              <span className="hero-currency">$</span>
              <span className="hero-int">{billedInt}</span>
              <span className="hero-frac">.{billedFrac}</span>
              <span className="hero-iso">USD</span>
            </div>

            <div className="hero-coverage">
              <span className="cov-bar">
                <span
                  className="cov-fill"
                  style={{ width: `${(coverage * 100).toFixed(2)}%` }}
                />
              </span>
              <span className="cov-text">
                <strong>{(coverage * 100).toFixed(1)}%</strong>
                <span className="cov-sep">·</span>
                <span className="cov-num">
                  ${attributed.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                </span>{" "}
                <span className="cov-detail">attributed of</span>{" "}
                <span className="cov-num">
                  ${billed.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                </span>{" "}
                <span className="cov-detail">billed</span>
              </span>
            </div>

            {topStream && topStream.pct > 0 && (
              <p className="hero-narrative">
                <span className="hn-quote">“</span>
                <strong style={{ color: `var(${topStream.cssVar})` }}>
                  {topStream.label}
                </strong>{" "}
                {topStream.pct >= 0.4 ? "dominates" : "leads"} at{" "}
                <strong>{fmtPct(topStream.pct)}</strong>
                <span className="hn-em"> — {topStream.desc}.</span>
              </p>
            )}
          </div>

          <div className="hero-right">
            <div className="hero-eyebrow hero-eyebrow-right">
              <span>Where it goes</span>
              <span className="eb-hint">click a band to filter</span>
            </div>

            <div
              className={`stream-bar-v2 ${streamFilter ? "has-filter" : ""}`}
            >
              {STREAMS.map((s) => {
                const v = globalStreamTotals[s.key] ?? 0;
                const pct = globalGrand > 0 ? v / globalGrand : 0;
                if (pct < 0.001) return null;
                const dimmed = streamFilter && streamFilter !== s.key;
                const active = streamFilter === s.key;
                return (
                  <button
                    key={s.key}
                    type="button"
                    className={`sb-seg ${dimmed ? "dim" : ""} ${active ? "active" : ""}`}
                    style={{
                      width: `${pct * 100}%`,
                      background: `var(${s.cssVar})`,
                    }}
                    onClick={() =>
                      setStreamFilter(streamFilter === s.key ? null : s.key)
                    }
                    title={`${s.label} — ${fmtUsdFull(v)} (${fmtPct(pct)})`}
                  >
                    {pct > 0.06 && (
                      <span className="sb-seg-text">
                        <span className="sb-seg-pct">{fmtPct(pct)}</span>
                      </span>
                    )}
                  </button>
                );
              })}
            </div>

            <div className="stream-legend">
              {sortedStreams.map((s) => {
                const isActive = streamFilter === s.key;
                const isDim = streamFilter && !isActive;
                return (
                  <button
                    key={s.key}
                    type="button"
                    className={`sl-chip ${isActive ? "active" : ""} ${isDim ? "dim" : ""}`}
                    style={
                      {
                        ["--chip-accent" as string]: `var(${s.cssVar})`,
                      } as React.CSSProperties
                    }
                    onClick={() =>
                      setStreamFilter(streamFilter === s.key ? null : s.key)
                    }
                    title={s.desc}
                  >
                    <span className="sl-dot" />
                    <span className="sl-label">{s.label}</span>
                    <span className="sl-amount">{fmtUsdFull(s.v)}</span>
                    <span className="sl-pct">{fmtPct(s.pct)}</span>
                  </button>
                );
              })}
            </div>

            {streamFilter && (
              <div className="stream-filter-active">
                filtering rows below to{" "}
                <strong style={{ color: `var(${STREAM_BY_KEY[streamFilter].cssVar})` }}>
                  {STREAM_BY_KEY[streamFilter].label}
                </strong>{" "}
                · <button className="sf-clear" onClick={() => setStreamFilter(null)}>clear</button>
              </div>
            )}
          </div>
        </section>

        {/* ─── INSIGHT CALLOUT (top of zoom) ────────────────────── */}
        {zoomPath.length === 0 && topRow && topRowShare >= 0.25 && (
          <div className="cost-insight">
            <span className="ci-bullet">▲</span>
            <span>
              <strong>{topRow.label}</strong> is your largest line at{" "}
              <strong>{fmtUsdFull(topRow.total)}</strong> ({fmtPct(topRowShare)}).{" "}
              {topRow.hasChildren && (
                <button
                  className="ci-link"
                  onClick={() => setZoomPath([...zoomPath, topRow.name])}
                >
                  drill in →
                </button>
              )}
            </span>
          </div>
        )}

        {/* ─── ROWS PANEL ───────────────────────────────────────── */}
        <section className="cost-rows-panel-v2">
          <header className="rows-header-strip">
            <div className="rhs-left">
              <div className="rhs-title">
                <span className="rhs-kicker">Breakdown</span>
                <span className="rhs-trail">
                  {crumbs.map((c, i) => {
                    const last = i === crumbs.length - 1;
                    const label = i === 0 ? "all" : labelFor(c, i);
                    return (
                      <span key={i} className="trail-wrap">
                        <button
                          type="button"
                          className={`trail-crumb ${last ? "current" : ""}`}
                          onClick={() =>
                            !last && setZoomPath(zoomPath.slice(0, i))
                          }
                          disabled={last}
                        >
                          {label}
                        </button>
                        {!last && <span className="trail-sep">/</span>}
                      </span>
                    );
                  })}
                </span>
              </div>
              {currentBucketDesc && (
                <p className="rhs-caption">{currentBucketDesc}</p>
              )}
            </div>
            <div className="rhs-right">
              <span className="rhs-meta">
                <strong>{filteredRows.length}</strong>{" "}
                {filteredRows.length === 1 ? "row" : "rows"}
                <span className="rhs-meta-sep">·</span>
                <span>{fmtUsdFull(zoomTotal)}</span>
              </span>
              {zoomPath.length > 0 && (
                <button
                  className="rhs-reset"
                  onClick={() => setZoomPath([])}
                  title="reset to top level"
                >
                  ↩ reset
                </button>
              )}
            </div>
          </header>

          <div className="rows-table-head">
            <div className="rh-rank">#</div>
            <div className="rh-label">name · description</div>
            <div className="rh-bar">distribution</div>
            <div className="rh-amount">cost</div>
            <div className="rh-pct">share</div>
            <div className="rh-spacer" />
          </div>

          <div className="rows-list">
            {visibleRows.length === 0 && (
              <div className="rows-empty">no rows match</div>
            )}
            {visibleRows.map((row, idx) => {
              const pct = zoomTotal > 0 ? row.total / zoomTotal : 0;
              const fillWidth =
                maxRowTotal > 0 ? (row.total / maxRowTotal) * 100 : 0;
              return (
                <div
                  key={row.name}
                  className={`row-v2 ${row.hasChildren ? "drillable" : ""}`}
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
                  <div className="rv-rank">{String(idx + 1).padStart(2, "0")}</div>
                  <div className="rv-label">
                    <div className="rv-name">{row.label}</div>
                    {row.desc && <div className="rv-desc">{row.desc}</div>}
                    {!row.desc && row.leafCount > 1 && (
                      <div className="rv-desc">
                        {row.leafCount.toLocaleString()} leaves
                        {row.hasChildren ? ` · ${row.childCount} children` : ""}
                      </div>
                    )}
                  </div>
                  <div className="rv-bar">
                    <div className="rv-bar-track">
                      <div
                        className="rv-bar-fill"
                        style={{ width: `${fillWidth.toFixed(2)}%` }}
                      >
                        {STREAMS.map((s) => {
                          const sv = row.streams[s.key] ?? 0;
                          if (sv <= 0) return null;
                          const segPct = (sv / row.total) * 100;
                          return (
                            <div
                              key={s.key}
                              className="rv-bar-seg"
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
                  <div className="rv-amount">{fmtUsdFull(row.total)}</div>
                  <div className="rv-pct">
                    <span className="rv-pct-bar">
                      <span
                        className="rv-pct-fill"
                        style={{ width: `${(pct * 100).toFixed(2)}%` }}
                      />
                    </span>
                    <span className="rv-pct-val">{fmtPct(pct)}</span>
                  </div>
                  <div className="rv-chev">
                    {row.hasChildren ? "›" : ""}
                  </div>
                </div>
              );
            })}
          </div>

          {hiddenCount > 0 && !expanded && (
            <button
              className="rows-expand-v2"
              onClick={() => setExpanded(true)}
            >
              <span>show {hiddenCount} more</span>
              <span className="rxv-meta">
                {fmtUsdFull(hiddenTotal)} ·{" "}
                {fmtPct(zoomTotal > 0 ? hiddenTotal / zoomTotal : 0)}
              </span>
              <span className="rxv-arrow">▾</span>
            </button>
          )}
          {expanded && filteredRows.length > TOP_N_DEFAULT && (
            <button
              className="rows-expand-v2"
              onClick={() => setExpanded(false)}
            >
              <span>collapse</span>
              <span className="rxv-arrow">▴</span>
            </button>
          )}
        </section>
      </div>
    </>
  );
}
