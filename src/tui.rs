//! Interactive TUI shown when `stacker` is run with no subcommand.
//!
//! Left pane: the stack (head on top). Right pane: detail for the selected
//! change's PR — whether it's ready to merge and which checks are failing.

use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Result, bail};
use octocrab::Octocrab;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

use crate::config::Config;
use crate::github::client;
use crate::github::pr::{self, CheckOutcome, PrDetail, PrState};
use crate::github::remote::{self, GitHubRepo};
use crate::jj::repo;
use crate::paths;
use crate::stack;
use crate::store::Store;

/// PR data state for one row.
enum Detail {
    NoPr,
    Unavailable(String),
    Loaded(Box<PrDetail>),
}

struct Row {
    short: String,
    summary: String,
    bookmark: Option<String>,
    pr_number: Option<u64>,
    has_conflict: bool,
    detail: Detail,
}

struct App {
    name: String,
    rows: Vec<Row>,
    selected: usize,
    footer: String,
    gh: Option<(Octocrab, GitHubRepo)>,
}

impl App {
    fn next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
        }
    }
    fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Fetch PR detail for every row that has a PR.
    async fn fetch_all(&mut self) {
        let Some((client, repo)) = &self.gh else {
            return;
        };
        for row in &mut self.rows {
            let Some(n) = row.pr_number else {
                row.detail = Detail::NoPr;
                continue;
            };
            row.detail = match pr::fetch_detail(client, repo, n).await {
                Ok(d) => Detail::Loaded(Box::new(d)),
                Err(e) => Detail::Unavailable(format!("{e:#}")),
            };
        }
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
    let loaded = repo::load(&cwd).await?;
    let state_dir = paths::repo_state_dir(loaded.repo_path())?;
    let config = Config::load(&state_dir)?;
    let store = Store::load(&state_dir)?;

    if store.stacks.is_empty() {
        println!(
            "No stacks tracked yet. Use `stacker create <name>` or `stacker import <name>`."
        );
        return Ok(());
    }

    // The stack containing @, else the first tracked stack.
    let name = stack::infer_stack_name(&loaded, &store)
        .unwrap_or_else(|_| store.stacks.keys().next().cloned().unwrap());
    let resolved = stack::resolve(&loaded.repo, &store, &name, &config.trunk)?;

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
            summary: e.summary.clone(),
            bookmark: e.bookmark.clone(),
            pr_number: e.pr_number,
            has_conflict: e.has_conflict,
            detail: if e.pr_number.is_some() && gh.is_none() {
                Detail::Unavailable("no GitHub auth/remote".into())
            } else {
                Detail::NoPr
            },
        })
        .collect();

    let mut app = App {
        name,
        rows,
        selected: 0,
        footer: String::new(),
        gh,
    };

    if app.gh.is_some() {
        eprintln!("Loading PR data…");
        app.fetch_all().await;
    } else {
        app.footer = "no GitHub auth/remote — PR data unavailable".into();
    }

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Down | KeyCode::Char('j') => app.next(),
            KeyCode::Up | KeyCode::Char('k') => app.prev(),
            KeyCode::Char('r') if app.gh.is_some() => {
                app.footer = "refreshing…".into();
                terminal.draw(|f| ui(f, app))?;
                app.fetch_all().await;
                app.footer.clear();
            }
            _ => {}
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(f.area());
    let cols = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(rows[0]);

    // Left: the stack.
    let items: Vec<ListItem> = app.rows.iter().map(stack_item).collect();
    let list = List::new(items)
        .block(Block::bordered().title(format!(" Stack: {} ", app.name)))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    f.render_stateful_widget(list, cols[0], &mut list_state);

    // Right: detail for the selected row.
    let detail = app
        .rows
        .get(app.selected)
        .map(detail_lines)
        .unwrap_or_default();
    let title = app
        .rows
        .get(app.selected)
        .and_then(|r| r.pr_number)
        .map(|n| format!(" PR #{n} "))
        .unwrap_or_else(|| " Pull request ".into());
    let para = Paragraph::new(detail)
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(para, cols[1]);

    // Footer.
    let help = "↑/↓ move   r refresh   q quit";
    let footer = if app.footer.is_empty() {
        help.to_string()
    } else {
        format!("{}   |   {help}", app.footer)
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::new().fg(Color::DarkGray)),
        rows[1],
    );
}

