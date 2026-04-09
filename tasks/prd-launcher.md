# PRD: Urbit Ship Launcher (Desktop)

## Introduction

A Tauri + Rust desktop application that bundles a `vere` binary and an exported Urbit pier tarball into a single-click experience. The user double-clicks the app, the launcher extracts the pier, starts the runtime, and presents a simple operational dashboard. The goal for v1 is a working demo/prototype that validates the concept with stakeholders — proving that a "download, unzip, double-click, get running" handoff from Tlon hosting to self-hosting is viable.

A separate CLI/build tool (outside the scope of this PRD) assembles the personalized app bundle from an exported pier archive and a `vere` binary. This PRD covers the launcher application itself.

## Goals

- Demonstrate a complete first-launch flow: extract bundled pier, validate, start `vere`, show status
- Provide an operational dashboard with status, logs, diagnostics, and start/stop/restart controls
- Supervise `vere` as a child process with graceful lifecycle management
- Validate runtime compatibility between bundled `vere` and the exported pier
- Prove the concept is viable enough to present to stakeholders for investment decisions

## User Stories

### US-001: Project scaffolding and path resolution

**Description:** As a developer, I need the Tauri + Rust project structure and platform-aware path resolution so that all subsequent work has a foundation.

**Acceptance Criteria:**

- [ ] Tauri project scaffolded with Rust backend and a frontend shell (React/TypeScript)
- [ ] `paths` module resolves: app support dir (`~/Library/Application Support/<App>/`), pier subdir, logs subdir, and run subdir
- [ ] `bundle` module locates bundled assets within the app's resource directory: `vere` binary, pier archive, manifest
- [ ] Paths are configurable for development (e.g., env var override for app support dir)
- [ ] Typecheck/lint passes for both Rust and TypeScript

### US-002: Pier manifest parsing

**Description:** As the launcher, I need to read and validate a `pier-manifest.json` so that I can verify the bundle's integrity and display ship metadata.

**Acceptance Criteria:**

- [ ] `manifest` module deserializes `pier-manifest.json` with fields: `format_version`, `ship`, `exported_at`, `archive_name`, `archive_sha256`, `vere_version`, `launcher_min_version`, `notes`
- [ ] Missing or malformed manifest produces a typed error with a user-friendly message
- [ ] Optional fields (`notes`, `launcher_min_version`) are handled gracefully when absent
- [ ] Unit tests cover: valid manifest, missing fields, corrupt JSON

### US-003: First-run pier extraction

**Description:** As a user launching the app for the first time, I want the bundled pier to be extracted automatically so that I don't have to manage files manually.

**Acceptance Criteria:**

- [ ] `extract` module unpacks `.tar.zst` (or `.tar.gz`) archive to a temporary directory inside the app support dir
- [ ] After extraction, validates the result (pier root exists, `.urb` directory present, no unexpected nesting)
- [ ] On successful validation, atomically moves (renames) temp dir to the final `pier/` path
- [ ] Writes an `.install-marker.json` recording: extraction timestamp, archive checksum, manifest version
- [ ] If pier already exists and marker is valid, extraction is skipped on subsequent launches
- [ ] Interrupted extraction (partial temp dir) is cleaned up on next launch before retrying
- [ ] Archive checksum is verified against manifest's `archive_sha256` before extraction begins

### US-004: Pier validation

**Description:** As the launcher, I need to validate the extracted pier structure before attempting to start `vere` so that obvious problems are caught early.

**Acceptance Criteria:**

- [ ] `pier` module checks: pier directory exists, `.urb` subdirectory exists, expected layout is intact
- [ ] If `.vere.txt` is present, parses and exposes the runtime version string
- [ ] Ship name is detected from pier structure and compared against manifest's `ship` field (warn on mismatch)
- [ ] Validation failure transitions the state machine to `Error` with a specific reason

### US-005: Runtime version compatibility check

**Description:** As the launcher, I need to compare the bundled `vere` version against the pier's expected version so that mismatches are surfaced before launch.

**Acceptance Criteria:**

- [ ] Runs bundled `vere` with version flag (e.g., `vere -R`) and captures output
- [ ] Compares version output against `.vere.txt` from the extracted pier
- [ ] Exact match: proceed silently
- [ ] Minor mismatch: log a warning, proceed (warning visible in diagnostics)
- [ ] Obvious incompatibility (e.g., major version difference): transition to `Error` with explanation
- [ ] If `.vere.txt` is absent: log info, proceed without check

### US-006: Launcher state machine

**Description:** As a developer, I need an explicit state machine driving the launcher so that the UI always reflects a clear, unambiguous status.

**Acceptance Criteria:**

