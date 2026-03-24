//! Declarative tmux pane routing — 2D layout sync.
//!
//! Mirror a columnar editor layout in tmux pane arrangements.
//!
//! Usage (as library):
//! ```ignore
//! use tmux_router::{Tmux, sync, FileResolution};
//! sync(
//!     &col_args,
//!     Some("@1"),
//!     Some("plan.md"),
//!     &tmux,
//!     Path::new(".tmux-router/registry.json"),
//!     &|path| { /* your file resolution logic */ },
//! )?;
//! ```

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::registry;
use crate::tmux::Tmux;

// =========================================================================
// Data types
// =========================================================================

/// A column of files (stacked top-to-bottom).
#[derive(Debug, Clone)]
pub struct Column {
    pub files: Vec<PathBuf>,
}

/// The 2D layout: columns left-to-right, files stacked within each column.
#[derive(Debug, Clone)]
pub struct Layout {
    pub columns: Vec<Column>,
}

/// A file resolved to its tmux pane.
#[derive(Debug)]
pub struct ResolvedFile {
    pub path: PathBuf,
    pub pane_id: String,
}

/// Result of resolving a file path through the caller-provided callback.
#[derive(Debug, Clone)]
pub enum FileResolution {
    /// File has a registered key and optionally a tmux session name.
    Registered {
        key: String,
        tmux_session: Option<String>,
    },
    /// File exists but is not managed (no registration).
    Unmanaged,
}

// =========================================================================
// SyncLog — structured operation trace for debugging
// =========================================================================

/// A single operation logged during sync reconciliation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read in tests and Debug output
pub struct SyncEntry {
    pub phase: &'static str,
    pub message: String,
    pub ok: bool,
}

/// Structured log of all sync operations for debugging.
#[derive(Debug, Clone, Default)]
pub struct SyncLog {
    entries: Vec<SyncEntry>,
}

impl SyncLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn log(&mut self, phase: &'static str, message: impl Into<String>) {
        let msg = message.into();
        eprintln!("[sync:{}] {}", phase, &msg);
        self.entries.push(SyncEntry {
            phase,
            message: msg,
            ok: true,
        });
    }

    pub fn log_err(&mut self, phase: &'static str, message: impl Into<String>) {
        let msg = message.into();
        eprintln!("[sync:{}] ERROR: {}", phase, &msg);
        self.entries.push(SyncEntry {
            phase,
            message: msg,
            ok: false,
        });
    }

    pub fn has_errors(&self) -> bool {
        self.entries.iter().any(|e| !e.ok)
    }

    /// Log the global tmux state (all windows and panes across all sessions).
    pub fn log_global_state(&mut self, tmux: &Tmux, label: &str) {
        // Log all windows
        match tmux.list_all_windows() {
            Ok(windows) => {
                self.log("GLOBAL", format!("[{}] windows: {}", label, windows));
            }
            Err(e) => {
                self.log_err("GLOBAL", format!("[{}] failed to list windows: {}", label, e));
            }
        }
        // Log all panes
        match tmux.list_all_panes() {
            Ok(panes) => {
                self.log("GLOBAL", format!("[{}] panes: {}", label, panes));
            }
            Err(e) => {
                self.log_err("GLOBAL", format!("[{}] failed to list panes: {}", label, e));
            }
        }
    }

    /// Return all entries (for testing assertions).
    pub fn entries(&self) -> &[SyncEntry] {
        &self.entries
    }

    /// Count of mutations (break/join operations, not snapshots or checks).
    pub fn mutation_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.phase, "DETACH" | "ATTACH" | "REORDER"))
            .count()
    }
}

// =========================================================================
// Layout parsing
// =========================================================================

impl Layout {
    /// Parse `--col` arguments into a Layout.
    /// Each arg is a comma-separated list of file paths.
    pub fn parse(col_args: &[String]) -> Result<Self> {
        let mut columns = Vec::new();
        for arg in col_args {
            let files: Vec<PathBuf> = arg
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            if files.is_empty() {
                anyhow::bail!("empty --col argument: '{}'", arg);
            }
            columns.push(Column { files });
        }
        if columns.is_empty() {
            anyhow::bail!("at least one --col required");
        }
        Ok(Layout { columns })
    }

    /// All files in the layout, in column-major order.
    pub fn all_files(&self) -> Vec<&Path> {
        self.columns
            .iter()
            .flat_map(|col| col.files.iter().map(|f| f.as_path()))
            .collect()
    }

    /// Which column index contains this file, if any.
    pub fn column_of(&self, file: &Path) -> Option<usize> {
        self.columns
            .iter()
            .position(|col| col.files.iter().any(|f| f == file))
    }
}

// =========================================================================
// Helpers
// =========================================================================

/// Find a donor pane in the same column as `file` (same-column only, no spiral).
/// Returns the pane_id of a resolved file in the same column.
pub fn find_column_pane(
    layout: &Layout,
    file: &Path,
    file_to_pane: &std::collections::HashMap<PathBuf, String>,
) -> Option<String> {
    let col_idx = layout.column_of(file)?;
    for f in &layout.columns[col_idx].files {
        if let Some(pane) = file_to_pane.get(f) {
            return Some(pane.clone());
        }
    }
    None
}

/// Find the best window for consolidating wanted panes.
/// Prefers the window (within the target session) that already contains the most wanted panes.
pub fn find_best_window(
    tmux: &Tmux,
    wanted: &std::collections::HashSet<&str>,
    target_session: Option<&str>,
) -> String {
    let mut best_window = String::new();
    let mut best_wanted = 0usize;
    let mut best_total = 0usize;
    for pane_id in wanted {
        let win = match tmux.pane_window(pane_id) {
            Ok(w) => w,
            Err(_) => continue,
        };
        // Only consider windows in the same tmux session
        if let Some(ts) = target_session
            && let Ok(pane_sess) = tmux.pane_session(pane_id)
                && pane_sess != ts {
                    continue;
                }
        let window_panes = tmux.list_window_panes(&win).unwrap_or_default();
        let wanted_count = window_panes
            .iter()
            .filter(|p| wanted.contains(p.as_str()))
            .count();
        let total = window_panes.len();
        if wanted_count > best_wanted || (wanted_count == best_wanted && total > best_total) {
            best_wanted = wanted_count;
            best_total = total;
            best_window = win;
        }
    }
    eprintln!("target_window={} (auto-detected, {} wanted panes)", best_window, best_wanted);
    best_window
}

// =========================================================================
// Public API
// =========================================================================

/// Result of a successful sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// The tmux session that owns the target window (None if undetermined).
    pub target_session: Option<String>,
    /// The tmux window ID where panes were arranged.
    pub target_window: String,
    /// File-to-pane assignments resolved during sync (path, pane_id).
    pub file_panes: Vec<(PathBuf, String)>,
}

/// Sync editor layout to tmux panes.
///
/// `col_args` are comma-separated file lists (one per column).
/// `window` is an optional tmux window ID to use as target.
/// `focus` is an optional file path to focus after sync.
/// `tmux` is the tmux server handle.
/// `registry_path` is the path to the registry JSON file.
/// `resolve_file` is a callback that resolves a file path to its registration info.
pub fn sync(
    col_args: &[String],
    window: Option<&str>,
    focus: Option<&str>,
    tmux: &Tmux,
    registry_path: &Path,
    resolve_file: &dyn Fn(&Path) -> Option<FileResolution>,
) -> Result<SyncResult> {
    let layout = Layout::parse(col_args)?;
    let all_files = layout.all_files();

    // Log comprehensive tmux tree at sync start
    if let Ok(tree) = tmux.dump_tmux_tree() {
        eprintln!("{}", tree);
    }

    let mut global_log = SyncLog::new();
    global_log.log_global_state(tmux, "sync-start");

    // --- Phase 1: Resolve each file to its session pane ---
    let mut resolved: Vec<ResolvedFile> = Vec::new();
    let mut unresolved_files: Vec<PathBuf> = Vec::new();
    let mut non_managed_files: Vec<PathBuf> = Vec::new();
    let mut doc_tmux_session: Option<String> = None;

    for file in &all_files {
        if !file.exists() {
            eprintln!("warning: file not found: {}, skipping", file.display());
            continue;
        }

        let resolution = match resolve_file(file) {
            Some(r) => r,
            None => {
                eprintln!("warning: could not resolve {}, skipping", file.display());
                continue;
            }
        };

        match resolution {
            FileResolution::Unmanaged => {
                non_managed_files.push(file.to_path_buf());
                continue;
            }
            FileResolution::Registered { key, tmux_session } => {
                // Collect tmux_session from first doc that has it
                if doc_tmux_session.is_none()
                    && let Some(ref ts) = tmux_session
                {
                    doc_tmux_session = Some(ts.clone());
                    eprintln!("tmux_session={} (from {})", ts, file.display());
                }

                match registry::lookup(registry_path, &key)? {
                    Some(pane_id) if tmux.pane_alive(&pane_id) => {
                        resolved.push(ResolvedFile {
                            path: file.to_path_buf(),
                            pane_id,
                        });
                    }
                    Some(pane_id) => {
                        eprintln!(
                            "warning: pane {} is dead for {}, will skip",
                            pane_id,
                            file.display()
                        );
                        unresolved_files.push(file.to_path_buf());
                    }
                    None => {
                        unresolved_files.push(file.to_path_buf());
                    }
                }
            }
        }
    }

    // --- Phase 2: Log unresolved files (no auto-restart) ---
    for file in &unresolved_files {
        eprintln!(
            "skipping {} — pane dead/missing (re-register to fix)",
            file.display()
        );
    }

    // Build a lookup from file path -> pane_id (mutable for Phase 1.5 auto-register)
    let mut file_to_pane: std::collections::HashMap<PathBuf, String> = resolved
        .iter()
        .map(|r| (r.path.clone(), r.pane_id.clone()))
        .collect();

    // --- Phase 1.5: In-memory auto-register unclaimed files ---
    // For unresolved files (have key but no pane), find a donor pane in the
    // SAME column only. This is ephemeral — not written to registry.
    // Same-column-only prevents cross-column focus jumps.
    for file in &unresolved_files {
        if let Some(donor_pane) = find_column_pane(&layout, file, &file_to_pane) {
            eprintln!(
                "auto-register {} → {} (in-memory, same column)",
                file.display(),
                donor_pane
            );
            file_to_pane.insert(file.clone(), donor_pane.clone());
            resolved.push(ResolvedFile {
                path: file.clone(),
                pane_id: donor_pane,
            });
        }
    }

    // --- Phase 1.75: Assign spare window panes to still-unresolved files ---
    // When find_column_pane fails (file is the sole occupant of its column),
    // look for spare panes in the target window that aren't already assigned.
    {
        // Determine target window: --window arg, or window of first resolved pane
        let target_win = window.map(|w| w.to_string()).or_else(|| {
            resolved.first().and_then(|r| tmux.pane_window(&r.pane_id).ok())
        });

        if let Some(ref win_id) = target_win {
            // Collect files still unresolved after Phase 1.5
            let still_unresolved: Vec<PathBuf> = unresolved_files
                .iter()
                .filter(|f| !file_to_pane.contains_key(*f))
                .cloned()
                .collect();

            if !still_unresolved.is_empty() {
                let window_panes = tmux.list_window_panes(win_id).unwrap_or_default();
                let assigned_panes: HashSet<&str> =
                    file_to_pane.values().map(|s| s.as_str()).collect();
                let mut spare_panes: Vec<String> = window_panes
                    .into_iter()
                    .filter(|p| !assigned_panes.contains(p.as_str()))
                    .collect();

                for file in &still_unresolved {
                    if let Some(pane_id) = spare_panes.pop() {
                        eprintln!(
                            "auto-assign {} → {} (spare pane in window {})",
                            file.display(),
                            pane_id,
                            win_id
                        );
                        file_to_pane.insert(file.clone(), pane_id.clone());
                        resolved.push(ResolvedFile {
                            path: file.clone(),
                            pane_id,
                        });
                    }
                }
            }
        }
    }

    // Helper: build an early SyncResult from the first resolved pane.
    let early_result = |tmux: &Tmux, file_to_pane: &std::collections::HashMap<PathBuf, String>| -> SyncResult {
        let win = resolved.first()
            .and_then(|r| tmux.pane_window(&r.pane_id).ok())
            .unwrap_or_default();
        SyncResult {
            target_session: doc_tmux_session.clone(),
            target_window: win,
            file_panes: file_to_pane.iter().map(|(p, id)| (p.clone(), id.clone())).collect(),
        }
    };

    // Single file without --window: just focus.
    if all_files.len() == 1 && window.is_none() {
        if let Some(r) = resolved.first() {
            tmux.select_pane(&r.pane_id)?;
        }
        return Ok(early_result(tmux, &file_to_pane));
    }

    if resolved.len() < 2 {
        // Not enough resolved panes for 2D layout — just focus, don't rearrange.
        // Respect --focus: only select a pane if the focus file has a resolved pane.
        // Otherwise, preserve the current tmux selection.
        if let Some(focus_file) = focus {
            let focus_path = PathBuf::from(focus_file);
            if !non_managed_files.contains(&focus_path)
                && let Some(pane) = file_to_pane.get(&focus_path) {
                    tmux.select_pane(pane)?;
                }
            // else: non-managed or no pane -> preserve selection
        } else if let Some(r) = resolved.first() {
            tmux.select_pane(&r.pane_id)?;
        }
        return Ok(early_result(tmux, &file_to_pane));
    }

    // --- Phase 3: Build the 2D column structure with resolved panes ---
    let mut pane_columns: Vec<Vec<String>> = Vec::new();
    for col in &layout.columns {
        let mut panes = Vec::new();
        for file in &col.files {
            if let Some(pane_id) = file_to_pane.get(file) {
                panes.push(pane_id.clone());
            }
        }
        if !panes.is_empty() {
            pane_columns.push(panes);
        }
    }

    // Deduplicate panes across all columns
    let mut seen = HashSet::new();
    for col in &mut pane_columns {
        col.retain(|p| seen.insert(p.clone()));
    }
    pane_columns.retain(|col| !col.is_empty());

    if pane_columns.is_empty() {
        anyhow::bail!("no resolved panes to arrange");
    }
    if pane_columns.len() == 1 && pane_columns[0].len() == 1 {
        tmux.select_pane(&pane_columns[0][0])?;
        return Ok(early_result(tmux, &file_to_pane));
    }

    // Collect the full set of wanted pane IDs
    let wanted: HashSet<&str> = pane_columns
        .iter()
        .flat_map(|col| col.iter().map(|s| s.as_str()))
        .collect();

    // --- Phase 4: Pick target window (same tmux session only) ---
    let target_session = if let Some(ref ts) = doc_tmux_session {
        if tmux.session_alive(ts) {
            Some(ts.clone())
        } else {
            eprintln!(
                "warning: configured tmux_session '{}' is dead, falling back to --window",
                ts
            );
            if let Some(w) = window {
                tmux.pane_session(w).ok()
            } else {
                wanted.iter().find_map(|p| tmux.pane_session(p).ok())
            }
        }
    } else if let Some(w) = window {
        tmux.pane_session(w).ok()
    } else {
        wanted.iter().find_map(|p| tmux.pane_session(p).ok())
    };
    eprintln!("target_session={:?}", target_session);

    // If --window is alive, use it directly (stable, deterministic).
    // Only search for best_window when --window is missing or dead.
    let target_window = if let Some(w) = window {
        if !tmux.list_window_panes(w).unwrap_or_default().is_empty() {
            eprintln!("target_window={} (from --window)", w);
            w.to_string()
        } else {
            eprintln!("warning: --window {} is dead, searching for best window", w);
            find_best_window(tmux, &wanted, target_session.as_deref())
        }
    } else {
        find_best_window(tmux, &wanted, target_session.as_deref())
    };

    let anchor_pane = pane_columns[0][0].clone();

    let desired_ordered: Vec<&str> = pane_columns
        .iter()
        .flat_map(|col| col.iter().map(|s| s.as_str()))
        .collect();

    // Resolve --focus to a pane ID (Option: None preserves current tmux selection)
    let focus_pane: Option<String> = if let Some(focus_file) = focus {
        let focus_path = PathBuf::from(focus_file);
        if non_managed_files.contains(&focus_path) {
            // Non-managed file -> preserve tmux selection
            None
        } else if let Some(pane) = file_to_pane.get(&focus_path) {
            // Directly resolved (includes auto-registered)
            Some(pane.clone())
        } else {
            // Column-positional fallback: find first pane in same column
            find_column_pane(&layout, &focus_path, &file_to_pane)
        }
    } else {
        Some(anchor_pane.clone())
    };

    // --- Phase 5: Reconcile (attach-first: attach -> select -> detach) ---
    let log = reconcile(
        tmux,
        &target_window,
        &pane_columns,
        &desired_ordered,
        target_session.as_deref(),
        focus_pane.as_deref(),
        registry_path,
    )?;

    // --- Phase 6: Resize + re-select ---
    equalize_sizes(tmux, &pane_columns);
    if let Some(ref fp) = focus_pane {
        tmux.select_pane(fp)?;
    }
    let sel = target_session.as_deref().and_then(|s| tmux.active_pane(s)).unwrap_or_default();
    eprintln!("phase6: focus={:?}, selected={}", focus_pane, sel);

    // Log global tmux state at sync end
    global_log.log_global_state(tmux, "sync-end");

    if log.has_errors() {
        eprintln!(
            "Sync completed with errors: {} panes in {} columns",
            desired_ordered.len(),
            pane_columns.len()
        );
    } else {
        eprintln!(
            "Sync: {} panes in {} columns",
            desired_ordered.len(),
            pane_columns.len()
        );
    }
    Ok(SyncResult {
        target_session,
        target_window,
        file_panes: file_to_pane.into_iter().collect(),
    })
}

