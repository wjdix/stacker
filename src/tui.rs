//! Interactive TUI shown when `stacker` is run with no subcommand.
//!
//! Left pane: the stack (head on top). Right pane: detail for the selected
//! change's PR — whether it's ready to merge and which checks are failing.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

/// How often the TUI re-checks PR status in the background.
const AUTO_REFRESH: Duration = Duration::from_secs(30);

use ansi_to_tui::IntoText;
use anyhow::{Result, bail};
use jj_lib::object_id::ObjectId;
use octocrab::Octocrab;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use tokio::sync::mpsc;

use crate::commands::{add, land, submit, sync};
use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, CheckOutcome, PrDetail, PrState};
use crate::github::remote::{self, GitHubRepo};
use crate::jj::{repo, trunk};
use crate::paths;
use crate::stack;
use crate::store::Store;

/// PR data state for one row.
enum Detail {
    NoPr,
    Loading,
    Unavailable(String),
    Loaded(Box<PrDetail>),
}

/// A fetched PR result, sent from a background task to the event loop.
struct FetchResult {
    index: usize,
    detail: Detail,
}

struct Row {
    short: String,
    /// Current commit-id (hex) — the revset for `jj diff`.
    commit: String,
    summary: String,
    pr_number: Option<u64>,
    has_conflict: bool,
    detail: Detail,
    /// Lazily-loaded, styled diff lines for this change (the right pane).
    diff: Option<Vec<Line<'static>>>,
}

struct App {
    /// All tracked stack names (sorted); `stack_idx` indexes into this.
    stacks: Vec<String>,
    stack_idx: usize,
    name: String,
    rows: Vec<Row>,
    selected: usize,
    footer: String,
    /// Vertical scroll offset of the right-pane diff for the selected change.
    diff_scroll: u16,
    /// Whether the per-check list is expanded (default: minimized to a summary).
    show_checks: bool,
    gh: Option<(Octocrab, GitHubRepo)>,
    /// Number of PR fetches still in flight in the current batch.
    outstanding: usize,
    /// True while a (manual or background) status check is in progress.
    refreshing: bool,
}

impl App {
    fn next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
        }
        self.diff_scroll = 0;
    }
    fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.diff_scroll = 0;
    }

    /// Load the selected change's diff (via difftastic) if not already cached.
    /// `width` is the right pane's width, used for difft's inline formatting.
    fn ensure_selected_diff(&mut self, width: usize) {
        if let Some(row) = self.rows.get_mut(self.selected)
            && row.diff.is_none()
        {
            row.diff = Some(load_diff(&row.commit, width));
        }
    }

    /// Number of diff lines for the selected change (for scroll clamping).
    fn selected_diff_len(&self) -> u16 {
        self.rows
            .get(self.selected)
            .and_then(|r| r.diff.as_ref())
            .map(|d| d.len() as u16)
            .unwrap_or(0)
    }

    /// Open the selected change's PR in the default browser.
    fn open_selected_pr(&mut self) {
        let target = match self.rows.get(self.selected).map(|r| &r.detail) {
            Some(Detail::Loaded(d)) if !d.url.is_empty() => Some((d.number, d.url.clone())),
            _ => None,
        };
        self.footer = match target {
            Some((number, url)) => match open::that_detached(&url) {
                Ok(()) => format!("opened PR #{number} in browser"),
                Err(e) => format!("couldn't open browser: {e}"),
            },
            None => "no PR URL for the selected change".into(),
        };
    }

    /// Mark all PR rows as loading (for the initial draw and on refresh).
    fn mark_loading(&mut self) {
        let have_gh = self.gh.is_some();
        for row in &mut self.rows {
            row.detail = match row.pr_number {
                None => Detail::NoPr,
                Some(_) if have_gh => Detail::Loading,
                Some(_) => Detail::Unavailable("no GitHub auth/remote".into()),
            };
        }
    }

    /// Begin a fetch batch: spawn the background tasks and arm the in-flight
    /// counter so the UI can show progress and the loop can pace auto-refresh.
    fn begin_fetch(&mut self) -> mpsc::UnboundedReceiver<FetchResult> {
        let rx = self.spawn_fetches();
        self.outstanding = if self.gh.is_some() {
            self.rows.iter().filter(|r| r.pr_number.is_some()).count()
        } else {
            0
        };
        self.refreshing = self.outstanding > 0;
        rx
    }

    /// Record one fetch result arriving; clears `refreshing` when the batch ends.
    fn note_result(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
        if self.outstanding == 0 {
            self.refreshing = false;
        }
    }

    /// Spawn one background task per PR; results stream back over the channel.
    /// The returned receiver closes once every task has finished.
    fn spawn_fetches(&self) -> mpsc::UnboundedReceiver<FetchResult> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some((client, repo)) = &self.gh {
            for (index, row) in self.rows.iter().enumerate() {
                let Some(number) = row.pr_number else { continue };
                let tx = tx.clone();
                let client = client.clone();
                let repo = repo.clone();
                tokio::spawn(async move {
                    let detail = match pr::fetch_detail(&client, &repo, number).await {
                        Ok(d) => Detail::Loaded(Box::new(d)),
                        Err(e) => Detail::Unavailable(format!("{e:#}")),
                    };
                    let _ = tx.send(FetchResult { index, detail });
                });
            }
        }
        rx
    }
}