- [ ] `state` module defines states: `Uninitialized`, `Extracting`, `Prepared`, `Starting`, `Running`, `Stopping`, `Stopped`, `Crashed`, `Error`
- [ ] State transitions are enforced (e.g., cannot go from `Uninitialized` to `Running` directly)
- [ ] State is observable from Tauri commands (frontend can poll or subscribe)
- [ ] Each state carries relevant context: error message for `Error`, exit code for `Crashed`, progress for `Extracting`
- [ ] State is thread-safe (wrapped in `Arc<Mutex<>>` or similar)

### US-007: vere process supervision

**Description:** As a user, I want the launcher to start, monitor, and stop `vere` so that I don't have to use a terminal.

**Acceptance Criteria:**

- [ ] `runtime` module spawns `vere` as a child process with the correct arguments for booting the extracted pier
- [ ] Process handle is retained for lifecycle management
- [ ] stdout and stderr are captured and forwarded to the log system
- [ ] Process exit is detected: clean exit transitions to `Stopped`, unexpected exit transitions to `Crashed` with exit code
- [ ] `stop` sends SIGTERM, waits up to 30 seconds, then SIGKILL if needed
- [ ] `restart` performs stop then start, transitioning through the correct states
- [ ] Only one `vere` instance can run at a time (enforced by the state machine)

### US-008: Log capture and persistence

**Description:** As a user, I want launcher and `vere` logs written to disk and viewable in the UI so that I can diagnose problems.

**Acceptance Criteria:**

- [ ] `logs` module writes `vere` stdout/stderr to `~/Library/Application Support/<App>/logs/vere.log`
- [ ] Launcher events are written to `launcher.log` in the same directory
- [ ] An in-memory ring buffer (e.g., last 500 lines) is maintained for UI consumption
- [ ] Logs are timestamped
- [ ] Log files are rotated or truncated to prevent unbounded growth (simple strategy: truncate on launch if > 10 MB)

### US-009: Health and readiness detection

**Description:** As a user, I want to know when my ship is ready to use so that I can open the web UI at the right time.

**Acceptance Criteria:**

- [ ] `health` module polls the expected local HTTP port (e.g., `localhost:8080` or as configured) after `vere` starts
- [ ] Ship is considered "ready" when the port responds with a successful HTTP status
- [ ] Poll interval: every 2 seconds, timeout after 120 seconds
- [ ] Ready state is exposed to the frontend (enables the "Open Ship" button)
- [ ] If readiness times out, a warning is shown but the app stays in `Running` state (vere is still alive)

### US-010: Tauri command layer

**Description:** As a frontend developer, I need Tauri commands that expose launcher functionality so the UI can drive the app.

**Acceptance Criteria:**

- [ ] Tauri commands implemented: `prepare_ship`, `start_ship`, `stop_ship`, `restart_ship`, `get_status`, `get_recent_logs`, `open_ship`, `reveal_data_dir`, `get_diagnostics`, `reset_ship`
- [ ] `get_status` returns current state, ship name, and readiness flag
- [ ] `get_recent_logs` returns the last N lines from the ring buffer
- [ ] `get_diagnostics` returns: app support path, pier path, bundled vere version, pier vere version, current state, PID (if running), last exit code, last error
- [ ] `open_ship` opens the local ship URL in the default browser
- [ ] `reveal_data_dir` opens the app support directory in Finder
- [ ] `reset_ship` stops vere if running, removes extracted pier and install marker, returns to `Uninitialized`
- [ ] All commands return typed results (success/error) — no panics

### US-011: Preparing screen

**Description:** As a user launching the app for the first time, I want to see clear progress as my ship is being set up.

**Acceptance Criteria:**

- [ ] Displayed when state is `Uninitialized`, `Extracting`, `Prepared`, or `Starting`
- [ ] Shows a status message reflecting the current step: "Extracting ship archive...", "Validating pier...", "Checking runtime compatibility...", "Starting runtime..."
- [ ] Shows a spinner or progress indicator
- [ ] Shows recent log lines scrolling beneath the status
- [ ] On first launch, extraction + start happens automatically (no user action required)
- [ ] Verify in browser using dev tools

### US-012: Running screen

**Description:** As a user with a running ship, I want a dashboard showing status and controls.

**Acceptance Criteria:**

- [ ] Displayed when state is `Running`
- [ ] Shows: ship name (e.g., `~sample-palnet`), "Running" status with visual indicator, uptime or "since" timestamp
- [ ] Action buttons: "Open Ship" (enabled when health check passes), "Stop", "Restart"
- [ ] Collapsible/scrollable log tail panel showing recent vere output
- [ ] Ship URL displayed as text for reference
- [ ] Verify in browser using dev tools

