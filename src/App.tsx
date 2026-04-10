import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import "./App.css";

// -- Types matching Rust structs --

interface StatusResponse {
  state: string;
  context?: Record<string, unknown>;
  ship_name: string | null;
  is_ready: boolean;
}

interface DiagnosticsData {
  app_support_path: string;
  pier_path: string;
  bundled_vere_version: string | null;
  pier_vere_version: string | null;
  current_state: { state: string; context?: Record<string, unknown> };
  pid: number | null;
  last_exit_code: number | null;
  last_error: string | null;
  ship_name: string | null;
  http_port: number;
}

// -- Helpers --

function formatUptime(startedAt: string): string {
  const start = new Date(startedAt);
  const now = new Date();
  const diffMs = now.getTime() - start.getTime();
  const secs = Math.floor(diffMs / 1000);
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ${secs % 60}s`;
  const hrs = Math.floor(mins / 60);
  return `${hrs}h ${mins % 60}m`;
}

function shipDisplayName(name: string | null): string {
  if (!name) return "Ship";
  return name.startsWith("~") ? name : `~${name}`;
}

// -- Components --

/** Strip the timestamp and [source] prefix, returning the display text and whether it's a launcher line. */
function parseLine(raw: string): { text: string; isLauncher: boolean } {
  // Format: "2026-04-09T23:00:03.429Z [launcher] message"
  //     or: "2026-04-09T23:00:03.480Z [stdout] message"
  const isLauncher = raw.includes("[launcher]");
  // Strip leading ISO timestamp (everything up to and including "Z ")
  const afterTs = raw.replace(/^\d{4}-\d{2}-\d{2}T[\d:.]+Z\s*/, "");
  // Strip the bracketed source tag
  const text = afterTs.replace(/^\[(stdout|stderr|launcher)\]\s*/, "");
  return { text, isLauncher };
}

function LogPanel({ logs }: { logs: string[] }) {
  const bodyRef = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);

  useEffect(() => {
    if (follow && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [logs, follow]);

  const handleScroll = useCallback(() => {
    const el = bodyRef.current;
    if (!el) return;
    // If user scrolls to within 20px of bottom, re-enable follow
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 20;
    setFollow(atBottom);
  }, []);

  return (
    <div className="log-panel">
      <div className="log-header">
        <span className="log-header-label">Logs</span>
        <button
          className={`log-follow-btn${follow ? " active" : ""}`}
          onClick={() => {
            const next = !follow;
            setFollow(next);
            if (next && bodyRef.current) {
              bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
            }
          }}
        >
          Follow
        </button>
      </div>
      <div ref={bodyRef} className="log-body" onScroll={handleScroll}>
        {logs.length === 0 ? (
          <div className="log-empty">No log output yet</div>
        ) : (
          logs.map((line, i) => {
            const { text, isLauncher } = parseLine(line);
            return (
              <div
                key={i}
                className={`log-line${isLauncher ? " log-launcher" : ""}`}
              >
                {text}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function DiagnosticsModal({
  onClose,
}: {
  onClose: () => void;
}) {
  const [data, setData] = useState<DiagnosticsData | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    invoke<DiagnosticsData>("get_diagnostics").then(setData);
  }, []);

  const copyToClipboard = useCallback(() => {
    if (!data) return;
    const text = Object.entries(data)
      .map(([k, v]) => `${k}: ${typeof v === "object" ? JSON.stringify(v) : v}`)
      .join("\n");
    writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [data]);

  const revealDataDir = useCallback(() => {
    invoke("reveal_data_dir");
  }, []);

  return (
    <div className="diagnostics-overlay" onClick={onClose}>
      <div className="diagnostics-panel" onClick={(e) => e.stopPropagation()}>
        <div className="diagnostics-header">
          <span className="diagnostics-title">Diagnostics</span>
          <button className="icon-btn" onClick={onClose}>
            &times;
          </button>
        </div>
        {data && (
          <div className="diagnostics-body">
            <DiagRow label="State" value={data.current_state.state} />
            <DiagRow label="Ship" value={data.ship_name ?? "unknown"} />
            <DiagRow label="PID" value={data.pid?.toString() ?? "none"} />
            <DiagRow label="HTTP port" value={data.http_port.toString()} />
            <DiagRow label="App support" value={data.app_support_path} />
            <DiagRow label="Pier path" value={data.pier_path} />
            <DiagRow
              label="Vere (bundled)"
              value={data.bundled_vere_version ?? "unknown"}
            />
            <DiagRow
              label="Vere (pier)"
              value={data.pier_vere_version ?? "unknown"}
            />
            <DiagRow
              label="Last exit code"
              value={data.last_exit_code?.toString() ?? "none"}
            />
            <DiagRow
              label="Last error"
              value={data.last_error ?? "none"}
            />
          </div>
        )}
        <div className="diagnostics-footer">
          <button onClick={copyToClipboard}>
            {copied ? "Copied" : "Copy to Clipboard"}
          </button>
          <button onClick={revealDataDir}>Reveal Data Directory</button>
        </div>
      </div>
    </div>
  );
}

function DiagRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="diagnostics-row">
      <span className="diagnostics-key">{label}</span>
      <span className="diagnostics-value">{value}</span>
    </div>
  );
}

// -- Screen Components --

function PreparingScreen({ status, logs }: { status: StatusResponse; logs: string[] }) {
  const stateLabel = status.state;
  let message = "Preparing your ship...";
  if (stateLabel === "Extracting") {
    const ctx = status.context as { message?: string } | undefined;
    message = ctx?.message ?? "Extracting ship archive...";
  } else if (stateLabel === "Prepared") {
    message = "Validating pier...";
  } else if (stateLabel === "Starting") {
    message = "Starting runtime...";
  } else if (stateLabel === "Uninitialized") {
    message = "Initializing...";
  }

  return (
    <div className="screen">
      <div className="screen-center">
        <div className="spinner" />
        <div className="preparing-status">{message}</div>
        <div className="ship-name">{shipDisplayName(status.ship_name)}</div>
      </div>
      <LogPanel logs={logs} />
    </div>
  );
}

function RunningScreen({
  status,
  logs,
  onStop,
  onRestart,
  onOpenShip,
}: {
  status: StatusResponse;
  logs: string[];
  onStop: () => void;
  onRestart: () => void;
  onOpenShip: () => void;
}) {
  const ctx = status.context as { started_at?: string; pid?: number } | undefined;
  const startedAt = ctx?.started_at;

  const [uptime, setUptime] = useState("");
  const [codeCopied, setCodeCopied] = useState(false);
  const [codeLoading, setCodeLoading] = useState(false);

  useEffect(() => {
    if (!startedAt) return;
    const update = () => setUptime(formatUptime(startedAt));
    update();
    const interval = setInterval(update, 1000);
    return () => clearInterval(interval);
  }, [startedAt]);

  const handleCopyCode = useCallback(async () => {
    setCodeLoading(true);
    try {
      const code = await invoke<string>("get_login_code");
      await writeText(code);
      setCodeCopied(true);
      setTimeout(() => setCodeCopied(false), 2000);
    } catch (e) {
      console.error("Failed to get +code:", e);
    } finally {
      setCodeLoading(false);
    }
  }, []);

  return (
    <div className="screen">
      <div className="screen-info">
        <div className="info-row">
          <span className="info-label">URL</span>
          <span className="ship-url">http://localhost:8080</span>
        </div>
        {startedAt && (
          <div className="info-row">
            <span className="info-label">Uptime</span>
            <span className="uptime">{uptime}</span>
          </div>
        )}
        <div className="actions">
          <button
            className="primary"
            onClick={onOpenShip}
            disabled={!status.is_ready}
          >
            {status.is_ready ? (<>Web access <span aria-hidden="true" style={{ fontSize: "10px" }}>{"\u2197"}</span></>) : "Waiting for ship..."}
          </button>
          <button
            onClick={handleCopyCode}
            disabled={!status.is_ready || codeLoading}
          >
            {codeCopied ? "Copied!" : codeLoading ? "Getting..." : "Copy +code"}
          </button>
          <button onClick={onStop}>Stop</button>
          <button onClick={onRestart}>Restart</button>
        </div>
      </div>
      <LogPanel logs={logs} />
    </div>
  );
}

function StoppedScreen({
  status,
  onStart,
}: {
  status: StatusResponse;
  onStart: () => void;
}) {
  return (
    <div className="screen">
      <div className="screen-center">
        <div className="ship-name">{shipDisplayName(status.ship_name)}</div>
        <div style={{ color: "#6b7280", fontSize: "13px" }}>
          Ship is stopped
        </div>
        <div className="actions">
          <button className="primary" onClick={onStart}>
            Start
          </button>
        </div>
      </div>
    </div>
  );
}

function ErrorScreen({
  status,
  logs,
  onRetry,
  onReset,
}: {
  status: StatusResponse;
  logs: string[];
  onRetry: () => void;
  onReset: () => void;
}) {
  const ctx = status.context as { message?: string; detail?: string } | undefined;
  const [showDetail, setShowDetail] = useState(false);

  return (
    <div className="screen">
      <div className="screen-info">
        <div className="error-box">
          <div className="error-title">Something went wrong</div>
          <div className="error-message">{ctx?.message ?? "Unknown error"}</div>
          {ctx?.detail && (
            <>
              <button
                className="icon-btn"
                style={{ fontSize: "11px", marginBottom: "4px" }}
                onClick={() => setShowDetail(!showDetail)}
              >
                {showDetail ? "Hide details" : "Show details"}
              </button>
              {showDetail && (
                <div className="error-detail">{ctx.detail}</div>
              )}
            </>
          )}
        </div>
        <div className="actions">
          <button className="primary" onClick={onRetry}>
            Retry
          </button>
          <button className="danger" onClick={onReset}>
            Reset Ship
          </button>
        </div>
      </div>
      <LogPanel logs={logs} />
    </div>
  );
}

function CrashedScreen({
  status,
  logs,
  onRestart,
  onReset,
}: {
  status: StatusResponse;
  logs: string[];
  onRestart: () => void;
  onReset: () => void;
}) {
  const ctx = status.context as {
    exit_code?: number | null;
    message?: string;
  } | undefined;

  return (
    <div className="screen">
      <div className="screen-info">
        <div className="error-box crashed">
          <div className="error-title">Runtime exited unexpectedly</div>
          <div className="error-message">
            {ctx?.message ?? "The runtime process terminated"}
          </div>
          {ctx?.exit_code != null && (
            <div>
              Exit code: <span className="exit-code">{ctx.exit_code}</span>
            </div>
          )}
        </div>
        <div className="actions">
          <button className="primary" onClick={onRestart}>
            Restart
          </button>
          <button className="danger" onClick={onReset}>
            Reset Ship
          </button>
        </div>
      </div>
      <LogPanel logs={logs} />
    </div>
  );
}

// -- Main App --

function App() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [showDiagnostics, setShowDiagnostics] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const prepareCalledRef = useRef(false);

  // Poll status and logs.
  useEffect(() => {
    let active = true;
    const poll = async () => {
      while (active) {
        try {
          const [s, l] = await Promise.all([
            invoke<StatusResponse>("get_status"),
            invoke<string[]>("get_recent_logs", { count: 200 }),
          ]);
          if (active) {
            setStatus(s);
            setLogs(l);
          }
        } catch {
          // Ignore errors during polling.
        }
        await new Promise((r) => setTimeout(r, 1000));
      }
    };
    poll();
    return () => {
      active = false;
    };
  }, []);

  // Auto-prepare on first load when Uninitialized.
  useEffect(() => {
    if (
      status &&
      status.state === "Uninitialized" &&
      !preparing &&
      !prepareCalledRef.current
    ) {
      prepareCalledRef.current = true;
      setPreparing(true);
      invoke("prepare_ship")
        .catch(() => {
          // Error is reflected in state machine.
        })
        .finally(() => setPreparing(false));
    }
  }, [status, preparing]);

  const handleStart = useCallback(async () => {
    try {
      // Need to be in the right state. If Stopped or Crashed, start directly.
      await invoke("start_ship");
    } catch {
      // Error reflected in state.
    }
  }, []);

  const handleStop = useCallback(async () => {
    try {
      await invoke("stop_ship");
    } catch {
      // Error reflected in state.
    }
  }, []);

  const handleRestart = useCallback(async () => {
    try {
      await invoke("restart_ship");
    } catch {
      // Error reflected in state.
    }
  }, []);

  const handleOpenShip = useCallback(async () => {
    try {
      await invoke("open_ship");
    } catch {
      // Best-effort.
    }
  }, []);

  const handleRetry = useCallback(async () => {
    prepareCalledRef.current = false;
    // Force back to Uninitialized to re-trigger prepare.
    try {
      await invoke("reset_ship");
    } catch {
      // Error reflected in state.
    }
  }, []);

  const handleReset = useCallback(async () => {
    prepareCalledRef.current = false;
    try {
      await invoke("reset_ship");
    } catch {
      // Error reflected in state.
    }
  }, []);

  if (!status) {
    return (
      <div className="app">
        <div className="screen">
          <div className="screen-center">
            <div className="spinner" />
          </div>
        </div>
      </div>
    );
  }

  const stateName = status.state;
  const isPreparing =
    stateName === "Uninitialized" ||
    stateName === "Extracting" ||
    stateName === "Prepared" ||
    stateName === "Starting" ||
    stateName === "Stopping";

  let badgeClass = "preparing";
  let badgeLabel = stateName;
  if (stateName === "Running") {
    badgeClass = "running";
    badgeLabel = "Running";
  } else if (stateName === "Stopped") {
    badgeClass = "stopped";
    badgeLabel = "Stopped";
  } else if (stateName === "Error") {
    badgeClass = "error";
    badgeLabel = "Error";
  } else if (stateName === "Crashed") {
    badgeClass = "crashed";
    badgeLabel = "Crashed";
  }

  return (
    <div className="app">
      <div className="header">
        <div className="header-left">
          <span className="ship-name">
            {shipDisplayName(status.ship_name)}
          </span>
          <span className={`status-badge ${badgeClass}`}>
            <span
              className={`status-dot${isPreparing || stateName === "Running" ? " pulse" : ""}`}
            />
            {badgeLabel}
          </span>
        </div>
        <div className="header-right">
          <button
            className="icon-btn"
            onClick={() => setShowDiagnostics(true)}
            title="Diagnostics"
          >
            &#9881;
          </button>
        </div>
      </div>
      <div className="content">
        {isPreparing && <PreparingScreen status={status} logs={logs} />}
        {stateName === "Running" && (
          <RunningScreen
            status={status}
            logs={logs}
            onStop={handleStop}
            onRestart={handleRestart}
            onOpenShip={handleOpenShip}
          />
        )}
        {stateName === "Stopped" && (
          <StoppedScreen status={status} onStart={handleStart} />
        )}
        {stateName === "Error" && (
          <ErrorScreen
            status={status}
            logs={logs}
            onRetry={handleRetry}
            onReset={handleReset}
          />
        )}
        {stateName === "Crashed" && (
          <CrashedScreen
            status={status}
            logs={logs}
            onRestart={handleRestart}
            onReset={handleReset}
          />
        )}
      </div>
      {showDiagnostics && (
        <DiagnosticsModal onClose={() => setShowDiagnostics(false)} />
      )}
    </div>
  );
}

export default App;