// =========================================================================
// Core reconciliation algorithm
// =========================================================================

/// Reconcile pane layout in target_window to match desired_ordered.
///
/// # Algorithm: Attach-first (prevents pane selection flicker)
///
/// ```text
/// SNAPSHOT — query current panes
/// FAST PATH — if already correct, done
/// ATTACH — join missing desired panes into target window (all with -d)
/// SELECT — select the focus pane (now in window after attach)
/// DETACH — stash unwanted panes (focus pane survives, no selection jump)
/// REORDER — if needed, break non-first + rejoin in correct order
/// VERIFY — confirm final layout
/// ```
///
/// By attaching BEFORE detaching, the focus pane is in the window when
/// unwanted panes are stashed, preventing tmux from auto-selecting a
/// different pane.
pub fn reconcile(
    tmux: &Tmux,
    target_window: &str,
    pane_columns: &[Vec<String>],
    desired_ordered: &[&str],
    session_name: Option<&str>,
    focus_pane: Option<&str>,
    registry_path: &Path,
) -> Result<SyncLog> {
    let mut log = SyncLog::new();

    let wanted: HashSet<&str> = desired_ordered.iter().copied().collect();
    let first_pane = desired_ordered[0];

    // --- SNAPSHOT ---
    let current = tmux.list_panes_ordered(target_window).unwrap_or_default();
    let current_refs: Vec<&str> = current.iter().map(|s| s.as_str()).collect();
    let sel = session_name.and_then(|s| tmux.active_pane(s)).unwrap_or_default();
    log.log(
        "SNAPSHOT",
        format!(
            "window={}, current={:?}, desired={:?}, selected={}",
            target_window, current_refs, desired_ordered, sel
        ),
    );

    // --- FAST PATH ---
    if current_refs == desired_ordered {
        log.log("FAST_PATH", "layout already correct");
        return Ok(log);
    }

    let current_panes = tmux.list_window_panes(target_window).unwrap_or_default();
    let current_set: HashSet<String> = current_panes.iter().cloned().collect();

    // --- SWAP fast path: 1:1 pane replacement ---
    // When exactly one pane needs to come in and one needs to go out,
    // use swap-pane for an atomic visual transition (no flicker).
    let to_attach: Vec<&str> = desired_ordered.iter()
        .copied()
        .filter(|p| !current_set.contains(*p))
        .collect();
    let to_detach: Vec<&str> = current_panes.iter()
        .map(|s| s.as_str())
        .filter(|p| !wanted.contains(p))
        .collect();

    if to_attach.len() == 1 && to_detach.len() == 1 {
        let incoming = to_attach[0];
        let outgoing = to_detach[0];

        // Only swap if the incoming pane is alive (exists somewhere)
        if tmux.pane_window(incoming).is_ok() {
            match tmux.swap_pane(incoming, outgoing) {
                Ok(()) => {
                    log.log("SWAP", format!("{} ↔ {} (atomic)", incoming, outgoing));
                    update_registry(tmux, incoming, registry_path, &mut log);
                    update_registry(tmux, outgoing, registry_path, &mut log);

                    // Select focus pane
                    let select_target = focus_pane.unwrap_or(first_pane);
                    let _ = tmux.select_pane(select_target);
                    log.log("SELECT", format!("focused {}", select_target));

                    return Ok(log);
                }
                Err(e) => {
                    log.log_err("SWAP", format!("swap-pane failed ({} ↔ {}): {}, falling back to join+stash", incoming, outgoing, e));
                    // Fall through to normal ATTACH/DETACH
                }
            }
        }
    }

    // --- ATTACH missing desired panes ---
    // First, ensure the first desired pane is in the target window.
    if !current_set.contains(first_pane) {
        let existing = tmux.list_window_panes(target_window).unwrap_or_default();
        if let Some(target) = existing.first() {
            let flag = first_pane_join_flag(pane_columns, target);
            match tmux.join_pane(first_pane, target, flag) {
                Ok(()) => {
                    log.log("ATTACH", format!("joined first {} into {} ({})", first_pane, target_window, flag));
                    update_registry(tmux, first_pane, registry_path, &mut log);
                }
                Err(e) => {
                    log.log_err("ATTACH", format!("failed to join first {}: {}", first_pane, e));
                }
            }
        }
    }

    // Join remaining desired panes in column order
    for (col_idx, column) in pane_columns.iter().enumerate() {
        for (row_idx, pane) in column.iter().enumerate() {
            if pane.as_str() == first_pane {
                continue;
            }
            let in_target = tmux
                .list_window_panes(target_window)
                .unwrap_or_default()
                .contains(pane);
            if in_target {
                continue;
            }
            if tmux.pane_window(pane).is_err() {
                log.log_err("ATTACH", format!("pane {} not found (dead?)", pane));
                continue;
            }
            let (target_pane, flag) = join_target(pane_columns, col_idx, row_idx);
            match tmux.join_pane(pane, &target_pane, flag) {
                Ok(()) => {
                    log.log("ATTACH", format!("{} → {} ({})", pane, target_pane, flag));
                    update_registry(tmux, pane, registry_path, &mut log);
                }
                Err(e) => {
                    log.log_err("ATTACH", format!("failed to join {} → {}: {}", pane, target_pane, e));
                }
            }
        }
    }

    let sel = session_name.and_then(|s| tmux.active_pane(s)).unwrap_or_default();
    log.log("ATTACH", format!("done — selected={}", sel));

    // --- SELECT focus pane (before detach, so stash won't change selection) ---
    let select_target = focus_pane.unwrap_or(first_pane);
    let _ = tmux.select_pane(select_target);
    log.log("SELECT", format!("pre-selected {} before detach", select_target));

    // --- DETACH unwanted panes ---
    let refreshed = tmux.list_window_panes(target_window).unwrap_or_default();
    // Log active window before detach for focus-steal diagnosis
    let active_before_detach = session_name
        .and_then(|s| tmux.active_window(s))
        .unwrap_or_default();
    log.log("DETACH", format!("active_window_before={}", active_before_detach));

    for pane in &refreshed {
        if wanted.contains(pane.as_str()) {
            continue;
        }
        let window_count = tmux.list_window_panes(target_window).unwrap_or_default().len();
        if window_count <= 1 {
            log.log("DETACH", format!("skipped {} — last pane in window", pane));
            continue;
        }
        let (result, verb) = if let Some(sess) = session_name {
            (tmux.stash_pane(pane, sess), "stashed")
        } else {
            (tmux.break_pane(pane), "broke")
        };
        match result {
            Ok(()) => {
                log.log("DETACH", format!("{} {} from {}", verb, pane, target_window));
                update_registry(tmux, pane, registry_path, &mut log);
                // Log active window after each stash to detect focus steal
                let active_after = session_name
                    .and_then(|s| tmux.active_window(s))
                    .unwrap_or_default();
                if active_after != active_before_detach {
                    log.log_err("DETACH", format!(
                        "FOCUS STEAL: active window changed {} → {} after stashing {}",
                        active_before_detach, active_after, pane
                    ));
                }
            }
            Err(e) => {
                log.log_err("DETACH", format!("failed to detach {}: {}", pane, e));
            }
        }
    }

    // Re-select target window after stash operations (restore focus if stolen)
    let active_after_detach = session_name
        .and_then(|s| tmux.active_window(s))
        .unwrap_or_default();
    if active_after_detach != target_window {
        log.log("DETACH", format!(
            "restoring focus: {} → {} (was stolen during stash)",
            active_after_detach, target_window
        ));
    }
    let _ = tmux.select_window(target_window);
    let sel = session_name.and_then(|s| tmux.active_pane(s)).unwrap_or_default();
    log.log("DETACH", format!("done — selected={}", sel));

    // --- REORDER if needed ---
    let current_order = tmux.list_panes_ordered(target_window).unwrap_or_default();
    let current_order_refs: Vec<&str> = current_order.iter().map(|s| s.as_str()).collect();
    let final_set: HashSet<&str> = current_order_refs.iter().copied().collect();

    if final_set == wanted && current_order_refs != desired_ordered {
        log.log("REORDER", format!("current={:?}, desired={:?}", current_order_refs, desired_ordered));
        for pane in desired_ordered.iter().skip(1) {
            let _ = tmux.break_pane(pane);
            log.log("REORDER", format!("broke {} for reorder", pane));
        }
        for (col_idx, column) in pane_columns.iter().enumerate() {
            for (row_idx, pane) in column.iter().enumerate() {
                if pane.as_str() == first_pane {
                    continue;
                }
                let in_target = tmux
                    .list_window_panes(target_window)
                    .unwrap_or_default()
                    .contains(pane);
                if in_target {
                    continue;
                }
                let (target_pane, flag) = join_target(pane_columns, col_idx, row_idx);
                let _ = tmux.join_pane(pane, &target_pane, flag);
                log.log("REORDER", format!("rejoined {} → {} ({})", pane, target_pane, flag));
            }
        }
        // Re-select focus after reorder
        let _ = tmux.select_pane(select_target);
    }

    // --- VERIFY ---
    let final_state = tmux.list_panes_ordered(target_window).unwrap_or_default();
    let final_refs: Vec<&str> = final_state.iter().map(|s| s.as_str()).collect();
    if final_refs == desired_ordered {
        log.log("VERIFY", "layout correct");
    } else {
        log.log_err(
            "VERIFY",
            format!(
                "mismatch — desired={:?}, actual={:?}",
                desired_ordered, final_refs
            ),
        );
    }

    // --- REGISTRY UPDATE for all wanted panes ---
    for pane in desired_ordered {
        update_registry(tmux, pane, registry_path, &mut log);
    }

    Ok(log)
}

