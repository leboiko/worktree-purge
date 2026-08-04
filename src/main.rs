//! CLI entry point and the terminal event loop.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};

use worktree_purge::app::events::{Action, handle_key};
use worktree_purge::app::purge;
use worktree_purge::app::state::{AppState, Mode};
use worktree_purge::model::{EnrichMsg, PurgeOutcome};
use worktree_purge::scan::enrich;
use worktree_purge::{format, git, scan, ui};

/// How long to wait for a key before redrawing. Also paces the spinner.
const TICK: Duration = Duration::from_millis(100);

#[derive(Parser, Debug)]
#[command(
    name = "worktree-purge",
    version,
    about = "Find and reclaim disk space from stale git worktrees",
    long_about = None
)]
struct Cli {
    /// Folder to scan for git worktrees.
    #[arg(default_value = ".")]
    folder: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !git::available() {
        bail!("the `git` command is required but could not be run");
    }

    let root = cli
        .folder
        .canonicalize()
        .with_context(|| format!("cannot read folder {}", cli.folder.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }

    let scan_result = scan::scan(&root);
    let mut state = AppState::from_scan(root, scan_result);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut state);
    ratatui::restore();
    result
}

/// Background work in flight, so the UI thread never blocks on it.
struct Jobs {
    enrich_rx: Receiver<EnrichMsg>,
    enrich_handle: enrich::EnrichHandle,
    purge_rx: Option<Receiver<PurgeOutcome>>,
    purge_results: Vec<PurgeOutcome>,
}

fn start_enrichment(state: &AppState) -> (Receiver<EnrichMsg>, enrich::EnrichHandle) {
    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&state.groups, tx);
    (rx, handle)
}

fn run(terminal: &mut ratatui::DefaultTerminal, state: &mut AppState) -> Result<()> {
    let (enrich_rx, enrich_handle) = start_enrichment(state);
    let mut jobs = Jobs {
        enrich_rx,
        enrich_handle,
        purge_rx: None,
        purge_results: Vec::new(),
    };
    state.enriching = true;

    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|frame| ui::draw(frame, state))?;

        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match handle_key(state, key) {
                Action::None => {}
                Action::Quit => {
                    jobs.enrich_handle.cancel();
                    return Ok(());
                }
                Action::Rescan => {
                    jobs.enrich_handle.cancel();
                    rescan(state, &mut jobs);
                }
                Action::Purge => start_purge(state, &mut jobs),
            }
        }

        drain_enrichment(state, &mut jobs);
        drain_purge(state, &mut jobs);

        if last_tick.elapsed() >= TICK {
            state.spinner_tick = state.spinner_tick.wrapping_add(1);
            last_tick = Instant::now();
        }

        if state.should_quit {
            jobs.enrich_handle.cancel();
            return Ok(());
        }
    }
}

fn rescan(state: &mut AppState, jobs: &mut Jobs) {
    let focused = state.focused_path();
    let root = state.root.clone();
    let sort = state.sort;

    let scanned = scan::scan(&root);
    let mut next = AppState::from_scan(root, scanned);
    next.sort = sort;
    if let Some(path) = focused.as_deref() {
        next.focus_path(path);
    }
    *state = next;

    let (rx, handle) = start_enrichment(state);
    jobs.enrich_rx = rx;
    jobs.enrich_handle = handle;
    state.enriching = true;
}

fn drain_enrichment(state: &mut AppState, jobs: &mut Jobs) {
    loop {
        match jobs.enrich_rx.try_recv() {
            Ok(EnrichMsg::Done) => state.enriching = false,
            Ok(msg) => state.apply_enrich(msg),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                state.enriching = false;
                break;
            }
        }
    }
}

/// Kick off the confirmed plan on a worker thread.
fn start_purge(state: &mut AppState, jobs: &mut Jobs) {
    let plan = match &state.mode {
        Mode::Confirm(plan) | Mode::ForceConfirm(plan) => plan.clone(),
        _ => return,
    };
    if plan.is_empty() {
        state.mode = Mode::Browse;
        return;
    }

    let (tx, rx) = mpsc::channel();
    let items = plan.items.clone();
    std::thread::spawn(move || {
        let mut report = |outcome: &PurgeOutcome| {
            let _ = tx.send(outcome.clone());
        };
        // Items needing force reached this point only via the second gate.
        purge::purge_all(&items, true, &mut report);
    });

    jobs.purge_rx = Some(rx);
    jobs.purge_results.clear();
    state.mode = Mode::Purging;
}

fn drain_purge(state: &mut AppState, jobs: &mut Jobs) {
    let Some(rx) = jobs.purge_rx.as_ref() else {
        return;
    };
    loop {
        match rx.try_recv() {
            Ok(outcome) => jobs.purge_results.push(outcome),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    // Channel closed: the batch is done.
    jobs.purge_rx = None;
    let outcomes = std::mem::take(&mut jobs.purge_results);
    let freed: u64 = outcomes.iter().filter_map(|o| o.result.as_ref().ok()).sum();
    let failed = outcomes.iter().filter(|o| !o.succeeded()).count();

    state.apply_purge_results(&outcomes);
    state.toast = Some(if failed == 0 {
        format!("freed {}", format::bytes(freed))
    } else {
        format!(
            "freed {} · {} failed",
            format::bytes(freed),
            format::count_noun(failed, "removal")
        )
    });
    state.mode = Mode::Results(outcomes);
}