pub async fn run() -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!(
            "`stacker` with no arguments opens an interactive TUI, which needs a terminal.\n\
             For non-interactive use, run a subcommand (e.g. `stacker status`)."
        );
    }

    let cwd = std::env::current_dir()?;
    let Some(mut app) = gather(&cwd, None).await? else {
        println!("No stacks tracked yet. Use `stacker create <name>` or `stacker import <name>`.");
        return Ok(());
    };

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app).await;
    ratatui::restore();
    result
}

/// Load the repo and build an [`App`] for the chosen stack, or `None` if no
/// stacks are tracked. `preferred` keeps the user on the same stack across a
/// reload; if it no longer exists we fall back to the @-stack, then the first.
async fn gather(cwd: &std::path::Path, preferred: Option<&str>) -> Result<Option<App>> {
    let loaded = repo::load(cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;

    if store.stacks.is_empty() {
        return Ok(None);
    }

    let name = preferred
        .filter(|n| store.stacks.contains_key(*n))
        .map(str::to_string)
        .or_else(|| stack::infer_stack_name(&loaded, &store).ok())
        .unwrap_or_else(|| store.stacks.keys().next().cloned().unwrap());
    let stacks: Vec<String> = store.stacks.keys().cloned().collect();
    let stack_idx = stacks.iter().position(|n| n == &name).unwrap_or(0);
    let trunk_id = trunk::resolve(&loaded.repo, &config.trunk, &config.remote).ok();
    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk, trunk_id.as_ref())?;

    let gh = match (
        remote::resolve(&loaded.repo, &config.remote).ok(),
        client::build().ok(),
    ) {
        (Some(r), Some(c)) => Some((c, r)),
        _ => None,
    };

    // Head on top, like `stacker status`.
    let rows: Vec<Row> = resolved
        .entries
        .iter()
        .rev()
        .map(|e| Row {
            short: e.short_change_id.clone(),
            commit: e.commit_id.hex(),
            summary: e.summary.clone(),
            pr_number: e.pr_number,
            has_conflict: e.has_conflict,
            detail: Detail::NoPr,
            diff: None,
        })
        .collect();

    let mut app = App {
        stacks,
        stack_idx,
        name,
        rows,
        selected: 0,
        footer: String::new(),
        diff_scroll: 0,
        show_checks: false,
        gh,
        outstanding: 0,
        refreshing: false,
    };
    app.mark_loading();
    if app.gh.is_none() {
        app.footer = "no GitHub auth/remote — PR data unavailable".into();
    }
    Ok(Some(app))
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> Result<()> {
    let mut rx = app.begin_fetch();
    let mut last_refresh = Instant::now();
    // `None` = no baseline yet (just (re)built a stack); avoids ringing the bell
    // merely from loading or switching to an already-ready stack.
    let mut prev_bottom: Option<BottomState> = None;
    app.ensure_selected_diff(right_pane_width(terminal));
    terminal.draw(|f| ui(f, app))?;

    loop {
        let mut dirty = false;

        // Drain any PR results that arrived since the last tick.
        while let Ok(res) = rx.try_recv() {
            if let Some(row) = app.rows.get_mut(res.index) {
                row.detail = res.detail;
            }
            app.note_result();
            dirty = true;
        }

        // Background status check: silently re-fetch on an interval so the TUI
        // reflects PR/check changes without the user pressing `r`.
        if app.gh.is_some() && !app.refreshing && last_refresh.elapsed() >= AUTO_REFRESH {
            rx = app.begin_fetch();
            last_refresh = Instant::now();
            dirty = true;
        }

        // Poll for input without blocking the result draining above.
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down | KeyCode::Char('j') => {
                    app.next();
                    dirty = true;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.prev();
                    dirty = true;
                }
                KeyCode::Char('o') => {
                    app.open_selected_pr();
                    dirty = true;
                }
                KeyCode::Char('J') => {
                    let max = app.selected_diff_len().saturating_sub(1);
                    app.diff_scroll = (app.diff_scroll + 1).min(max);
                    dirty = true;
                }
                KeyCode::Char('K') => {
                    app.diff_scroll = app.diff_scroll.saturating_sub(1);
                    dirty = true;
                }
                KeyCode::Char('c') => {
                    app.show_checks = !app.show_checks;
                    dirty = true;
                }
                KeyCode::Tab | KeyCode::BackTab if app.stacks.len() > 1 => {
                    let n = app.stacks.len();
                    let next = if key.code == KeyCode::Tab {
                        (app.stack_idx + 1) % n
                    } else {
                        (app.stack_idx + n - 1) % n
                    };
                    let target = app.stacks[next].clone();
                    if reload(app, &target).await? {
                        rx = app.begin_fetch();
                        last_refresh = Instant::now();
                        prev_bottom = None; // new stack → re-establish baseline
                        app.selected = 0;
                        dirty = true;
                    }
                }
                KeyCode::Char('r') if app.gh.is_some() => {
                    app.mark_loading();
                    rx = app.begin_fetch();
                    last_refresh = Instant::now();
                    prev_bottom = None;
                    dirty = true;
                }
                KeyCode::Char('p') => {
                    let name = app.name.clone();
                    let title = format!("submit {name}");
                    with_suspended(terminal, &title, submit::run(Some(name.clone()), false)).await;
                    if !reload(app, &name).await? {
                        break;
                    }
                    rx = app.begin_fetch();
                    last_refresh = Instant::now();
                    prev_bottom = None;
                    dirty = true;
                }
                KeyCode::Char('s') => {
                    let name = app.name.clone();
                    let title = format!("sync {name}");
                    with_suspended(terminal, &title, sync::run(Some(name.clone()), false, false))
                        .await;
                    if !reload(app, &name).await? {
                        break;
                    }
                    rx = app.begin_fetch();
                    last_refresh = Instant::now();
                    prev_bottom = None;
                    dirty = true;
                }
                KeyCode::Char('l') => {
                    let name = app.name.clone();
                    let title = format!("land {name}");
                    with_suspended(terminal, &title, land::run(Some(name.clone()), false)).await;
                    if !reload(app, &name).await? {
                        break;
                    }
                    rx = app.begin_fetch();
                    last_refresh = Instant::now();
                    prev_bottom = None;
                    dirty = true;
                }
                KeyCode::Char('a') => {
                    let name = app.name.clone();
                    run_add(terminal, &name).await;
                    if !reload(app, &name).await? {
                        break;
                    }
                    rx = app.begin_fetch();
                    last_refresh = Instant::now();
                    prev_bottom = None;
                    dirty = true;
                }
                _ => {}
            }
        }

        // Alert when the bottom of the stack *becomes* ready to merge (or merged)
        // while we're watching it. `prev_bottom == None` means we're still
        // establishing the baseline for a freshly (re)built stack, so we record
        // the first definitive state without ringing.
        if let Some(now_bottom) = definitive_bottom(&app.rows) {
            if let Some(prev) = prev_bottom
                && now_bottom != prev
                && matches!(now_bottom, BottomState::Ready | BottomState::Merged)
            {
                ring_bell();
                app.footer = match now_bottom {
                    BottomState::Merged => "🔔 bottom PR merged".into(),
                    _ => "🔔 bottom PR is ready to merge".into(),
                };
                dirty = true;
            }
            prev_bottom = Some(now_bottom);
        }

        if dirty {
            app.ensure_selected_diff(right_pane_width(terminal));
            terminal.draw(|f| ui(f, app))?;
        }
    }
    Ok(())
}