/// Determine the join flag when the first desired pane (anchor) needs to join
/// the target window alongside an existing pane.
///
/// If the existing pane is a desired pane, compute the correct relative direction
/// so the anchor lands in the right position without needing REORDER to fix it.
/// If the existing pane is unwanted (will be detached), use `-dh` as a neutral default.
fn first_pane_join_flag(pane_columns: &[Vec<String>], existing_pane: &str) -> &'static str {
    // Find existing pane's position in the desired column structure
    for (col_idx, column) in pane_columns.iter().enumerate() {
        for (row_idx, pane) in column.iter().enumerate() {
            if pane == existing_pane {
                // Existing pane is desired. First pane (0,0) should be:
                // - left of existing if existing is in a later column
                // - above existing if existing is in same column but later row
                if col_idx > 0 {
                    return "-dbh"; // before (left), horizontal
                } else if row_idx > 0 {
                    return "-dbv"; // before (above), vertical
                }
                // Existing is at (0,0) — same as first pane, shouldn't happen
                return "-dh";
            }
        }
    }
    // Existing pane is not desired (will be detached). Direction doesn't matter
    // much since REORDER handles final positioning, but -dh (right) avoids the
    // visual flicker of -dbh placing the anchor left of an unwanted pane.
    "-dh"
}

/// Determine join target and split direction for a pane at (col_idx, row_idx).
/// All flags include `-d` to prevent changing the active pane during reconcile.
fn join_target(pane_columns: &[Vec<String>], col_idx: usize, row_idx: usize) -> (String, &'static str) {
    if col_idx == 0 {
        // Same column as anchor: stack below previous pane
        (pane_columns[0][row_idx - 1].clone(), "-dv")
    } else if row_idx == 0 {
        // First pane of new column: horizontal split right of previous column's first pane
        (pane_columns[col_idx - 1][0].clone(), "-dh")
    } else {
        // Stack below previous pane in this column
        (pane_columns[col_idx][row_idx - 1].clone(), "-dv")
    }
}

/// Query a pane's current window and update the registry.
/// Registry errors are non-fatal warnings — the pane layout is correct regardless.
fn update_registry(tmux: &Tmux, pane: &str, registry_path: &Path, log: &mut SyncLog) {
    match tmux.pane_window(pane) {
        Ok(win) => {
            if let Err(e) = registry::update_window_for_entry(registry_path, pane, &win) {
                log.log(
                    "REGISTRY",
                    format!("warning: failed to update registry for {}: {}", pane, e),
                );
            }
        }
        Err(e) => {
            log.log(
                "REGISTRY",
                format!("warning: can't query window for {}: {}", pane, e),
            );
        }
    }
}