fn stack_item(row: &Row) -> ListItem<'static> {
    // Status glyph: conflict > merge verdict > plain.
    let (glyph, style) = if row.has_conflict {
        ("⚠", Style::new().fg(Color::Red))
    } else {
        match &row.detail {
            Detail::Loaded(d) if d.verdict.ready => ("●", Style::new().fg(Color::Green)),
            Detail::Loaded(_) => ("●", Style::new().fg(Color::Yellow)),
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

fn detail_lines(row: &Row) -> Vec<Line<'static>> {
    match &row.detail {
        Detail::NoPr => vec![
            Line::from(format!("{}  {}", row.short, row.summary)),
            bookmark_line(row),
            Line::raw(""),
            Line::from("No PR opened for this change yet.").style(Style::new().fg(Color::DarkGray)),
            Line::from("Run `stacker submit` to open it.").style(Style::new().fg(Color::DarkGray)),
        ],
        Detail::Unavailable(why) => vec![
            Line::from(format!("{}  {}", row.short, row.summary)),
            bookmark_line(row),
            Line::raw(""),
            Line::from(Span::styled(
                format!("PR data unavailable: {why}"),
                Style::new().fg(Color::Yellow),
            )),
        ],
        Detail::Loaded(d) => loaded_detail_lines(row, d),
    }
}

fn loaded_detail_lines(row: &Row, d: &PrDetail) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("#{}  {}", d.number, d.title),
            Style::new().add_modifier(Modifier::BOLD),
        )),
        bookmark_line(row),
        Line::from(format!("State: {}", state_label(d.state))),
        Line::raw(""),
    ];

    let (color, prefix) = if d.verdict.ready {
        (Color::Green, "✓ ")
    } else {
        (Color::Yellow, "✗ ")
    };
    lines.push(Line::from(vec![
        Span::raw("Merge: "),
        Span::styled(
            format!("{prefix}{}", d.verdict.summary),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(format!("Mergeable state: {}", d.mergeable_state)));
    lines.push(Line::raw(""));

    lines.push(Line::from(Span::styled(
        format!("Checks ({})", d.checks.len()),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    if d.checks.is_empty() {
        lines.push(Line::from("  No checks reported.").style(Style::new().fg(Color::DarkGray)));
    } else {
        // Failing first, then running, then the rest — most relevant on top.
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
    }

    if !d.url.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(d.url.clone()).style(Style::new().fg(Color::DarkGray)));
    }
    lines
}

fn bookmark_line(row: &Row) -> Line<'static> {
    match &row.bookmark {
        Some(b) => Line::from(format!("Bookmark: {b}")).style(Style::new().fg(Color::DarkGray)),
        None => Line::from("Bookmark: (none)").style(Style::new().fg(Color::DarkGray)),
    }
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
            name: "feature".into(),
            rows: vec![
                Row {
                    short: "abc12345".into(),
                    summary: "Add login form".into(),
                    bookmark: Some("wd/login".into()),
                    pr_number: Some(42),
                    has_conflict: false,
                    detail: Detail::Loaded(Box::new(detail)),
                },
                Row {
                    short: "def67890".into(),
                    summary: "WIP".into(),
                    bookmark: None,
                    pr_number: None,
                    has_conflict: false,
                    detail: Detail::NoPr,
                },
            ],
            selected: 0,
            footer: String::new(),
            gh: None,
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
        assert!(content.contains("q quit")); // footer help
    }

    #[test]
    fn second_row_has_no_pr_panel() {
        let mut app = sample_app();
        app.selected = 1;
        let content = rendered(&app, 160, 30);
        assert!(content.contains("No PR opened for this change yet."));
    }
}
