//! Browse insights in a scrollable TUI.

use crate::db::SearchResult;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io::stdout;

const CATEGORY_COLORS: &[(&str, Color)] = &[
    ("decision", Color::Yellow),
    ("pattern", Color::Cyan),
    ("learning", Color::Green),
    ("architecture", Color::Magenta),
    ("migration", Color::Red),
    ("bugfix", Color::LightRed),
];

fn category_color(cat: &str) -> Color {
    CATEGORY_COLORS
        .iter()
        .find(|(c, _)| *c == cat)
        .map(|(_, color)| *color)
        .unwrap_or(Color::White)
}

/// Threshold below which we use stacked (full-screen detail) layout.
const WIDE_THRESHOLD: u16 = 100;

enum View {
    List,
    Detail,
}

pub fn run_browse(insights: Vec<SearchResult>, repo_name: &str) -> Result<()> {
    if insights.is_empty() {
        println!("No insights to browse. Run `diwa index` first.");
        return Ok(());
    }

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;

    let mut list_state = ListState::default();
    list_state.select(Some(0));
    let mut view = View::List;
    let mut detail_scroll: u16 = 0;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let wide = area.width >= WIDE_THRESHOLD;

            match (&view, wide) {
                (_, true) => {
                    // Wide: side-by-side
                    let chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                        .split(area);

                    render_list(frame, chunks[0], &insights, &mut list_state, repo_name);

                    if let Some(selected) = list_state.selected() {
                        if let Some(r) = insights.get(selected) {
                            render_detail(frame, chunks[1], r, detail_scroll);
                        }
                    }
                }
                (View::List, false) => {
                    // Narrow: full-width list
                    render_list(frame, area, &insights, &mut list_state, repo_name);
                }
                (View::Detail, false) => {
                    // Narrow: full-screen detail overlay
                    if let Some(selected) = list_state.selected() {
                        if let Some(r) = insights.get(selected) {
                            frame.render_widget(Clear, area);
                            render_detail(frame, area, r, detail_scroll);
                        }
                    }
                }
            }

            // Footer
            let help_text = match (&view, wide) {
                (_, true) | (View::List, false) => {
                    " j/k navigate   Enter detail   q quit "
                }
                (View::Detail, false) => {
                    " j/k scroll   h/Esc back   n/p prev/next   q quit "
                }
            };
            let help = Paragraph::new(Line::from(Span::styled(
                help_text,
                Style::default().fg(Color::DarkGray),
            )));
            let help_area = Rect {
                x: 0,
                y: area.height.saturating_sub(1),
                width: area.width,
                height: 1,
            };
            frame.render_widget(help, help_area);
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            let wide = terminal.size()?.width >= WIDE_THRESHOLD;

            // Helper closures
            let move_down = |ls: &mut ListState, scroll: &mut u16| {
                let i = ls.selected().unwrap_or(0);
                if i < insights.len().saturating_sub(1) {
                    ls.select(Some(i + 1));
                    *scroll = 0;
                }
            };
            let move_up = |ls: &mut ListState, scroll: &mut u16| {
                let i = ls.selected().unwrap_or(0);
                if i > 0 {
                    ls.select(Some(i - 1));
                    *scroll = 0;
                }
            };

            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Esc => match view {
                    View::Detail if !wide => view = View::List,
                    _ => break,
                },

                KeyCode::Down | KeyCode::Char('j') => match view {
                    View::List => move_down(&mut list_state, &mut detail_scroll),
                    View::Detail if wide => move_down(&mut list_state, &mut detail_scroll),
                    View::Detail => detail_scroll = detail_scroll.saturating_add(1),
                },
                KeyCode::Up | KeyCode::Char('k') => match view {
                    View::List => move_up(&mut list_state, &mut detail_scroll),
                    View::Detail if wide => move_up(&mut list_state, &mut detail_scroll),
                    View::Detail => detail_scroll = detail_scroll.saturating_sub(1),
                },

                KeyCode::Enter | KeyCode::Char('l') => {
                    if !wide && matches!(view, View::List) {
                        view = View::Detail;
                        detail_scroll = 0;
                    }
                }
                KeyCode::Char('h') | KeyCode::Backspace => {
                    if matches!(view, View::Detail) && !wide {
                        view = View::List;
                        detail_scroll = 0;
                    }
                }

                KeyCode::Char('n') if matches!(view, View::Detail) => {
                    move_down(&mut list_state, &mut detail_scroll);
                }
                KeyCode::Char('p') if matches!(view, View::Detail) => {
                    move_up(&mut list_state, &mut detail_scroll);
                }

                KeyCode::Home | KeyCode::Char('g') if matches!(view, View::List) => {
                    list_state.select(Some(0));
                    detail_scroll = 0;
                }
                KeyCode::End | KeyCode::Char('G') if matches!(view, View::List) => {
                    list_state.select(Some(insights.len().saturating_sub(1)));
                    detail_scroll = 0;
                }

                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn render_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    insights: &[SearchResult],
    list_state: &mut ListState,
    repo_name: &str,
) {
    let items: Vec<ListItem> = insights
        .iter()
        .map(|r| {
            let color = category_color(&r.category);
            let date = r.commit_date.split('T').next().unwrap_or(&r.commit_date);

            let line = Line::from(vec![
                Span::styled(
                    format!("{} ", date),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("[{}] ", r.category),
                    Style::default().fg(color),
                ),
                Span::raw(&r.title),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" diwa  {}  ({}) ", repo_name, insights.len())),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, list_state);
}

fn render_detail(frame: &mut ratatui::Frame, area: Rect, r: &SearchResult, scroll: u16) {
    let color = category_color(&r.category);
    let date = r.commit_date.split('T').next().unwrap_or(&r.commit_date);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            &r.title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(format!("[{}]", r.category), Style::default().fg(color)),
            Span::styled(
                format!("  {}  {}", date, &r.commit_sha[..7.min(r.commit_sha.len())]),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
    ];

    for line in r.body.lines() {
        lines.push(Line::from(line.to_string()));
    }

    if !r.tags.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("tags: {}", r.tags),
            Style::default().fg(Color::DarkGray),
        )));
    }

    if !r.files.is_empty() {
        lines.push(Line::from(""));
        for f in &r.files {
            lines.push(Line::from(Span::styled(
                format!("  {f}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let detail = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(detail, area);
}