/// Right-pane width (≈58% of the terminal, minus borders), for difft formatting.
fn right_pane_width(terminal: &ratatui::DefaultTerminal) -> usize {
    let w = terminal.size().map(|s| s.width).unwrap_or(120);
    ((w as usize) * 58 / 100).saturating_sub(4).max(40)
}

/// Leave the TUI, run a command (so its plan/confirmation/output appear in the
/// normal terminal), wait for acknowledgement, then re-enter the TUI.
async fn with_suspended<Fut>(terminal: &mut ratatui::DefaultTerminal, title: &str, fut: Fut)
where
    Fut: std::future::Future<Output = Result<()>>,
{
    ratatui::restore();
    println!("\n── stacker {title} ──\n");
    if let Err(e) = fut.await {
        eprintln!("\nerror: {e:#}");
    }
    prompt_enter();
    *terminal = ratatui::init();
}

/// `stacker add`, with an interactive description prompt (read in normal mode).
async fn run_add(terminal: &mut ratatui::DefaultTerminal, name: &str) {
    use std::io::Write;
    ratatui::restore();
    println!("\n── stacker add {name} ──\n");
    print!("Description for the new change (optional): ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let message = match input.trim() {
        "" => None,
        s => Some(s.to_string()),
    };
    if let Err(e) = add::run(Some(name.to_string()), message).await {
        eprintln!("\nerror: {e:#}");
    }
    prompt_enter();
    *terminal = ratatui::init();
}