### US-013: Stopped screen

**Description:** As a user whose ship is stopped, I want a clear way to start it again.

**Acceptance Criteria:**

- [ ] Displayed when state is `Stopped`
- [ ] Shows: ship name, "Stopped" status
- [ ] "Start" button to resume the runtime
- [ ] Link/button to view diagnostics
- [ ] Verify in browser using dev tools

### US-014: Error and Crashed screens

**Description:** As a user who hit a problem, I want to understand what went wrong and what I can do about it.

**Acceptance Criteria:**

- [ ] Error screen displayed when state is `Error`: shows a concise error summary, expandable details section with full error context, "Retry" button (re-runs preparation from current point), "Reset" button (clears pier, restarts from scratch), "Reveal Logs" button
- [ ] Crashed screen displayed when state is `Crashed`: shows "Runtime exited unexpectedly", last exit code, last few log lines, "Restart" button, "Reveal Logs" button
- [ ] Verify in browser using dev tools

### US-015: Diagnostics panel

**Description:** As a user filing a bug report, I want a diagnostics view that shows all relevant system info in one place.

**Acceptance Criteria:**

- [ ] Accessible from any screen (e.g., gear icon or menu)
- [ ] Shows: app support path, pier path, bundled vere version, pier .vere.txt version, current state, PID (if running), last exit code, last error message, manifest contents
- [ ] "Copy to Clipboard" button for easy bug report inclusion
- [ ] "Reveal Data Directory" button
- [ ] Verify in browser using dev tools

### US-016: Window close stops the ship

**Description:** As a user, when I close the app window, the ship should stop so there are no hidden background processes.

**Acceptance Criteria:**

- [ ] Closing the Tauri window triggers graceful `vere` shutdown (SIGTERM, wait, SIGKILL)
- [ ] App exits cleanly after runtime stops
- [ ] No orphaned `vere` process remains after app exit
- [ ] State is persisted (optional: write `Stopped` to state file so next launch knows it was clean)

### US-017: Single-instance guard

**Description:** As a user, if I accidentally double-click the app while it's already running, I should not get a second instance fighting over the pier.

**Acceptance Criteria:**

- [ ] Lock file or OS-level single-instance mechanism prevents two launcher instances from running simultaneously
- [ ] Second launch attempt focuses/raises the existing window (or shows a message and exits)
- [ ] Stale lock files (from a crash) are detected and cleaned up on launch

## Functional Requirements

- FR-01: The launcher must resolve platform-appropriate paths for app support, pier storage, and log output
- FR-02: The launcher must locate bundled assets (vere binary, pier archive, manifest) from the app's resource directory
- FR-03: The launcher must parse `pier-manifest.json` and validate required fields
- FR-04: The launcher must verify the pier archive's SHA-256 checksum against the manifest before extraction
- FR-05: The launcher must extract the pier archive to a temporary directory, validate the result, then atomically move it to the final location
- FR-06: The launcher must skip extraction if a valid pier and install marker already exist
- FR-07: The launcher must clean up partial/interrupted extractions on launch
- FR-08: The launcher must validate the extracted pier structure (directory exists, `.urb` present, no nesting errors)
- FR-09: The launcher must compare the bundled `vere` version against the pier's `.vere.txt` and warn or block on mismatch
- FR-10: The launcher must maintain an explicit state machine with enforced transitions
- FR-11: The launcher must spawn `vere` as a supervised child process, capturing stdout/stderr
- FR-12: The launcher must implement graceful stop (SIGTERM + timeout + SIGKILL) and restart
- FR-13: The launcher must persist logs to disk with timestamps
- FR-14: The launcher must detect runtime readiness by polling the local HTTP port
- FR-15: The launcher must expose all functionality via typed Tauri commands
- FR-16: The UI must display screens for Preparing, Running, Stopped, Error, and Crashed states
- FR-17: The UI must provide an "Open Ship" action that opens the local URL in the default browser
- FR-18: The UI must provide a diagnostics panel with system info and a copy-to-clipboard function
- FR-19: Closing the app window must trigger graceful runtime shutdown with no orphaned processes
- FR-20: The launcher must enforce single-instance execution via a lock file or OS mechanism
- FR-21: The launcher must provide a reset function that removes the extracted pier and returns to `Uninitialized`

## Non-Goals