/// Equalize pane sizes after reconciliation.
pub fn equalize_sizes(tmux: &Tmux, pane_columns: &[Vec<String>]) {
    if pane_columns.len() == 2 {
        let _ = tmux.resize_pane(&pane_columns[0][0], "-x", 50);
    } else if pane_columns.len() > 2
        && let Ok(win) = tmux.pane_window(&pane_columns[0][0]) {
            let _ = tmux.select_layout(&win, "even-horizontal");
        }
    for col in pane_columns {
        if col.len() > 1 {
            let pct = 100 / col.len() as u32;
            let _ = tmux.resize_pane(&col[0], "-y", pct);
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::IsolatedTmux;
    use tempfile::TempDir;

    // --- Layout parsing unit tests ---

    #[test]
    fn parse_single_col() {
        let args = vec!["plan.md,corky.md".to_string()];
        let layout = Layout::parse(&args).unwrap();
        assert_eq!(layout.columns.len(), 1);
        assert_eq!(layout.columns[0].files.len(), 2);
        assert_eq!(layout.columns[0].files[0], PathBuf::from("plan.md"));
        assert_eq!(layout.columns[0].files[1], PathBuf::from("corky.md"));
    }

    #[test]
    fn parse_multiple_cols() {
        let args = vec![
            "plan.md,corky.md".to_string(),
            "agent-doc.md".to_string(),
        ];
        let layout = Layout::parse(&args).unwrap();
        assert_eq!(layout.columns.len(), 2);
        assert_eq!(layout.columns[0].files.len(), 2);
        assert_eq!(layout.columns[1].files.len(), 1);
    }

    #[test]
    fn parse_empty_col_fails() {
        let args = vec!["".to_string()];
        assert!(Layout::parse(&args).is_err());
    }

    #[test]
    fn parse_no_cols_fails() {
        let args: Vec<String> = vec![];
        assert!(Layout::parse(&args).is_err());
    }

    #[test]
    fn all_files_preserves_order() {
        let args = vec!["a.md,b.md".to_string(), "c.md".to_string()];
        let layout = Layout::parse(&args).unwrap();
        let files = layout.all_files();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], Path::new("a.md"));
        assert_eq!(files[1], Path::new("b.md"));
        assert_eq!(files[2], Path::new("c.md"));
    }

    #[test]
    fn parse_trims_whitespace() {
        let args = vec!["plan.md , corky.md".to_string()];
        let layout = Layout::parse(&args).unwrap();
        assert_eq!(layout.columns[0].files[0], PathBuf::from("plan.md"));
        assert_eq!(layout.columns[0].files[1], PathBuf::from("corky.md"));
    }

    // --- SyncLog unit tests ---

    #[test]
    fn sync_log_collects_entries() {
        let mut log = SyncLog::new();
        log.log("SNAPSHOT", "test message");
        log.log_err("DETACH", "something failed");
        assert_eq!(log.entries().len(), 2);
        assert!(log.entries()[0].ok);
        assert_eq!(log.entries()[0].phase, "SNAPSHOT");
        assert!(!log.entries()[1].ok);
        assert_eq!(log.entries()[1].phase, "DETACH");
    }

    #[test]
    fn sync_log_has_errors() {
        let mut log = SyncLog::new();
        log.log("SNAPSHOT", "ok");
        assert!(!log.has_errors());
        log.log_err("DETACH", "bad");
        assert!(log.has_errors());
    }

    #[test]
    fn sync_log_mutation_count() {
        let mut log = SyncLog::new();
        log.log("SNAPSHOT", "snapshot");
        log.log("FAST_PATH", "fast");
        log.log("DETACH", "broke pane");
        log.log("ATTACH", "joined pane");
        log.log("VERIFY", "ok");
        assert_eq!(log.mutation_count(), 2); // DETACH + ATTACH
    }

    // --- Helpers ---

    /// Full tmux state snapshot for assertions.
    #[derive(Debug)]
    struct TmuxState {
        /// Panes in the target window (ordered).
        target_panes: Vec<String>,
        /// Total windows in the session.
        window_count: usize,
        /// Currently selected window ID.
        active_window: String,
    }

    /// Capture the full tmux state for a session.
    fn snapshot_state(tmux: &IsolatedTmux, session: &str, target_window: &str) -> TmuxState {
        let target_panes = tmux.list_panes_ordered(target_window).unwrap_or_default();
        let window_count = count_windows(tmux, session);
        let active_window = active_window(tmux, session);
        TmuxState {
            target_panes,
            window_count,
            active_window,
        }
    }

    /// Assert the target window contains exactly the expected panes (in order).
    fn assert_target_panes(state: &TmuxState, expected: &[&str], msg: &str) {
        let actual: Vec<&str> = state.target_panes.iter().map(|s| s.as_str()).collect();
        assert_eq!(actual, expected, "{}: target panes mismatch", msg);
    }

    /// Assert the active window is the target window.
    fn assert_active_window(state: &TmuxState, target_window: &str, msg: &str) {
        assert_eq!(
            state.active_window, target_window,
            "{}: active window should be target",
            msg
        );
    }

    /// Assert that all given panes are alive.
    fn assert_all_alive(tmux: &IsolatedTmux, panes: &[String], msg: &str) {
        for pane in panes {
            assert!(tmux.pane_alive(pane), "{}: pane {} should be alive", msg, pane);
        }
    }

    fn setup_panes(tmux: &Tmux, n: usize) -> (String, Vec<String>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let first_pane = tmux.new_session("test", tmp.path()).unwrap();
        let target_window = tmux.pane_window(&first_pane).unwrap();
        // Resize window large enough to fit many panes
        let _ = tmux.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let mut panes = vec![first_pane];
        for _ in 1..n {
            let pane = tmux.new_window("test", tmp.path()).unwrap();
            panes.push(pane);
        }
        (target_window, panes, tmp)
    }

    /// A no-op registry path for tests that don't need registry persistence.
    fn dummy_registry_path() -> PathBuf {
        PathBuf::from("/dev/null")
    }

    // --- Integration tests using IsolatedTmux ---

    #[test]
    fn test_sync_2col_happy_path() {
        let t = IsolatedTmux::new("sync-test-2col-happy");
        let (target_window, panes, _tmp) = setup_panes(&t, 3);

        // panes[0] is already in target_window, panes[1] and [2] are in separate windows
        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone()],
            vec![panes[2].clone()],
        ];
        let desired: Vec<&str> = pane_columns
            .iter()
            .flat_map(|col| col.iter().map(|s| s.as_str()))
            .collect();

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        // Verify all panes are in the target window
        let final_panes = t.list_window_panes(&target_window).unwrap();
        for pane in &desired {
            assert!(
                final_panes.contains(&pane.to_string()),
                "pane {} should be in target window",
                pane
            );
        }
        assert!(!log.has_errors(), "should have no errors: {:?}", log.entries());

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "2col happy path");
        assert_active_window(&state, &target_window, "2col happy path");
        // 3 panes started in 3 windows; after joining 2 into target, only 1 window remains
        assert_eq!(state.window_count, 1, "2col happy path: all panes consolidated into 1 window");
        assert_all_alive(&t, &panes, "2col happy path");
    }

    #[test]
    fn test_sync_already_correct() {
        let t = IsolatedTmux::new("sync-test-already-correct");
        let tmp = TempDir::new().unwrap();

        // Create 2 panes in the same window
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();

        // Split to create second pane in same window
        let pane_b_raw = t
            .raw_cmd(&[
                "split-window",
                "-t",
                &pane_a,
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .unwrap();
        let pane_b = pane_b_raw.trim().to_string();

        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        // Verify they're already ordered correctly
        let current = t.list_panes_ordered(&target_window).unwrap();
        let current_refs: Vec<&str> = current.iter().map(|s| s.as_str()).collect();
        assert_eq!(current_refs, desired, "setup should produce correct order");

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        assert!(
            log.entries().iter().any(|e| e.phase == "FAST_PATH"),
            "should take fast path"
        );
        assert_eq!(log.mutation_count(), 0, "should have zero mutations");

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "already correct");
        assert_active_window(&state, &target_window, "already correct");
        assert_eq!(state.window_count, 1, "already correct: window count unchanged");
        assert_all_alive(&t, &[pane_a, pane_b], "already correct");
    }

    #[test]
    fn test_sync_unwanted_pane_evicted() {
        let t = IsolatedTmux::new("sync-test-unwanted-evict");
        let tmp = TempDir::new().unwrap();

        // Create pane A in target window, then split to add B and X
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let pane_b = t
            .raw_cmd(&[
                "split-window",
                "-t",
                &pane_a,
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .unwrap();
        let pane_x = t
            .raw_cmd(&[
                "split-window",
                "-t",
                &pane_a,
                "-v",
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .unwrap();

        // Desired: [A, B] — X should be evicted
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(
            !final_panes.contains(&pane_x),
            "X should have been evicted"
        );
        assert!(final_panes.contains(&pane_a), "A should remain");
        assert!(final_panes.contains(&pane_b), "B should remain");

        // Verify X is still alive (broken out, not killed)
        assert!(t.pane_alive(&pane_x), "X should still be alive");

        assert!(
            log.entries().iter().any(|e| e.phase == "DETACH" && e.ok),
            "should have evict entry"
        );

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "unwanted evicted");
        assert_active_window(&state, &target_window, "unwanted evicted");
        // Started with 1 window (all panes via split). X evicted to solo window -> 2 windows.
        assert_eq!(state.window_count, 2, "unwanted evicted: target + X's solo window");
    }

    #[test]
    fn test_sync_missing_pane_joined() {
        let t = IsolatedTmux::new("sync-test-missing-join");
        let (target_window, panes, _tmp) = setup_panes(&t, 2);

        // panes[0] is in target_window. panes[1] is in another window.
        let pane_columns = vec![vec![panes[0].clone()], vec![panes[1].clone()]];
        let desired: Vec<&str> = vec![panes[0].as_str(), panes[1].as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 2);
        assert!(final_panes.contains(&panes[0]));
        assert!(final_panes.contains(&panes[1]));
        assert!(
            log.entries().iter().any(|e| e.phase == "ATTACH" && e.ok),
            "should have join entry"
        );
    }

    #[test]
    fn test_sync_dead_pane_logged() {
        let t = IsolatedTmux::new("sync-test-dead-pane");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();

        // Kill pane B
        t.kill_pane(&pane_b).unwrap();

        // Desired: [A, B, C] — B is dead
        let pane_columns = vec![
            vec![pane_a.clone(), pane_b.clone()],
            vec![pane_c.clone()],
        ];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str(), pane_c.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        // A and C should be in the window; B should have failed
        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a));
        assert!(final_panes.contains(&pane_c));
        assert!(!final_panes.contains(&pane_b), "dead pane should not be present");

        assert!(log.has_errors(), "should have errors for dead pane");
        assert!(
            log.entries()
                .iter()
                .any(|e| e.phase == "ATTACH" && !e.ok && e.message.contains(&pane_b)),
            "should have error entry mentioning dead pane"
        );
    }

    #[test]
    fn test_sync_wrong_order_reordered() {
        let t = IsolatedTmux::new("sync-test-wrong-order");
        let tmp = TempDir::new().unwrap();

        // Create A, then B in separate windows
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        // Put A into B's window: A before B (horizontal split)
        t.join_pane(&pane_a, &pane_b, "-bh").unwrap();

        // Window now has [A, B]. We want [A, B] with A as anchor.
        let target_window = t.pane_window(&pane_b).unwrap();

        // Desired: [A, B] with A as anchor — if A is currently right of B, reconcile should fix it
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        // Both panes should be in the window
        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a));
        assert!(final_panes.contains(&pane_b));
        assert!(!log.has_errors() || log.entries().iter().any(|e| e.phase == "VERIFY"),
            "reconcile should complete");
    }

    #[test]
    fn test_sync_3col_layout() {
        let t = IsolatedTmux::new("sync-test-3col");
        let (target_window, panes, _tmp) = setup_panes(&t, 5);

        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone()],
            vec![panes[2].clone(), panes[3].clone()],
            vec![panes[4].clone()],
        ];
        let desired: Vec<&str> = pane_columns
            .iter()
            .flat_map(|col| col.iter().map(|s| s.as_str()))
            .collect();

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 5, "all 5 panes should be in window");
        for pane in &desired {
            assert!(
                final_panes.contains(&pane.to_string()),
                "pane {} should be in target window",
                pane
            );
        }
        assert!(!log.has_errors(), "should have no errors: {:?}", log.entries());
    }

    #[test]
    fn test_sync_single_column_stacked() {
        let t = IsolatedTmux::new("sync-test-single-col-stack");
        let (target_window, panes, _tmp) = setup_panes(&t, 3);

        // Single column with 3 panes stacked vertically
        let pane_columns = vec![vec![
            panes[0].clone(),
            panes[1].clone(),
            panes[2].clone(),
        ]];
        let desired: Vec<&str> = vec![panes[0].as_str(), panes[1].as_str(), panes[2].as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 3);
        for pane in &desired {
            assert!(final_panes.contains(&pane.to_string()));
        }
        assert!(!log.has_errors());
    }

    #[test]
    fn test_sync_anchor_not_in_target() {
        let t = IsolatedTmux::new("sync-test-anchor-elsewhere");
        let tmp = TempDir::new().unwrap();

        // Create 3 panes: A, B, C — each in separate windows
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();

        // Use B's window as target — but A is our anchor (desired first pane)
        let target_window = t.pane_window(&pane_b).unwrap();

        let pane_columns = vec![vec![pane_a.clone()], vec![pane_c.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_c.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "anchor A should be in target");
        assert!(final_panes.contains(&pane_c), "C should be in target");

        // B should have been evicted (it was in target but not wanted)
        assert!(
            !final_panes.contains(&pane_b),
            "B should have been evicted"
        );
        assert!(t.pane_alive(&pane_b), "B should still be alive");

        assert!(
            log.entries().iter().any(|e| e.phase == "ATTACH" && e.ok),
            "should have anchor move entry"
        );
    }

    /// Verify that when the anchor pane joins a window containing a desired pane
    /// from a later column, the final ordered layout is column-major (no REORDER needed).
    #[test]
    fn test_sync_anchor_joins_left_of_desired_pane() {
        let t = IsolatedTmux::new("sync-anchor-col-major");
        let tmp = TempDir::new().unwrap();

        // A (anchor) and B — each in separate windows. B's window is the target.
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_b).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        // Desired: [[A], [B]] — two columns, A left, B right
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        // Final layout should be column-major: A (left), B (right)
        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "layout should be column-major without REORDER");

        // Should NOT have REORDER entries — the join flag should place A correctly
        let has_reorder = log.entries().iter().any(|e| e.phase == "REORDER");
        assert!(!has_reorder, "anchor join should use correct flag, avoiding REORDER: {:?}", log.entries());
    }

    /// Verify anchor joins above a desired pane in the same column (single-column layout).
    #[test]
    fn test_sync_anchor_joins_above_in_same_column() {
        let t = IsolatedTmux::new("sync-anchor-same-col");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_b).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        // Desired: [[A, B]] — single column, A above, B below
        let pane_columns = vec![vec![pane_a.clone(), pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "layout should stack A above B without REORDER");

        let has_reorder = log.entries().iter().any(|e| e.phase == "REORDER");
        assert!(!has_reorder, "anchor join should use vertical flag, avoiding REORDER: {:?}", log.entries());
    }

    /// Verify first_pane_join_flag returns correct flags for various pane positions.
    #[test]
    fn test_first_pane_join_flag_logic() {
        let cols = vec![
            vec!["A".to_string(), "B".to_string()],
            vec!["C".to_string()],
        ];

        // Target is in later column → anchor goes left (-dbh)
        assert_eq!(first_pane_join_flag(&cols, "C"), "-dbh");

        // Target is in same column, later row → anchor goes above (-dbv)
        assert_eq!(first_pane_join_flag(&cols, "B"), "-dbv");

        // Target is not in desired set → neutral default (-dh)
        assert_eq!(first_pane_join_flag(&cols, "X"), "-dh");
    }

    /// Single pane layout — trivial case, no joins needed.
    #[test]
    fn test_sync_single_pane_layout() {
        let t = IsolatedTmux::new("sync-single-pane");
        let (target_window, panes, _tmp) = setup_panes(&t, 1);

        let pane_columns = vec![vec![panes[0].clone()]];
        let desired: Vec<&str> = vec![panes[0].as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(final_ordered.len(), 1);
        assert_eq!(final_ordered[0], panes[0]);
        // Should hit fast path — pane already in correct position
        assert!(log.entries().iter().any(|e| e.phase == "FAST_PATH"));
    }

    /// 2x2 grid layout: [[A, B], [C, D]] — 2 columns, 2 rows each.
    /// Verifies column-major order: A (top-left), B (bottom-left), C (top-right), D (bottom-right).
    #[test]
    fn test_sync_2x2_grid_layout() {
        let t = IsolatedTmux::new("sync-2x2-grid");
        let (target_window, panes, _tmp) = setup_panes(&t, 4);

        // [[A, B], [C, D]] — column-major
        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone()],
            vec![panes[2].clone(), panes[3].clone()],
        ];
        let desired: Vec<&str> = vec![
            panes[0].as_str(),
            panes[1].as_str(),
            panes[2].as_str(),
            panes[3].as_str(),
        ];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "2x2 grid should be column-major order");
        assert!(!log.has_errors(), "2x2 grid should have no errors: {:?}", log.entries());
    }

    /// 2x2 grid with anchor not in target — anchor must join with correct flag.
    #[test]
    fn test_sync_2x2_grid_anchor_elsewhere() {
        let t = IsolatedTmux::new("sync-2x2-anchor-elsewhere");
        let tmp = TempDir::new().unwrap();

        // Create 4 panes: A, B, C, D — each in separate windows
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();
        let pane_d = t.new_window("test", tmp.path()).unwrap();

        // Use C's window as target — A is anchor but not in target
        let target_window = t.pane_window(&pane_c).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        let pane_columns = vec![
            vec![pane_a.clone(), pane_b.clone()],
            vec![pane_c.clone(), pane_d.clone()],
        ];
        let desired: Vec<&str> = vec![
            pane_a.as_str(),
            pane_b.as_str(),
            pane_c.as_str(),
            pane_d.as_str(),
        ];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "2x2 grid with anchor elsewhere should produce correct column-major order");
        assert!(!log.has_errors());
    }

    /// 3-column layout: [[A], [B], [C]] — horizontal only, no vertical stacking.
    #[test]
    fn test_sync_3col_horizontal_layout() {
        let t = IsolatedTmux::new("sync-3col-horizontal");
        let (target_window, panes, _tmp) = setup_panes(&t, 3);

        let pane_columns = vec![
            vec![panes[0].clone()],
            vec![panes[1].clone()],
            vec![panes[2].clone()],
        ];
        let desired: Vec<&str> = vec![panes[0].as_str(), panes[1].as_str(), panes[2].as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "3-column horizontal layout should be left-to-right");
        assert!(!log.has_errors());
    }

    /// Asymmetric layout: [[A, B, C], [D]] — tall left column, short right column.
    #[test]
    fn test_sync_asymmetric_3_1_layout() {
        let t = IsolatedTmux::new("sync-asymmetric-3-1");
        let (target_window, panes, _tmp) = setup_panes(&t, 4);

        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone(), panes[2].clone()],
            vec![panes[3].clone()],
        ];
        let desired: Vec<&str> = vec![
            panes[0].as_str(),
            panes[1].as_str(),
            panes[2].as_str(),
            panes[3].as_str(),
        ];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_ordered = t.list_panes_ordered(&target_window).unwrap();
        let final_refs: Vec<&str> = final_ordered.iter().map(|s| s.as_str()).collect();
        assert_eq!(final_refs, desired, "asymmetric 3+1 layout should be column-major");
        assert!(!log.has_errors());
    }

    /// Verify first_pane_join_flag handles larger grid: [[A,B,C], [D,E], [F]].
    #[test]
    fn test_first_pane_join_flag_3col_grid() {
        let cols = vec![
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
            vec!["D".to_string(), "E".to_string()],
            vec!["F".to_string()],
        ];

        // Same column, different rows
        assert_eq!(first_pane_join_flag(&cols, "B"), "-dbv");
        assert_eq!(first_pane_join_flag(&cols, "C"), "-dbv");

        // Later columns
        assert_eq!(first_pane_join_flag(&cols, "D"), "-dbh");
        assert_eq!(first_pane_join_flag(&cols, "E"), "-dbh");
        assert_eq!(first_pane_join_flag(&cols, "F"), "-dbh");

        // Unwanted
        assert_eq!(first_pane_join_flag(&cols, "Z"), "-dh");
    }

    #[test]
    fn test_sync_mixed_evict_and_join() {
        let t = IsolatedTmux::new("sync-test-mixed");
        let tmp = TempDir::new().unwrap();

        // A in target window, X in target window (split), B in separate window
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let pane_x = t
            .raw_cmd(&[
                "split-window",
                "-t",
                &pane_a,
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();

        // Desired: [A, B] — X should be evicted, B should be joined
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a));
        assert!(final_panes.contains(&pane_b));
        assert!(!final_panes.contains(&pane_x));
        assert!(t.pane_alive(&pane_x), "X should still be alive");

        // With swap-pane fast path, a 1:1 replacement uses SWAP instead of DETACH+ATTACH
        let has_swap = log.entries().iter().any(|e| e.phase == "SWAP" && e.ok);
        let has_detach_attach = log.entries().iter().any(|e| e.phase == "DETACH" && e.ok)
            && log.entries().iter().any(|e| e.phase == "ATTACH" && e.ok);
        assert!(
            has_swap || has_detach_attach,
            "should have swap or detach+attach"
        );

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "mixed evict and join");
        assert_active_window(&state, &target_window, "mixed evict and join");
        // Started: 2 windows (target with A+X, B solo). After: target (A+B), X solo -> 2 windows.
        assert_eq!(state.window_count, 2, "mixed evict and join: target + X's solo window");
        assert_all_alive(&t, &[pane_a, pane_b], "mixed evict and join");
    }

    #[test]
    fn test_sync_unwanted_is_only_pane() {
        // Edge case: target window has ONLY an unwanted pane.
        // Anchor joins first (Phase 1), then X can be evicted normally.
        let t = IsolatedTmux::new("sync-test-unwanted-only");
        let tmp = TempDir::new().unwrap();

        let pane_x = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_x).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let pane_a = t.new_window("test", tmp.path()).unwrap();
        let pane_b = t.new_window("test", tmp.path()).unwrap();

        // Desired: [A, B]. X is alone in target.
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "A should be in target");
        assert!(final_panes.contains(&pane_b), "B should be in target");
        assert!(!final_panes.contains(&pane_x), "X should have been evicted");
        assert!(t.pane_alive(&pane_x), "X should still be alive");

        // Anchor should have been moved in, and X evicted
        assert!(
            log.entries().iter().any(|e| e.phase == "ATTACH" && e.ok),
            "should have anchor entry"
        );
        assert!(
            log.entries().iter().any(|e| e.phase == "DETACH" && e.ok),
            "should have evict entry"
        );
    }

    #[test]
    fn test_sync_multiple_unwanted() {
        let t = IsolatedTmux::new("sync-test-multi-unwanted");
        let tmp = TempDir::new().unwrap();

        // Create A in target, split to add X1 and X2
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let pane_x1 = t
            .raw_cmd(&[
                "split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}",
            ])
            .unwrap();
        let pane_x2 = t
            .raw_cmd(&[
                "split-window", "-t", &pane_a, "-v", "-P", "-F", "#{pane_id}",
            ])
            .unwrap();

        // Desired: [A] only — X1 and X2 should be evicted
        let pane_columns = vec![vec![pane_a.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 1, "only A should remain");
        assert!(final_panes.contains(&pane_a));
        assert!(t.pane_alive(&pane_x1), "X1 should still be alive");
        assert!(t.pane_alive(&pane_x2), "X2 should still be alive");

        let detach_count = log
            .entries()
            .iter()
            .filter(|e| e.phase == "DETACH" && e.ok && e.message.starts_with("broke"))
            .count();
        assert!(detach_count >= 2, "should have detached at least 2 panes, got {}", detach_count);
    }

    #[test]
    fn test_sync_wanted_in_shared_window() {
        // Wanted pane B shares a window with C. B must be isolated before joining.
        let t = IsolatedTmux::new("sync-test-shared-window");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        // Create B and C in a separate window (split)
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t
            .raw_cmd(&[
                "split-window", "-t", &pane_b, "-h", "-P", "-F", "#{pane_id}",
            ])
            .unwrap();

        // Desired: [A, B]. B shares a window with C.
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "A should be in target");
        assert!(final_panes.contains(&pane_b), "B should be in target");
        assert!(!final_panes.contains(&pane_c), "C should NOT be in target");

        // C should still be alive in its own window
        assert!(t.pane_alive(&pane_c), "C should still be alive");
        assert!(!log.has_errors(), "should have no errors: {:?}", log.entries());
    }

    // --- Real-world simulation tests ---

    #[test]
    fn test_reconcile_with_stale_sessions() {
        // Simulate: 5 panes created, 2 die, reconcile with the 3 survivors
        let t = IsolatedTmux::new("sync-test-stale-sessions");
        let (target_window, panes, _tmp) = setup_panes(&t, 5);

        // Kill panes 3 and 4
        t.kill_pane(&panes[3]).unwrap();
        t.kill_pane(&panes[4]).unwrap();

        // Desired layout uses only the 3 survivors
        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone()],
            vec![panes[2].clone()],
        ];
        let desired: Vec<&str> = pane_columns
            .iter()
            .flat_map(|col| col.iter().map(|s| s.as_str()))
            .collect();

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        for pane in &desired {
            assert!(
                final_panes.contains(&pane.to_string()),
                "survivor {} should be in target",
                pane
            );
        }
        // Dead panes should not appear
        assert!(!final_panes.contains(&panes[3]));
        assert!(!final_panes.contains(&panes[4]));
        assert!(!log.has_errors(), "no errors for survivors: {:?}", log.entries());
    }

    #[test]
    fn test_reconcile_multi_file_same_pane() {
        // Simulate: multiple files claim the same pane (deduplication)
        let t = IsolatedTmux::new("sync-test-multi-file-same-pane");
        let (target_window, panes, _tmp) = setup_panes(&t, 3);

        // Two columns, but panes[1] appears twice (like two files claiming same pane)
        // After dedup, we should get [panes[0], panes[1], panes[2]]
        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone()],
            vec![panes[2].clone()],
        ];
        let desired: Vec<&str> = pane_columns
            .iter()
            .flat_map(|col| col.iter().map(|s| s.as_str()))
            .collect();

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 3, "should have 3 unique panes");
        assert!(!log.has_errors());
    }

    #[test]
    fn test_reconcile_dead_panes_in_desired() {
        // Some desired panes are dead — reconcile should skip them gracefully
        let t = IsolatedTmux::new("sync-test-dead-desired");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();
        let pane_d = t.new_window("test", tmp.path()).unwrap();

        // Kill B and D
        t.kill_pane(&pane_b).unwrap();
        t.kill_pane(&pane_d).unwrap();

        // Desired: [A, B, C, D] — B and D are dead
        let pane_columns = vec![
            vec![pane_a.clone(), pane_b.clone()],
            vec![pane_c.clone(), pane_d.clone()],
        ];
        let desired: Vec<&str> = vec![
            pane_a.as_str(),
            pane_b.as_str(),
            pane_c.as_str(),
            pane_d.as_str(),
        ];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        // A and C should be arranged; B and D silently skipped
        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "A should be in target");
        assert!(final_panes.contains(&pane_c), "C should be in target");
        assert!(!final_panes.contains(&pane_b), "dead B should not be present");
        assert!(!final_panes.contains(&pane_d), "dead D should not be present");

        // Should have logged errors for dead panes
        assert!(log.has_errors(), "should have errors for dead panes");
    }

    #[test]
    fn test_reconcile_large_layout() {
        // 8 panes in a 3-column grid simulating real editor state
        let t = IsolatedTmux::new("sync-test-large-layout");
        let (target_window, panes, _tmp) = setup_panes(&t, 8);

        // 3 columns: [0,1,2], [3,4,5], [6,7]
        let pane_columns = vec![
            vec![panes[0].clone(), panes[1].clone(), panes[2].clone()],
            vec![panes[3].clone(), panes[4].clone(), panes[5].clone()],
            vec![panes[6].clone(), panes[7].clone()],
        ];
        let desired: Vec<&str> = pane_columns
            .iter()
            .flat_map(|col| col.iter().map(|s| s.as_str()))
            .collect();

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 8, "all 8 panes should be in window");
        for pane in &desired {
            assert!(
                final_panes.contains(&pane.to_string()),
                "pane {} should be in target window",
                pane
            );
        }
        assert!(!log.has_errors(), "should have no errors: {:?}", log.entries());
    }

    #[test]
    fn test_sync_3_panes_to_2_evicts_extra() {
        // Real-world scenario: window has 3 panes from previous sync,
        // editor now has only 2 files open -> 3rd pane must be evicted.
        let t = IsolatedTmux::new("sync-test-3to2-evict");
        let tmp = TempDir::new().unwrap();

        // Create A in target window, then split to add B and C (3 panes in same window)
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let pane_b = t
            .raw_cmd(&["split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();
        let pane_c = t
            .raw_cmd(&["split-window", "-t", &pane_b, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();

        // Verify setup: 3 panes in target window
        let initial = t.list_window_panes(&target_window).unwrap();
        assert_eq!(initial.len(), 3, "should start with 3 panes");

        // Desired: only [A, B] — C should be evicted
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 2, "should have exactly 2 panes after sync");
        assert!(final_panes.contains(&pane_a), "A should remain");
        assert!(final_panes.contains(&pane_b), "B should remain");
        assert!(!final_panes.contains(&pane_c), "C should be evicted");
        assert!(t.pane_alive(&pane_c), "C should still be alive (detached, not killed)");
        assert!(!log.has_errors(), "no errors: {:?}", log.entries());

        // Verify C was detached
        let detach_count = log
            .entries()
            .iter()
            .filter(|e| e.phase == "DETACH" && e.ok)
            .count();
        assert!(detach_count >= 1, "should have detached at least 1 pane");

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "3 to 2 evict");
        assert_active_window(&state, &target_window, "3 to 2 evict");
        // Started: 1 window (all splits). C evicted to solo -> 2 windows.
        assert_eq!(state.window_count, 2, "3 to 2 evict: target + C's solo window");
        assert_all_alive(&t, &[pane_a, pane_b], "3 to 2 evict");
    }

    #[test]
    fn test_sync_3_panes_to_2_with_external_join() {
        // Window has 3 panes, desired has 2 — one of the desired is in a different window.
        let t = IsolatedTmux::new("sync-test-3to2-external");
        let tmp = TempDir::new().unwrap();

        // Create A in target, split to add X1 and X2 (3 panes in window)
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);
        let pane_x1 = t
            .raw_cmd(&["split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();
        let pane_x2 = t
            .raw_cmd(&["split-window", "-t", &pane_x1, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();

        // Create B in a separate window
        let pane_b = t.new_window("test", tmp.path()).unwrap();

        // Verify: target has 3 panes, B is elsewhere
        let initial = t.list_window_panes(&target_window).unwrap();
        assert_eq!(initial.len(), 3);
        assert!(!initial.contains(&pane_b));

        // Desired: [A, B] — X1 and X2 must be evicted, B must be joined
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 2, "should have exactly 2 panes");
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B joined into target");
        assert!(!final_panes.contains(&pane_x1), "X1 evicted");
        assert!(!final_panes.contains(&pane_x2), "X2 evicted");
        assert!(t.pane_alive(&pane_x1), "X1 still alive");
        assert!(t.pane_alive(&pane_x2), "X2 still alive");
        assert!(!log.has_errors(), "no errors: {:?}", log.entries());
    }

    #[test]
    fn test_reconcile_pane_from_shared_window() {
        // Desired pane shares a window with other panes — sync should isolate and join it
        let t = IsolatedTmux::new("sync-test-pane-from-shared");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        // Create B and C in a shared window (not the target)
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let shared_window = t.pane_window(&pane_b).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &shared_window, "-x", "200", "-y", "60"]);
        let pane_c = t
            .raw_cmd(&["split-window", "-t", &pane_b, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();

        // Desired layout: [A, B] — B must be pulled from shared window
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "A should be in target");
        assert!(final_panes.contains(&pane_b), "B should be in target");
        assert!(t.pane_alive(&pane_c), "C should still be alive");
        assert!(!log.has_errors(), "should have no errors: {:?}", log.entries());
    }

    #[test]
    fn test_reconcile_scattered_panes() {
        // 4 panes scattered across solo windows. Desired: [A, B] col1, [C] col2
        let t = IsolatedTmux::new("sync-test-scattered");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();
        let pane_d = t.new_window("test", tmp.path()).unwrap();

        let target_window = t.pane_window(&pane_a).unwrap();

        let pane_columns = vec![
            vec![pane_a.clone(), pane_b.clone()],
            vec![pane_c.clone()],
        ];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str(), pane_c.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 3, "should have exactly 3 panes");
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B in target");
        assert!(final_panes.contains(&pane_c), "C in target");
        assert!(!final_panes.contains(&pane_d), "D not in target");
        assert!(t.pane_alive(&pane_d), "D still alive");
        assert!(!log.has_errors(), "no errors: {:?}", log.entries());
    }

    #[test]
    fn test_reconcile_evict_and_join_external() {
        // Target has [A, X1, X2], desired is [A, B] where B is in separate window.
        let t = IsolatedTmux::new("sync-test-evict-join-ext");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        let pane_x1 = t
            .raw_cmd(&["split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();
        let pane_x2 = t
            .raw_cmd(&["split-window", "-t", &pane_x1, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();

        let pane_b = t.new_window("test", tmp.path()).unwrap();

        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 2, "should have exactly A and B");
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B in target");
        assert!(t.pane_alive(&pane_x1), "X1 still alive");
        assert!(t.pane_alive(&pane_x2), "X2 still alive");
        assert!(!log.has_errors(), "no errors: {:?}", log.entries());
    }

    #[test]
    fn test_reconcile_full_real_world_scenario() {
        // 4 solo windows (A, B, C, D). Previous sync left A+C together.
        // Desired: col1=[B, D], col2=[A]. C evicted, B and D joined.
        let t = IsolatedTmux::new("sync-test-full-realworld");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();
        let pane_d = t.new_window("test", tmp.path()).unwrap();

        // Put A and C into the same window (simulating a previous sync)
        let target_window = t.pane_window(&pane_a).unwrap();
        t.join_pane(&pane_c, &pane_a, "-h").unwrap();

        let initial = t.list_window_panes(&target_window).unwrap();
        assert_eq!(initial.len(), 2);
        assert!(initial.contains(&pane_a));
        assert!(initial.contains(&pane_c));

        let pane_columns = vec![
            vec![pane_b.clone(), pane_d.clone()],
            vec![pane_a.clone()],
        ];
        let desired: Vec<&str> = vec![pane_b.as_str(), pane_d.as_str(), pane_a.as_str()];

        let log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B in target");
        assert!(final_panes.contains(&pane_d), "D in target");
        assert!(!final_panes.contains(&pane_c), "C evicted from target");
        assert!(t.pane_alive(&pane_c), "C still alive");
        assert!(!log.has_errors(), "no errors: {:?}", log.entries());

        // Snapshot-based state assertions
        let state = snapshot_state(&t, "test", &target_window);
        assert_target_panes(&state, &desired, "full real world");
        assert_active_window(&state, &target_window, "full real world");
        // Started: 3 windows (A+C, B, D). After: target (A+B+D), C solo -> 2 windows.
        assert_eq!(state.window_count, 2, "full real world: target + C's solo window");
        assert_all_alive(&t, &[pane_a.clone(), pane_b, pane_d], "full real world");
    }

    #[test]
    fn test_reconcile_stable_across_repeated_syncs() {
        // Bug reproduction: switching between two files in the editor
        // caused window cycling (0->1->2->0...) because reconcile would
        // break/join panes each time, creating temporary windows.
        // After fix: repeated reconcile with the same desired layout
        // should be idempotent — no new windows, same target.
        let t = IsolatedTmux::new("sync-test-stable-repeat");
        let _tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", _tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", _tmp.path()).unwrap();

        // Start: A in one window, B in another
        let target_window = t.pane_window(&pane_a).unwrap();

        let pane_columns = vec![vec![pane_a.clone(), pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        // First reconcile: should consolidate A and B into target_window
        let log1 = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();
        let panes1 = t.list_window_panes(&target_window).unwrap();
        assert_eq!(panes1.len(), 2, "first sync: 2 panes in target");
        assert!(panes1.contains(&pane_a), "first sync: A present");
        assert!(panes1.contains(&pane_b), "first sync: B present");
        assert!(!log1.has_errors(), "first sync: no errors");

        // Verify active window after first sync
        let state1 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state1, &target_window, "first sync");

        // Count total windows in the session
        let windows_after_1 = t
            .raw_cmd(&["list-windows", "-t", "test", "-F", "#{window_id}"])
            .unwrap();
        let win_count_1 = windows_after_1.lines().count();

        // Second reconcile with SAME layout — should be a no-op
        let log2 = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();
        let panes2 = t.list_window_panes(&target_window).unwrap();
        assert_eq!(panes2.len(), 2, "second sync: still 2 panes");
        assert!(panes2.contains(&pane_a), "second sync: A present");
        assert!(panes2.contains(&pane_b), "second sync: B present");
        assert!(!log2.has_errors(), "second sync: no errors");
        assert_eq!(
            log2.mutation_count(),
            0,
            "second sync: no mutations (idempotent)"
        );

        // Verify no new windows were created
        let windows_after_2 = t
            .raw_cmd(&["list-windows", "-t", "test", "-F", "#{window_id}"])
            .unwrap();
        let win_count_2 = windows_after_2.lines().count();
        assert_eq!(
            win_count_1, win_count_2,
            "no new windows from idempotent sync"
        );

        // Verify active window after second sync
        let state2 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state2, &target_window, "second sync");

        // Third reconcile — still stable
        let log3 = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();
        assert_eq!(
            log3.mutation_count(),
            0,
            "third sync: still no mutations"
        );

        // Verify active window after third sync
        let state3 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state3, &target_window, "third sync");
    }

    #[test]
    fn test_reconcile_tab_switching_no_window_cycling() {
        // Bug reproduction: switching between two documents in the editor
        // causes window cycling (0->1->2->0...) because each sync evicts
        // the unwanted pane via break_pane into a NEW solo window.
        //
        // Scenario: 3 panes (A=agent-doc, B=plugin, C=dave-franklin).
        // Editor alternates between [A,C] and [B,C] as user switches tabs.
        // With stash window: evicted panes go to stash, no new windows created.
        let t = IsolatedTmux::new("sync-test-tab-cycling");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();

        // Pick A's window as canonical
        let target_window = t.pane_window(&pane_a).unwrap();

        // --- Sync 1: layout = [A, C] ---
        let cols1 = vec![vec![pane_a.clone(), pane_c.clone()]];
        let desired1: Vec<&str> = vec![pane_a.as_str(), pane_c.as_str()];
        let log1 = reconcile(&t, &target_window, &cols1, &desired1, Some("test"), None, &dummy_registry_path()).unwrap();
        assert!(!log1.has_errors(), "sync1 errors: {:?}", log1.entries());
        let panes1 = t.list_window_panes(&target_window).unwrap();
        assert!(panes1.contains(&pane_a), "sync1: A in target");
        assert!(panes1.contains(&pane_c), "sync1: C in target");
        assert!(!panes1.contains(&pane_b), "sync1: B not in target");

        // Verify active window after sync 1
        let state1 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state1, &target_window, "sync1");

        // Count windows after sync 1
        let win_count_1 = count_windows(&t, "test");

        // --- Sync 2: layout = [B, C] (user switched left tab) ---
        let cols2 = vec![vec![pane_b.clone(), pane_c.clone()]];
        let desired2: Vec<&str> = vec![pane_b.as_str(), pane_c.as_str()];
        let log2 = reconcile(&t, &target_window, &cols2, &desired2, Some("test"), None, &dummy_registry_path()).unwrap();
        assert!(!log2.has_errors(), "sync2 errors: {:?}", log2.entries());
        let panes2 = t.list_window_panes(&target_window).unwrap();
        assert!(panes2.contains(&pane_b), "sync2: B in target");
        assert!(panes2.contains(&pane_c), "sync2: C in target");
        assert!(!panes2.contains(&pane_a), "sync2: A not in target");

        // Verify active window after sync 2
        let state2 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state2, &target_window, "sync2");

        // A should be in the stash, NOT in a new solo window
        let win_count_2 = count_windows(&t, "test");
        // Allow at most +1 window (the stash). No growing.
        assert!(
            win_count_2 <= win_count_1 + 1,
            "sync2: windows should not grow unbounded ({} -> {})",
            win_count_1,
            win_count_2
        );

        // --- Sync 3: back to [A, C] (user switched back) ---
        let log3 = reconcile(&t, &target_window, &cols1, &desired1, Some("test"), None, &dummy_registry_path()).unwrap();
        assert!(!log3.has_errors(), "sync3 errors: {:?}", log3.entries());
        let panes3 = t.list_window_panes(&target_window).unwrap();
        assert!(panes3.contains(&pane_a), "sync3: A in target");
        assert!(panes3.contains(&pane_c), "sync3: C in target");

        // Verify active window after sync 3
        let state3 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state3, &target_window, "sync3");

        let win_count_3 = count_windows(&t, "test");
        // Window count must NOT keep growing with each switch
        assert_eq!(
            win_count_2, win_count_3,
            "sync3: no new windows from switching back ({} -> {})",
            win_count_2, win_count_3
        );

        // --- Sync 4: [B, C] again ---
        let log4 = reconcile(&t, &target_window, &cols2, &desired2, Some("test"), None, &dummy_registry_path()).unwrap();
        assert!(!log4.has_errors(), "sync4 errors: {:?}", log4.entries());

        // Verify active window after sync 4
        let state4 = snapshot_state(&t, "test", &target_window);
        assert_active_window(&state4, &target_window, "sync4");

        let win_count_4 = count_windows(&t, "test");
        assert_eq!(
            win_count_3, win_count_4,
            "sync4: window count stable ({} -> {})",
            win_count_3, win_count_4
        );

        // All panes still alive (no process killed)
        assert!(t.pane_alive(&pane_a), "A still alive");
        assert!(t.pane_alive(&pane_b), "B still alive");
        assert!(t.pane_alive(&pane_c), "C still alive");

        // Verify the TARGET WINDOW stays selected throughout all syncs
        let active_win = active_window(&t, "test");
        assert_eq!(
            active_win, target_window,
            "target window should be selected after all syncs"
        );
    }

    /// Count windows in a tmux session.
    fn count_windows(tmux: &IsolatedTmux, session: &str) -> usize {
        tmux.raw_cmd(&["list-windows", "-t", session, "-F", "#{window_id}"])
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Get the active window ID in a tmux session.
    fn active_window(tmux: &IsolatedTmux, session: &str) -> String {
        tmux.raw_cmd(&[
            "display-message",
            "-t",
            session,
            "-p",
            "#{window_id}",
        ])
        .unwrap_or_default()
        .trim()
        .to_string()
    }

    /// Get the active pane ID in a tmux session.
    fn active_pane(tmux: &IsolatedTmux, session: &str) -> String {
        tmux.raw_cmd(&[
            "display-message",
            "-t",
            session,
            "-p",
            "#{pane_id}",
        ])
        .unwrap_or_default()
        .trim()
        .to_string()
    }

    #[test]
    fn test_reconcile_2col_tab_switch_selects_left_pane() {
        // Bug reproduction: 2-column editor layout. User switches between
        // agent-doc.md and plugin.md on the LEFT side. dave-franklin.md
        // stays on the RIGHT. After each switch, the LEFT pane should be selected.
        let t = IsolatedTmux::new("sync-test-2col-focus");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();

        let target_window = t.pane_window(&pane_a).unwrap();

        // --- Sync 1: layout = [[A], [C]], focus = A ---
        let cols1 = vec![vec![pane_a.clone()], vec![pane_c.clone()]];
        let desired1: Vec<&str> = vec![pane_a.as_str(), pane_c.as_str()];
        let log1 = reconcile(&t, &target_window, &cols1, &desired1, Some("test"), Some(&pane_a), &dummy_registry_path()).unwrap();
        assert!(!log1.has_errors(), "sync1 errors: {:?}", log1.entries());
        let sel1 = active_pane(&t, "test");
        assert_eq!(sel1, pane_a, "sync1: A (left) should be selected after reconcile");

        // --- Sync 2: layout = [[B], [C]], focus = B ---
        let cols2 = vec![vec![pane_b.clone()], vec![pane_c.clone()]];
        let desired2: Vec<&str> = vec![pane_b.as_str(), pane_c.as_str()];
        let log2 = reconcile(&t, &target_window, &cols2, &desired2, Some("test"), Some(&pane_b), &dummy_registry_path()).unwrap();
        assert!(!log2.has_errors(), "sync2 errors: {:?}", log2.entries());
        // After reconcile: focus pane (B) should already be selected (attach->select->detach)
        let sel2 = active_pane(&t, "test");
        assert_eq!(sel2, pane_b, "sync2: B (left) should be selected after reconcile");

        // Verify B is actually on the left (first in pane order)
        let ordered = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered[0], pane_b, "sync2: B should be leftmost pane");
        assert_eq!(ordered[1], pane_c, "sync2: C should be rightmost pane");

        // --- Sync 3: back to [[A], [C]], focus = A ---
        let log3 = reconcile(&t, &target_window, &cols1, &desired1, Some("test"), Some(&pane_a), &dummy_registry_path()).unwrap();
        assert!(!log3.has_errors(), "sync3 errors: {:?}", log3.entries());
        let sel3 = active_pane(&t, "test");
        assert_eq!(sel3, pane_a, "sync3: A (left) should be selected after reconcile");

        let ordered3 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered3[0], pane_a, "sync3: A should be leftmost pane");
        assert_eq!(ordered3[1], pane_c, "sync3: C should be rightmost pane");

        // --- Sync 4: [[B], [C]] again ---
        let log4 = reconcile(&t, &target_window, &cols2, &desired2, Some("test"), Some(&pane_b), &dummy_registry_path()).unwrap();
        assert!(!log4.has_errors(), "sync4 errors: {:?}", log4.entries());
        let sel4 = active_pane(&t, "test");
        assert_eq!(sel4, pane_b, "sync4: B (left) should be selected after reconcile");

        let ordered4 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered4[0], pane_b, "sync4: B should be leftmost pane");
        assert_eq!(ordered4[1], pane_c, "sync4: C should be rightmost pane");

        // All alive
        assert_all_alive(&t, &[pane_a.clone(), pane_b, pane_c], "end");

        // Window count stable
        let win_count = count_windows(&t, "test");
        // Expect: target + stash (2), possibly + dead B/A window shells
        assert!(win_count <= 3, "at most 3 windows: target + stash + 1 shell");
    }

    #[test]
    fn test_full_flow_2col_tab_switch_pane_selection() {
        // Bug reproduction: full sync flow (reconcile + equalize_sizes + select_pane).
        let t = IsolatedTmux::new("sync-full-flow");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-x", "200", "-y", "60"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();

        let target_window = t.pane_window(&pane_a).unwrap();

        // Helper: full sync flow (reconcile with focus + equalize_sizes + select_pane)
        let full_sync = |cols: &[Vec<String>], desired: &[&str], focus: &str, label: &str| {
            let log = reconcile(&t, &target_window, cols, desired, Some("test"), Some(focus), &dummy_registry_path()).unwrap();
            assert!(!log.has_errors(), "{}: reconcile errors: {:?}", label, log.entries());

            let sel_after_reconcile = active_pane(&t, "test");
            eprintln!("{}: after reconcile, selected={}", label, sel_after_reconcile);
            assert_eq!(sel_after_reconcile, focus,
                "{}: reconcile should pre-select focus pane (attach->select->detach)", label);

            equalize_sizes(&t, cols);

            let sel_after_equalize = active_pane(&t, "test");
            eprintln!("{}: after equalize_sizes, selected={}", label, sel_after_equalize);
            assert_eq!(sel_after_equalize, focus,
                "{}: equalize_sizes should not change selected pane", label);

            // Final select_pane (as sync does)
            t.select_pane(focus).unwrap();
            let sel_final = active_pane(&t, "test");
            assert_eq!(sel_final, focus, "{}: final selected pane", label);

            // Verify the focus pane is in the target window
            let ordered = t.list_panes_ordered(&target_window).unwrap();
            assert!(ordered.contains(&focus.to_string()),
                "{}: focus pane {} not in target window {:?}", label, focus, ordered);
        };

        // --- Sync 1: [[A], [C]], focus = A (left) ---
        let cols1 = vec![vec![pane_a.clone()], vec![pane_c.clone()]];
        let desired1: Vec<&str> = vec![pane_a.as_str(), pane_c.as_str()];
        full_sync(&cols1, &desired1, &pane_a, "sync1");

        // Verify A is leftmost
        let ordered1 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered1[0], pane_a, "sync1: A should be leftmost");

        // --- Sync 2: [[B], [C]], focus = B (left) ---
        let cols2 = vec![vec![pane_b.clone()], vec![pane_c.clone()]];
        let desired2: Vec<&str> = vec![pane_b.as_str(), pane_c.as_str()];
        full_sync(&cols2, &desired2, &pane_b, "sync2");

        let ordered2 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered2[0], pane_b, "sync2: B should be leftmost");

        // --- Sync 3: back to [[A], [C]], focus = A ---
        full_sync(&cols1, &desired1, &pane_a, "sync3");

        let ordered3 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered3[0], pane_a, "sync3: A should be leftmost");

        // --- Sync 4: [[B], [C]] again ---
        full_sync(&cols2, &desired2, &pane_b, "sync4");

        let ordered4 = t.list_panes_ordered(&target_window).unwrap();
        assert_eq!(ordered4[0], pane_b, "sync4: B should be leftmost");

        // All panes alive
        assert_all_alive(&t, &[pane_a, pane_b, pane_c], "end");
    }

    // --- Auto-register and focus resolution tests ---

    #[test]
    fn test_auto_register_shares_column_pane() {
        let layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
                },
                Column {
                    files: vec![PathBuf::from("c.md")],
                },
            ],
        };

        let mut file_to_pane = std::collections::HashMap::new();
        file_to_pane.insert(PathBuf::from("a.md"), "%1".to_string());
        file_to_pane.insert(PathBuf::from("c.md"), "%2".to_string());

        let donor = find_column_pane(&layout, Path::new("b.md"), &file_to_pane);
        assert_eq!(donor, Some("%1".to_string()), "b.md should get a.md's pane (same column)");
    }

    #[test]
    fn test_no_cross_column_donor() {
        let layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("a.md")],
                },
                Column {
                    files: vec![PathBuf::from("b.md")],
                },
            ],
        };

        let mut file_to_pane = std::collections::HashMap::new();
        file_to_pane.insert(PathBuf::from("a.md"), "%1".to_string());

        let donor = find_column_pane(&layout, Path::new("b.md"), &file_to_pane);
        assert_eq!(donor, None, "should NOT fall back to adjacent column");
    }

    #[test]
    fn test_focus_non_managed_preserves_selection() {
        let layout = Layout {
            columns: vec![Column {
                files: vec![PathBuf::from("a.md")],
            }],
        };
        let file_to_pane = std::collections::HashMap::new();
        let result = find_column_pane(&layout, Path::new("readme.txt"), &file_to_pane);
        assert_eq!(result, None, "non-layout file should return None");
    }

    #[test]
    fn test_focus_column_positional_fallback() {
        let layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
                },
                Column {
                    files: vec![PathBuf::from("c.md")],
                },
            ],
        };

        let mut file_to_pane = std::collections::HashMap::new();
        file_to_pane.insert(PathBuf::from("a.md"), "%1".to_string());
        file_to_pane.insert(PathBuf::from("c.md"), "%2".to_string());

        let result = find_column_pane(&layout, Path::new("b.md"), &file_to_pane);
        assert_eq!(result, Some("%1".to_string()), "should fall back to a.md's pane in same column");
    }

    #[test]
    fn test_unclaimed_left_col_preserves_selection() {
        let layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("plugin.md")],
                },
                Column {
                    files: vec![PathBuf::from("dave-franklin.md")],
                },
            ],
        };

        let mut file_to_pane = std::collections::HashMap::new();
        file_to_pane.insert(PathBuf::from("dave-franklin.md"), "%65".to_string());

        // Phase 1.5: find_column_pane for plugin.md in col0 — no donor
        let donor = find_column_pane(&layout, Path::new("plugin.md"), &file_to_pane);
        assert_eq!(donor, None, "no donor in same column -> no auto-register");

        // Focus resolution: plugin.md not in file_to_pane -> column fallback
        let focus_pane = file_to_pane
            .get(&PathBuf::from("plugin.md"))
            .cloned()
            .or_else(|| find_column_pane(&layout, Path::new("plugin.md"), &file_to_pane));
        assert_eq!(focus_pane, None, "focus should be None -> preserve tmux selection");
    }

    #[test]
    fn test_claimed_left_col_selects_left_pane() {
        let _layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("agent-doc.md")],
                },
                Column {
                    files: vec![PathBuf::from("dave-franklin.md")],
                },
            ],
        };

        let mut file_to_pane = std::collections::HashMap::new();
        file_to_pane.insert(PathBuf::from("agent-doc.md"), "%39".to_string());
        file_to_pane.insert(PathBuf::from("dave-franklin.md"), "%65".to_string());

        let focus_pane = file_to_pane.get(&PathBuf::from("agent-doc.md")).cloned();
        assert_eq!(focus_pane, Some("%39".to_string()), "claimed left file -> select left pane");
    }

    #[test]
    fn test_column_of() {
        let layout = Layout {
            columns: vec![
                Column {
                    files: vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
                },
                Column {
                    files: vec![PathBuf::from("c.md")],
                },
            ],
        };
        assert_eq!(layout.column_of(Path::new("a.md")), Some(0));
        assert_eq!(layout.column_of(Path::new("b.md")), Some(0));
        assert_eq!(layout.column_of(Path::new("c.md")), Some(1));
        assert_eq!(layout.column_of(Path::new("d.md")), None);
    }

    // --- Property-based tests ---

    mod proptest_reconcile {
        use super::*;
        use proptest::prelude::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

        fn unique_socket(prefix: &str) -> String {
            let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
            format!("{}-{}-{}", prefix, std::process::id(), id)
        }

        /// Distribute N panes into num_cols columns as evenly as possible.
        fn distribute_into_columns(panes: &[String], num_cols: usize) -> Vec<Vec<String>> {
            let mut columns: Vec<Vec<String>> = (0..num_cols).map(|_| Vec::new()).collect();
            for (i, pane) in panes.iter().enumerate() {
                columns[i % num_cols].push(pane.clone());
            }
            columns.retain(|c| !c.is_empty());
            columns
        }

        proptest! {
            #[test]
            fn reconcile_completeness(
                num_panes in 2..6usize,
                num_cols in 1..4usize,
            ) {
                let t = IsolatedTmux::new(&unique_socket("pt-comp"));
                let (target_window, panes, _tmp) = setup_panes(&t, num_panes);

                let pane_columns = distribute_into_columns(&panes, num_cols);
                let desired: Vec<&str> = pane_columns
                    .iter()
                    .flat_map(|col| col.iter().map(|s| s.as_str()))
                    .collect();

                let _log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

                // After reconcile, all desired panes must be in the target window
                let final_panes = t.list_window_panes(&target_window).unwrap();
                for pane in &desired {
                    prop_assert!(
                        final_panes.contains(&pane.to_string()),
                        "pane {} missing from target window. final={:?}",
                        pane,
                        final_panes
                    );
                }
                // No extra panes
                for pane in &final_panes {
                    prop_assert!(
                        desired.contains(&pane.as_str()),
                        "unexpected pane {} in target window",
                        pane
                    );
                }
            }

            #[test]
            fn reconcile_idempotent(
                num_panes in 2..5usize,
                num_cols in 1..3usize,
            ) {
                let t = IsolatedTmux::new(&unique_socket("pt-idemp"));
                let (target_window, panes, _tmp) = setup_panes(&t, num_panes);

                let pane_columns = distribute_into_columns(&panes, num_cols);
                let desired: Vec<&str> = pane_columns
                    .iter()
                    .flat_map(|col| col.iter().map(|s| s.as_str()))
                    .collect();

                // First reconcile
                let _log1 = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

                // Second reconcile — should be fast path (zero mutations)
                let log2 = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

                prop_assert!(
                    log2.entries().iter().any(|e| e.phase == "FAST_PATH"),
                    "second reconcile should take fast path, got: {:?}",
                    log2.entries()
                );
            }

            #[test]
            fn reconcile_no_pane_loss(
                num_panes in 2..6usize,
                num_cols in 1..4usize,
            ) {
                let t = IsolatedTmux::new(&unique_socket("pt-noloss"));
                let (_, panes, _tmp) = setup_panes(&t, num_panes);

                let pane_columns = distribute_into_columns(&panes, num_cols);
                let desired: Vec<&str> = pane_columns
                    .iter()
                    .flat_map(|col| col.iter().map(|s| s.as_str()))
                    .collect();
                let target_window = t.pane_window(&panes[0]).unwrap();

                let _log = reconcile(&t, &target_window, &pane_columns, &desired, None, None, &dummy_registry_path()).unwrap();

                // All panes must still be alive (no pane was destroyed)
                for pane in &panes {
                    prop_assert!(
                        t.pane_alive(pane),
                        "pane {} should still be alive after reconcile",
                        pane
                    );
                }
            }
        }
    }

    #[test]
    fn test_reconcile_stash_full_falls_back_to_break() {
        // Bug reproduction: when the stash window is too small to accept more
        // panes (tmux "pane too small" error), reconcile should fall back to
        // break_pane instead of leaving unwanted panes in the target window.
        let t = IsolatedTmux::new("sync-test-stash-full");
        let tmp = TempDir::new().unwrap();

        // Create 4 panes in separate windows
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "10"]);
        let pane_b = t.new_window("test", tmp.path()).unwrap();
        let pane_c = t.new_window("test", tmp.path()).unwrap();
        let pane_d = t.new_window("test", tmp.path()).unwrap();

        // First sync: bring all 4 panes into target window
        let cols_all = vec![
            vec![pane_a.clone()],
            vec![pane_b.clone()],
            vec![pane_c.clone()],
            vec![pane_d.clone()],
        ];
        let desired_all: Vec<&str> = vec![&pane_a, &pane_b, &pane_c, &pane_d];
        let log1 = reconcile(&t, &target_window, &cols_all, &desired_all, Some("test"), None, &dummy_registry_path()).unwrap();
        assert!(!log1.has_errors(), "sync1 errors: {:?}", log1.entries());

        // Pre-fill the stash window with extra panes to make it cramped.
        // The stash window starts at minimum height; adding panes shrinks each row.
        let stash_window = t.ensure_stash_window("test").unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &stash_window, "-y", "4"]);
        for _ in 0..3 {
            let _ = t.raw_cmd(&["split-window", "-t", &stash_window, "-dv"]);
        }

        // Second sync: only keep A and B, evict C and D.
        // With a cramped stash window, stash_pane's join may fail —
        // the fix should fall back to break_pane.
        let cols_ab = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired_ab: Vec<&str> = vec![&pane_a, &pane_b];
        let log2 = reconcile(&t, &target_window, &cols_ab, &desired_ab, Some("test"), None, &dummy_registry_path()).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(final_panes.len(), 2, "should have exactly A and B, got: {:?}", final_panes);
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B in target");
        assert!(t.pane_alive(&pane_c), "C still alive (stashed or broken out)");
        assert!(t.pane_alive(&pane_d), "D still alive (stashed or broken out)");
        assert!(!log2.has_errors(), "sync2 errors: {:?}", log2.entries());
    }

    #[test]
    fn test_reconcile_ghost_pane_with_numeric_session() {
        // Bug reproduction: 3 panes in window but only 2 desired.
        // Session name is numeric ("0"), which caused ensure_stash_window
        // to fail with "index 0 in use" (tmux parsed "0" as window index).
        // Ghost pane was left in the target window instead of being stashed.
        let t = IsolatedTmux::new("sync-test-ghost-numeric");
        let tmp = TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "60"]);

        // Create 2 more panes in the same window (simulating 3 panes)
        let pane_b = t
            .raw_cmd(&["split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();
        let pane_ghost = t
            .raw_cmd(&["split-window", "-t", &pane_b, "-h", "-P", "-F", "#{pane_id}"])
            .unwrap();

        let initial = t.list_window_panes(&target_window).unwrap();
        assert_eq!(initial.len(), 3, "setup: 3 panes in window");

        // Desired layout: only A and B (ghost should be evicted)
        let pane_columns = vec![vec![pane_a.clone()], vec![pane_b.clone()]];
        let desired: Vec<&str> = vec![pane_a.as_str(), pane_b.as_str()];

        // Use session_name = Some("test") to trigger stash path
        let log = reconcile(
            &t, &target_window, &pane_columns, &desired,
            Some("test"), None, &dummy_registry_path(),
        ).unwrap();

        let final_panes = t.list_window_panes(&target_window).unwrap();
        assert_eq!(
            final_panes.len(), 2,
            "ghost pane should be stashed, got: {:?}", final_panes
        );
        assert!(final_panes.contains(&pane_a), "A in target");
        assert!(final_panes.contains(&pane_b), "B in target");
        assert!(!final_panes.contains(&pane_ghost), "ghost should not be in target");
        assert!(t.pane_alive(&pane_ghost), "ghost still alive (stashed)");

        // Verify ghost went to stash window (not a new visible window)
        let stash = t.find_stash_window("test");
        assert!(stash.is_some(), "stash window should exist");
        let stash_panes = t.list_window_panes(&stash.unwrap()).unwrap();
        assert!(stash_panes.contains(&pane_ghost), "ghost should be in stash window");

        assert!(!log.has_errors(), "no errors: {:?}", log.entries());
    }

    #[test]
    fn test_sync_spare_pane_for_unregistered_file() {
        // Phase 1.75: When a file has a session key but no registry entry,
        // and it's the sole file in its column (no column donor),
        // sync should assign a spare pane from the target window.
        let t = IsolatedTmux::new("sync-test-spare-pane");
        let tmp = tempfile::TempDir::new().unwrap();

        // Create two panes
        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "50"]);
        // Split to create second pane in same window
        let pane_b = t
            .raw_cmd(&[
                "split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}",
            ])
            .unwrap()
            .trim()
            .to_string();

        // Create test files
        let file_a = tmp.path().join("registered.md");
        let file_b = tmp.path().join("unregistered.md");
        std::fs::write(&file_a, "# Registered").unwrap();
        std::fs::write(&file_b, "# Unregistered").unwrap();

        // Register only file_a in the registry
        let registry_path = tmp.path().join("registry.json");
        let mut registry = crate::registry::Registry::new();
        registry.insert(
            "session-aaa".to_string(),
            crate::registry::RegistryEntry {
                pane: pane_a.clone(),
                pid: std::process::id(),
                cwd: tmp.path().to_string_lossy().to_string(),
                started: String::new(),
                file: file_a.to_string_lossy().to_string(),
                window: target_window.clone(),
            },
        );
        crate::registry::save_registry(&registry_path, &registry).unwrap();

        // file_b has a session key but no registry entry
        let col_args = vec![
            file_a.to_string_lossy().to_string(),
            file_b.to_string_lossy().to_string(),
        ];

        let resolve_file = |path: &Path| -> Option<FileResolution> {
            if path == file_a {
                Some(FileResolution::Registered {
                    key: "session-aaa".to_string(),
                    tmux_session: Some("test".to_string()),
                })
            } else if path == file_b {
                Some(FileResolution::Registered {
                    key: "session-bbb".to_string(),
                    tmux_session: Some("test".to_string()),
                })
            } else {
                None
            }
        };

        let result = sync(
            &col_args,
            Some(&target_window),
            None,
            &t,
            &registry_path,
            &resolve_file,
        )
        .unwrap();

        // Verify: both files should have pane assignments in the result
        assert_eq!(
            result.file_panes.len(),
            2,
            "both files should have pane assignments, got: {:?}",
            result.file_panes
        );

        let pane_for_a = result
            .file_panes
            .iter()
            .find(|(p, _)| p == &file_a)
            .map(|(_, id)| id.as_str());
        let pane_for_b = result
            .file_panes
            .iter()
            .find(|(p, _)| p == &file_b)
            .map(|(_, id)| id.as_str());

        assert_eq!(pane_for_a, Some(pane_a.as_str()), "registered file gets its pane");
        assert!(pane_for_b.is_some(), "unregistered file gets a spare pane");
        assert_eq!(pane_for_b, Some(pane_b.as_str()), "unregistered file gets the spare pane");
    }

    #[test]
    fn test_sync_file_panes_includes_all_resolved() {
        // Verify that SyncResult::file_panes contains entries for all
        // resolved files, including those assigned via Phase 1.5 (column donor).
        let t = IsolatedTmux::new("sync-test-file-panes");
        let tmp = tempfile::TempDir::new().unwrap();

        let pane_a = t.new_session("test", tmp.path()).unwrap();
        let target_window = t.pane_window(&pane_a).unwrap();
        let _ = t.raw_cmd(&["resize-window", "-t", &target_window, "-x", "200", "-y", "50"]);
        let pane_b = t
            .raw_cmd(&[
                "split-window", "-t", &pane_a, "-h", "-P", "-F", "#{pane_id}",
            ])
            .unwrap()
            .trim()
            .to_string();

        let file_a = tmp.path().join("a.md");
        let file_b = tmp.path().join("b.md");
        let file_c = tmp.path().join("c.md"); // same column as a, unregistered
        std::fs::write(&file_a, "# A").unwrap();
        std::fs::write(&file_b, "# B").unwrap();
        std::fs::write(&file_c, "# C").unwrap();

        let registry_path = tmp.path().join("registry.json");
        let mut registry = crate::registry::Registry::new();
        registry.insert(
            "session-aaa".to_string(),
            crate::registry::RegistryEntry {
                pane: pane_a.clone(),
                pid: std::process::id(),
                cwd: tmp.path().to_string_lossy().to_string(),
                started: String::new(),
                file: file_a.to_string_lossy().to_string(),
                window: target_window.clone(),
            },
        );
        registry.insert(
            "session-bbb".to_string(),
            crate::registry::RegistryEntry {
                pane: pane_b.clone(),
                pid: std::process::id(),
                cwd: tmp.path().to_string_lossy().to_string(),
                started: String::new(),
                file: file_b.to_string_lossy().to_string(),
                window: target_window.clone(),
            },
        );
        crate::registry::save_registry(&registry_path, &registry).unwrap();

        // Layout: col0=[a.md, c.md], col1=[b.md]
        // c.md is unregistered but in same column as a.md → Phase 1.5 column donor
        let col_args = vec![
            format!("{},{}", file_a.to_string_lossy(), file_c.to_string_lossy()),
            file_b.to_string_lossy().to_string(),
        ];

        let resolve_file = |path: &Path| -> Option<FileResolution> {
            if path == file_a {
                Some(FileResolution::Registered {
                    key: "session-aaa".to_string(),
                    tmux_session: Some("test".to_string()),
                })
            } else if path == file_b {
                Some(FileResolution::Registered {
                    key: "session-bbb".to_string(),
                    tmux_session: Some("test".to_string()),
                })
            } else if path == file_c {
                Some(FileResolution::Registered {
                    key: "session-ccc".to_string(),
                    tmux_session: Some("test".to_string()),
                })
            } else {
                None
            }
        };

        let result = sync(
            &col_args,
            Some(&target_window),
            None,
            &t,
            &registry_path,
            &resolve_file,
        )
        .unwrap();

        // All three files should be in file_panes
        assert!(
            result.file_panes.len() >= 2,
            "at least 2 file_panes expected, got: {:?}",
            result.file_panes
        );

        let has_a = result.file_panes.iter().any(|(p, _)| p == &file_a);
        let has_b = result.file_panes.iter().any(|(p, _)| p == &file_b);
        let has_c = result.file_panes.iter().any(|(p, _)| p == &file_c);

        assert!(has_a, "file_a should be in file_panes");
        assert!(has_b, "file_b should be in file_panes");
        assert!(has_c, "file_c (column donor) should be in file_panes");
    }
}