fn prompt_enter() {
    println!("\nPress Enter to return to stacker…");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

/// Rebuild the app for the same stack after a mutating command. Returns false if
/// no tracked stacks remain (e.g. the stack was fully landed).
async fn reload(app: &mut App, preferred: &str) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    match gather(&cwd, Some(preferred)).await? {
        Some(new_app) => {
            *app = new_app;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn ui(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(f.area());
    let cols = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(rows[0]);

    // A PR is mergeable *now* only if every PR below it in the stack is merged;
    // higher PRs may be "clean" against their base but must wait their turn.
    let eligible = compute_eligibility(&app.rows);

    // Left: the stack.
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .zip(eligible.iter())
        .map(|(row, &e)| stack_item(row, e))
        .collect();
    let title = if app.stacks.len() > 1 {
        format!(" Stack: {} [{}/{}] ", app.name, app.stack_idx + 1, app.stacks.len())
    } else {
        format!(" Stack: {} ", app.name)
    };
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    f.render_stateful_widget(list, cols[0], &mut list_state);

    // Right: compact PR status header on top, the change's diff (scrollable) below.
    let sel_eligible = eligible.get(app.selected).copied().unwrap_or(false);
    let header = app
        .rows
        .get(app.selected)
        .map(|r| status_header(r, sel_eligible, app.show_checks))
        .unwrap_or_default();
    let pr_title = app
        .rows
        .get(app.selected)
        .and_then(|r| r.pr_number)
        .map(|n| format!(" PR #{n} "))
        .unwrap_or_else(|| " Pull request ".into());

    // Header takes its content height (+borders), capped so the diff keeps room.
    let max_header = cols[1].height.saturating_sub(4).max(3);
    let header_h = ((header.len() as u16) + 2).min(max_header);
    let right =
        Layout::vertical([Constraint::Length(header_h), Constraint::Min(0)]).split(cols[1]);

    f.render_widget(
        Paragraph::new(header)
            .block(Block::bordered().title(pr_title))
            .wrap(Wrap { trim: false }),
        right[0],
    );

    let diff_lines = app
        .rows
        .get(app.selected)
        .and_then(|r| r.diff.clone())
        .unwrap_or_else(|| vec![Line::from("loading diff…").style(Style::new().fg(Color::DarkGray))]);
    f.render_widget(
        Paragraph::new(diff_lines)
            .block(Block::bordered().title(" Diff (difftastic) "))
            .scroll((app.diff_scroll, 0)),
        right[1],
    );

    // Footer.
    let help = if app.stacks.len() > 1 {
        "↑/↓ move   ⇥ stack   J/K scroll diff   c checks   o open   p submit   s sync   l land   a add   r refresh   q quit"
    } else {
        "↑/↓ move   J/K scroll diff   c checks   o open   p submit   s sync   l land   a add   r refresh   q quit"
    };
    let status = if app.refreshing {
        "⟳ checking…"
    } else {
        ""
    };
    let note = [status, app.footer.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("   ");
    let footer = if note.is_empty() {
        help.to_string()
    } else {
        format!("{note}   |   {help}")
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::new().fg(Color::DarkGray)),
        rows[1],
    );
}

/// Notable state of the bottom-of-stack PR, for the terminal-bell alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomState {
    Other,
    Ready,
    Merged,
}

/// Classify the bottom of the stack (trunk-nearest = last row). The bottom is
/// always eligible to merge (nothing below it), so a ready verdict means ready
/// to merge now.
fn bottom_state(rows: &[Row]) -> BottomState {
    match rows.last().map(|r| &r.detail) {
        Some(Detail::Loaded(d)) => match d.state {
            PrState::Merged => BottomState::Merged,
            _ if d.verdict.ready => BottomState::Ready,
            _ => BottomState::Other,
        },
        _ => BottomState::Other,
    }
}

/// Bottom-of-stack state, or `None` while the bottom's PR is still loading
/// (so a pending fetch isn't mistaken for "not ready").
fn definitive_bottom(rows: &[Row]) -> Option<BottomState> {
    match rows.last().map(|r| &r.detail) {
        Some(Detail::Loading) => None,
        _ => Some(bottom_state(rows)),
    }
}

/// Load a change's diff for the right pane. Prefers **difftastic** (`jj diff
/// --tool difft`, inline + forced color, parsed from ANSI), and falls back to a
/// self-colorized `jj diff --git` if difft isn't available.
fn load_diff(commit: &str, width: usize) -> Vec<Line<'static>> {
    let dft = std::process::Command::new("jj")
        .args(["--ignore-working-copy", "diff", "--tool", "difft", "-r", commit])
        .env("DFT_COLOR", "always")
        .env("DFT_DISPLAY", "inline")
        .env("DFT_WIDTH", width.max(40).to_string())
        .output();

    if let Ok(out) = &dft
        && out.status.success()
    {
        if out.stdout.is_empty() {
            return vec![Line::from("(no changes)").style(Style::new().fg(Color::DarkGray))];
        }
        if let Ok(text) = out.stdout.into_text() {
            return text.lines;
        }
    }

    // Fallback: plain unified diff, colorized ourselves.
    let git = std::process::Command::new("jj")
        .args(["--ignore-working-copy", "diff", "--git", "-r", commit])
        .output();
    match git {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            if text.trim().is_empty() {
                return vec![Line::from("(no changes)").style(Style::new().fg(Color::DarkGray))];
            }
            text.lines().map(style_diff_line).collect()
        }
        Ok(o) => vec![error_line(&String::from_utf8_lossy(&o.stderr))],
        Err(e) => vec![error_line(&format!("couldn't run jj diff: {e}"))],
    }
}