- **Multi-ship support:** v1 is one app = one ship. No pier selection, no multi-pier management.
- **User-driven pier import:** The pier is always bundled. No "browse for archive" flow.
- **Packaging pipeline/CLI:** The build tool that assembles the app bundle is a separate effort.
- **Automatic updates:** No OTA update mechanism for vere or the pier in v1.
- **Background daemon mode:** Closing the window stops the ship. No system tray, no "keep running in background."
- **Embedded webview for ship UI:** v1 opens the ship in the default browser, not an in-app webview.
- **Linux/AppImage support:** v1 targets macOS only. Linux is a future milestone.
- **Telemetry or analytics:** No usage tracking in v1.
- **Cloud/hosting integration:** No API calls to Tlon hosting. The app is fully offline after download.
- **Code signing and notarization:** Nice-to-have for v1, not a blocker. Users may need to bypass Gatekeeper during testing.

## Design Considerations

### UI/UX

- The UI should feel calm and operational — not a complex control panel. Clear status, obvious actions, visible logs.
- Status language should be plain: "Preparing your ship", "Running", "Stopped", "Something went wrong."
- The preparing screen should auto-progress without user intervention on first launch.
- Error states should always offer a next action (retry, reset, reveal logs). Never a dead end.
- The log panel should be present but not dominant — collapsible on the Running screen.

### Visual Design

- Minimal chrome. The window can be small (e.g., 480x640).
- Ship name (`~sample-palnet`) should be prominent on the Running screen.
- Use color/iconography sparingly to indicate state: green for running, yellow for warning, red for error.

## Technical Considerations

### Tauri + Rust

- Tauri v2 recommended for better cross-platform support and updated APIs
- Frontend: React + TypeScript (lightweight, no heavy framework needed)
- State management: Rust-side state behind `Arc<Mutex<>>`, exposed via Tauri commands
- No database or persistent store beyond the filesystem (install marker, logs, optional state file)

### vere Invocation

- The exact `vere` command and flags for booting an imported pier need to be confirmed. Likely something like: `vere run <pier-path> --http-port <port> --loom <size>`
- The launcher must make the bundled `vere` binary executable (`chmod +x`) on first use if needed
- Port selection: use a fixed default (e.g., 8080) or allow manifest override. Detect port conflicts.

### Archive Format

- Prefer `.tar.zst` for better compression ratio (smaller downloads). Fall back to `.tar.gz` support.
- The archive should contain the pier directory at its root (not nested inside another directory).

### Concurrency

- Extraction and vere supervision happen on background threads; Tauri commands must not block the UI thread.
- Log streaming from the child process to the ring buffer and disk should be async.

### Dependencies (Rust)

- `serde` / `serde_json` for manifest parsing
- `tar` + `zstd` (or `flate2`) for archive extraction
- `sha2` for checksum verification
- `tokio` for async process management
- `tauri` v2 for the desktop shell

### Known Risks

- **vere flags not confirmed:** Need to verify the exact invocation for booting an exported pier. Incorrect flags could cause silent failures or data corruption. This should be resolved early in implementation.
- **Large bundle size:** A pier archive + vere binary could be hundreds of MB. Acceptable for a demo but worth noting.
- **macOS Gatekeeper:** Unsigned apps will trigger warnings. For stakeholder demos, either sign the app or provide instructions to bypass.

## Success Metrics

- A stakeholder can download a zip, extract it, double-click the app, and reach a running ship UI — with zero terminal usage and zero configuration
- First-launch extraction + boot completes without manual intervention
- Runtime crashes are surfaced in the UI with actionable information (not a silent exit)
- The diagnostics panel provides enough information for a developer to triage a bug report remotely
- The demo is convincing enough for stakeholders to greenlight investment in a production version

## Open Questions

1. **Exact `vere` invocation:** What are the correct command-line flags to boot an imported/exported pier? Does vere need `--lite`, `--prop`, or pier-type-specific flags?

> vere just needs to run `./urbit <pier-dir>`

2. **HTTP port:** Should the port be hardcoded (e.g., 8080), derived from the manifest, or auto-selected to avoid conflicts?

> sure, we can pass the flag `--http-port 8080` just to be sure

3. **Ship name detection:** How is the ship name derived from the pier structure? Is it the pier directory name, or is it embedded in metadata?

> it's usually the folder name

4. **Archive nesting:** What is the expected directory structure inside the pier archive? Is it `<ship-name>/` at root, or `pier/` at root, or flat contents?

> `<ship-name>/`

5. **App naming:** Should the app be named generically ("Urbit Launcher") or personalized per ship ("~sample-palnet")? Personalized naming affects the app support directory path.

> ideally personalized per ship

6. **Loom size:** Does the loom size need to be specified at launch, and if so, what default is appropriate?

> defaults to 2GB, should be fine

7. **First-boot vs. subsequent boots:** Does `vere` need different flags for the initial boot of an imported pier vs. subsequent boots?

> nope
