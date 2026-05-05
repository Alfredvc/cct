import { useEffect, useRef, useState } from "react";
import * as d3 from "d3";
import { flamegraph, type StackFrame } from "d3-flame-graph";
import "d3-flame-graph/dist/d3-flamegraph.css";

type DecompResponse = {
  tree: StackFrame;
  total_billed_usd: number;
  total_attributed_usd: number;
  days: number;
  computed_at: string;
};

const STREAM_COLORS: Record<string, string> = {
  cache_read: "#5b8def",
  cache_create: "#e87f3e",
  output_write: "#43a047",
  fresh_input: "#9c27b0",
};

function colorOf(node: { data: StackFrame; parent: any }): string {
  let n: any = node;
  while (n) {
    const name = n.data?.name as string | undefined;
    if (name && STREAM_COLORS[name]) return STREAM_COLORS[name];
    n = n.parent;
  }
  return "#888";
}

export function FlamegraphPage() {
  const [data, setData] = useState<DecompResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const chartRef = useRef<HTMLDivElement>(null);
  const flameRef = useRef<ReturnType<typeof flamegraph> | null>(null);

  // Fetch decomposition data. The endpoint returns 503 while the server is
  // still computing the initial result at startup; retry a few times before
  // surfacing an error.
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
          if (!res.ok) {
            throw new Error(`HTTP ${res.status}`);
          }
          return res.json();
        })
        .then((json) => {
          if (json && !cancelled) setData(json);
        })
        .catch((e) => {
          if (!cancelled) setError(String(e));
        });
    };
    attempt(15); // ~30s grace
    return () => {
      cancelled = true;
    };
  }, []);

  // Build the flamegraph once data is in. Re-build on data change (rare —
  // happens when the DB is re-ingested).
  useEffect(() => {
    if (!data || !chartRef.current) return;
    const el = chartRef.current;
    el.innerHTML = "";

    const chart = flamegraph()
      .width(el.clientWidth)
      .cellHeight(20)
      .transitionDuration(250)
      .minFrameSize(2)
      .sort(true)
      .setColorMapper(colorOf)
      .label((d: any) => {
        const v = (d.value as number) / 100;
        const root = (data.tree.value as number) / 100;
        const pct = root > 0 ? (100 * (d.value as number)) / data.tree.value : 0;
        return `${d.data.name}  $${v.toFixed(2)}  (${pct.toFixed(1)}%)`;
      });

    flameRef.current = chart;
    d3.select(el).datum(data.tree).call(chart);

    const onResize = () => {
      if (!chartRef.current || !flameRef.current) return;
      flameRef.current.width(chartRef.current.clientWidth);
      flameRef.current.update(data.tree);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      flameRef.current?.destroy();
      flameRef.current = null;
    };
  }, [data]);

  // Apply search to the live chart instance.
  useEffect(() => {
    const f = flameRef.current;
    if (!f) return;
    if (search) f.search(search);
    else f.clear();
  }, [search]);

  return (
    <div style={{ padding: "16px 24px", color: "#222" }}>
      <div style={headerRow}>
        <div style={titleStyle}>Cost decomposition</div>
        {data && (
          <div style={subtitleStyle}>
            ${data.total_attributed_usd.toFixed(2)} attributed of $
            {data.total_billed_usd.toFixed(2)} billed · last {data.days}d ·
            computed {new Date(data.computed_at).toLocaleString()}
          </div>
        )}
        <input
          type="search"
          placeholder="search…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          style={searchStyle}
        />
      </div>
      <div style={legendStyle}>
        {Object.entries(STREAM_COLORS).map(([name, color]) => (
          <span key={name} style={{ color }}>
            ■ {name}
          </span>
        ))}
        <span style={{ color: "#666", marginLeft: 12, fontSize: 12 }}>
          width = USD · click block to zoom · click root to reset · hierarchy
          bucket → category → subcat → stream → scope
        </span>
      </div>
      {error && (
        <div style={{ color: "#c62828", marginTop: 16 }}>Error: {error}</div>
      )}
      {!error && !data && (
        <div style={{ color: "#666", marginTop: 16 }}>
          Computing decomposition (~10s)…
        </div>
      )}
      <div ref={chartRef} style={{ width: "100%", marginTop: 12 }} />
    </div>
  );
}

const headerRow: React.CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: 16,
  marginBottom: 6,
};
const titleStyle: React.CSSProperties = { fontSize: 20, fontWeight: 600 };
const subtitleStyle: React.CSSProperties = {
  fontSize: 13,
  color: "#555",
  fontVariantNumeric: "tabular-nums",
  flex: 1,
};
const searchStyle: React.CSSProperties = {
  padding: "4px 8px",
  font: "13px monospace",
  width: 220,
};
const legendStyle: React.CSSProperties = {
  display: "flex",
  gap: 14,
  fontSize: 12,
  color: "#555",
  marginBottom: 12,
  alignItems: "center",
};