fn error_line(msg: &str) -> Line<'static> {
    Line::from(format!("diff unavailable: {}", msg.trim())).style(Style::new().fg(Color::Red))
}

/// Colorize one line of a `--git` unified diff (fallback path).
fn style_diff_line(l: &str) -> Line<'static> {
    let style = if l.starts_with("diff --git")
        || l.starts_with("index ")
        || l.starts_with("+++")
        || l.starts_with("---")
    {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if l.starts_with("@@") {
        Style::new().fg(Color::Magenta)
    } else if l.starts_with('+') {
        Style::new().fg(Color::Green)
    } else if l.starts_with('-') {
        Style::new().fg(Color::Red)
    } else {
        Style::new()
    };
    Line::from(Span::styled(l.to_string(), style))
}

/// Ring the terminal bell.
fn ring_bell() {
    use std::io::Write;
    print!("\x07");
    let _ = std::io::stdout().flush();
}

/// Whether each row is mergeable *now*: true iff every PR below it (closer to
/// trunk) is already merged. Rows are head-first, so "below" means higher index.
fn compute_eligibility(rows: &[Row]) -> Vec<bool> {
    let mut eligible = vec![false; rows.len()];
    let mut all_below_merged = true;
    for i in (0..rows.len()).rev() {
        eligible[i] = all_below_merged;
        let merged = matches!(&rows[i].detail, Detail::Loaded(d) if d.state == PrState::Merged);
        all_below_merged = all_below_merged && merged;
    }
    eligible
}

fn stack_item(row: &Row, eligible: bool) -> ListItem<'static> {
    // Status glyph: conflict > merge verdict > plain.
    let (glyph, style) = if row.has_conflict {
        ("⚠", Style::new().fg(Color::Red))
    } else {
        match &row.detail {
            Detail::Loaded(d) => merge_glyph(d, eligible),
            Detail::Loading => ("◌", Style::new().fg(Color::Cyan)),
            _ => ("●", Style::new().fg(Color::DarkGray)),
        }
    };
    let pr = match row.pr_number {
        Some(n) => format!("PR #{n}"),
        None => "no PR".to_string(),
    };
    ListItem::new(Line::from(vec![
        Span::styled(glyph, style),
        Span::raw(" "),
        Span::styled(row.short.clone(), Style::new().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::raw(truncate(&row.summary, 26)),
        Span::raw("  "),
        Span::styled(pr, Style::new().fg(Color::DarkGray)),
    ]))
}

/// Left-list glyph for a loaded PR, accounting for stack-order eligibility.
fn merge_glyph(d: &PrDetail, eligible: bool) -> (&'static str, Style) {
    match d.state {
        PrState::Merged => ("●", Style::new().fg(Color::Blue)),
        PrState::Closed => ("●", Style::new().fg(Color::DarkGray)),
        _ if d.verdict.ready && eligible => ("●", Style::new().fg(Color::Green)),
        // Clean per GitHub, but lower PRs must merge first.
        _ if d.verdict.ready => ("●", Style::new().fg(Color::Blue)),
        _ => ("●", Style::new().fg(Color::Yellow)),
    }
}

/// Compact PR status for the top of the right pane (checks minimized by default).
fn status_header(row: &Row, eligible: bool, show_checks: bool) -> Vec<Line<'static>> {
    match &row.detail {
        Detail::Loading => vec![
            Line::from(format!("{}  {}", row.short, row.summary)),
            Line::from(Span::styled("◌ Loading PR data…", Style::new().fg(Color::Cyan))),
        ],
        Detail::NoPr => vec![
            Line::from(format!("{}  {}", row.short, row.summary)),
            Line::from("No PR opened for this change yet.").style(Style::new().fg(Color::DarkGray)),
        ],
        Detail::Unavailable(why) => vec![
            Line::from(format!("{}  {}", row.short, row.summary)),
            Line::from(Span::styled(
                format!("PR data unavailable: {why}"),
                Style::new().fg(Color::Yellow),
            )),
        ],
        Detail::Loaded(d) => loaded_header(d, eligible, show_checks),
    }
}

fn loaded_header(d: &PrDetail, eligible: bool, show_checks: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Title + state.
    lines.push(Line::from(vec![
        Span::styled(format!("#{} ", d.number), Style::new().fg(Color::DarkGray)),
        Span::styled(d.title.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(format!("[{}]", state_label(d.state)), Style::new().fg(Color::DarkGray)),
    ]));

    // Stack-aware merge verdict.
    let (color, text) = match d.state {
        PrState::Merged => (Color::Blue, "✓ Merged".to_string()),
        PrState::Closed => (Color::DarkGray, "Closed".to_string()),
        _ if d.verdict.ready && eligible => (Color::Green, format!("✓ {}", d.verdict.summary)),
        _ if d.verdict.ready => (
            Color::Blue,
            "• Ready, but merge the PR(s) below it in the stack first".to_string(),
        ),
        _ => (Color::Yellow, format!("✗ {}", d.verdict.summary)),
    };
    let mut verdict = vec![Span::styled(
        text,
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )];
    if d.state == PrState::Open {
        verdict.push(Span::styled(
            format!("  ({})", d.mergeable_state),
            Style::new().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(verdict));

    // Checks: a one-line summary by default; the full list when expanded (`c`).
    if d.checks.is_empty() {
        lines.push(Line::from("no checks reported").style(Style::new().fg(Color::DarkGray)));
    } else if show_checks {
        lines.push(Line::from(Span::styled(
            format!("Checks ({})", d.checks.len()),
            Style::new().add_modifier(Modifier::BOLD),
        )));
        let mut ordered: Vec<&pr::CheckLine> = d.checks.iter().collect();
        ordered.sort_by_key(|c| check_sort_key(c.outcome));
        for c in ordered {
            let (glyph, style) = check_glyph(c.outcome);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, style),
                Span::raw(" "),
                Span::raw(c.name.clone()),
            ]));
        }
    } else {
        let count = |o| d.checks.iter().filter(|c| c.outcome == o).count();
        let (passed, failed, running) = (
            count(CheckOutcome::Passed),
            count(CheckOutcome::Failed),
            count(CheckOutcome::Running),
        );
        lines.push(Line::from(vec![
            Span::raw("checks: "),
            Span::styled(format!("✓ {passed}"), Style::new().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(format!("✗ {failed}"), Style::new().fg(Color::Red)),
            Span::raw("  "),
            Span::styled(format!("• {running}"), Style::new().fg(Color::Yellow)),
            Span::styled("   (c expands)", Style::new().fg(Color::DarkGray)),
        ]));
        if failed > 0 {
            let names: Vec<String> = d
                .checks
                .iter()
                .filter(|c| c.outcome == CheckOutcome::Failed)
                .map(|c| c.name.clone())
                .collect();
            lines.push(Line::from(Span::styled(
                format!("  failing: {}", names.join(", ")),
                Style::new().fg(Color::Red),
            )));
        }
    }
    lines
}

fn state_label(s: PrState) -> &'static str {
    s.label()
}

fn check_sort_key(o: CheckOutcome) -> u8 {
    match o {
        CheckOutcome::Failed => 0,
        CheckOutcome::Running => 1,
        CheckOutcome::Neutral => 2,
        CheckOutcome::Skipped => 3,
        CheckOutcome::Passed => 4,
    }
}

fn check_glyph(o: CheckOutcome) -> (&'static str, Style) {
    match o {
        CheckOutcome::Passed => ("✓", Style::new().fg(Color::Green)),
        CheckOutcome::Failed => ("✗", Style::new().fg(Color::Red)),
        CheckOutcome::Running => ("•", Style::new().fg(Color::Yellow)),
        CheckOutcome::Skipped => ("–", Style::new().fg(Color::DarkGray)),
        CheckOutcome::Neutral => ("·", Style::new().fg(Color::DarkGray)),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::{CheckLine, Verdict};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_app() -> App {
        let detail = PrDetail {
            number: 42,
            title: "Add login form".into(),
            state: PrState::Open,
            mergeable_state: "unstable",
            verdict: Verdict {
                ready: false,
                summary: "checks failing".into(),
            },
            checks: vec![
                CheckLine { name: "build".into(), outcome: CheckOutcome::Passed },
                CheckLine { name: "unit-tests".into(), outcome: CheckOutcome::Failed },
            ],
            url: "https://github.com/o/r/pull/42".into(),
        };
        App {
            stacks: vec!["feature".into()],
            stack_idx: 0,
            name: "feature".into(),
            rows: vec![
                Row {
                    short: "abc12345".into(),
                    commit: "c0ffee01".into(),
                    summary: "Add login form".into(),
                    pr_number: Some(42),
                    has_conflict: false,
                    detail: Detail::Loaded(Box::new(detail)),
                    diff: None,
                },
                Row {
                    short: "def67890".into(),
                    commit: "c0ffee02".into(),
                    summary: "WIP".into(),
                    pr_number: None,
                    has_conflict: false,
                    detail: Detail::NoPr,
                    diff: None,
                },
            ],
            selected: 0,
            footer: String::new(),
            diff_scroll: 0,
            show_checks: false,
            gh: None,
            outstanding: 0,
            refreshing: false,
        }
    }

    fn rendered(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_stack_title_and_pr_detail() {
        let content = rendered(&sample_app(), 160, 30);
        assert!(content.contains("Stack: feature"));
        assert!(content.contains("#42"));
        assert!(content.contains("Add login form"));
        assert!(content.contains("unit-tests")); // failing check is listed
        assert!(content.contains("✗")); // failing glyph
        assert!(content.contains("✓")); // passing glyph
        // footer advertises the action keys
        assert!(content.contains("o open"));
        assert!(content.contains("p submit"));
        assert!(content.contains("s sync"));
        assert!(content.contains("l land"));
        assert!(content.contains("a add"));
        assert!(content.contains("q quit"));
    }

    #[test]
    fn second_row_has_no_pr_panel() {
        let mut app = sample_app();
        app.selected = 1;
        let content = rendered(&app, 160, 30);
        assert!(content.contains("No PR opened for this change yet."));
    }

    #[test]
    fn bottom_state_classifies_the_last_row() {
        // two_ready_app: bottom (last) is a ready, open PR.
        assert_eq!(bottom_state(&two_ready_app().rows), BottomState::Ready);
        // sample_app: bottom (last) has no PR.
        assert_eq!(bottom_state(&sample_app().rows), BottomState::Other);

        // A merged bottom classifies as Merged.
        let mut app = two_ready_app();
        if let Detail::Loaded(d) = &mut app.rows.last_mut().unwrap().detail {
            d.state = PrState::Merged;
        }
        assert_eq!(bottom_state(&app.rows), BottomState::Merged);
    }

    #[test]
    fn multiple_stacks_show_switcher_affordances() {
        let mut app = sample_app();
        app.stacks = vec!["alpha".into(), "feature".into()];
        app.stack_idx = 1;
        let content = rendered(&app, 160, 30);
        assert!(content.contains("[2/2]")); // header position indicator
        assert!(content.contains("⇥ stack")); // footer hint

        // With a single stack, neither affordance appears.
        let single = rendered(&sample_app(), 160, 30);
        assert!(!single.contains("[1/1]"));
        assert!(!single.contains("⇥ stack"));
    }

    #[test]
    fn refreshing_shows_footer_indicator() {
        let mut app = sample_app();
        app.refreshing = true;
        let content = rendered(&app, 160, 30);
        assert!(content.contains("checking"));
    }

    #[test]
    fn loading_row_shows_loading_state() {
        let mut app = sample_app();
        app.rows[0].detail = Detail::Loading;
        let content = rendered(&app, 160, 30);
        assert!(content.contains("Loading PR data"));
    }

    fn ready_pr(number: u64) -> Detail {
        Detail::Loaded(Box::new(PrDetail {
            number,
            title: format!("change {number}"),
            state: PrState::Open,
            mergeable_state: "clean",
            verdict: Verdict {
                ready: true,
                summary: "Ready to merge".into(),
            },
            checks: vec![],
            url: String::new(),
        }))
    }

    fn two_ready_app() -> App {
        App {
            stacks: vec!["feature".into()],
            stack_idx: 0,
            name: "feature".into(),
            rows: vec![
                // head-first: index 0 = top, index 1 = bottom (nearest trunk)
                Row {
                    short: "top00000".into(),
                    commit: "c0ffee03".into(),
                    summary: "top".into(),
                    pr_number: Some(2),
                    has_conflict: false,
                    detail: ready_pr(2),
                    diff: None,
                },
                Row {
                    short: "bot00000".into(),
                    commit: "c0ffee04".into(),
                    summary: "bot".into(),
                    pr_number: Some(1),
                    has_conflict: false,
                    detail: ready_pr(1),
                    diff: None,
                },
            ],
            selected: 0,
            footer: String::new(),
            diff_scroll: 0,
            show_checks: false,
            gh: None,
            outstanding: 0,
            refreshing: false,
        }
    }

    #[test]
    fn higher_clean_pr_must_wait_for_lower() {
        let mut app = two_ready_app();

        // Both PRs are individually "clean", but only the bottom is mergeable now.
        let eligible = compute_eligibility(&app.rows);
        assert_eq!(eligible, vec![false, true]);

        // Top PR (selected) shows the "merge lower first" state, not green-ready.
        app.selected = 0;
        let top = rendered(&app, 160, 30);
        assert!(top.contains("merge the PR(s) below"));

        // Bottom PR is genuinely ready to merge.
        app.selected = 1;
        let bottom = rendered(&app, 160, 30);
        assert!(bottom.contains("✓ Ready to merge"));
    }
}
