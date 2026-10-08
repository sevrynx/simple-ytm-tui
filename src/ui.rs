use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Gauge, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::api::Track;
use crate::app::{App, Focus, Mode};

const ACCENT: Color = Color::Magenta;
const SECONDARY: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [search, lists, now, help] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [results, queue] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(lists);

    draw_search(f, app, search);
    draw_results(f, app, results);
    draw_queue(f, app, queue);
    draw_now_playing(f, app, now);
    draw_help(f, app, help);
}

fn panel(title: Line<'static>, active: bool) -> Block<'static> {
    let border = if active {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title)
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let editing = app.mode == Mode::Search;
    let title = Line::from(vec![
        " Search ".bold(),
        Span::styled(
            format!("[{}] ", app.filter.label()),
            Style::new().fg(SECONDARY),
        ),
    ]);
    let text = if editing {
        Line::from(app.input.clone())
    } else if app.last_query.is_empty() {
        Line::from("press / to search".dark_gray())
    } else {
        Line::from(app.last_query.clone().dark_gray())
    };
    f.render_widget(Paragraph::new(text).block(panel(title, editing)), area);
    if editing {
        let x = area.x + 1 + app.input.chars().count() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + 1));
    }
}

fn track_line(t: &Track, playing: bool) -> ListItem<'static> {
    let mut spans = Vec::new();
    if playing {
        spans.push(Span::styled("▶ ", Style::new().fg(ACCENT).bold()));
    }
    let title_style = if playing {
        Style::new().fg(ACCENT).bold()
    } else {
        Style::new().bold()
    };
    spans.push(Span::styled(t.title.clone(), title_style));
    if !t.artist.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(t.artist.clone(), Style::new().fg(SECONDARY)));
    }
    if !t.length.is_empty() {
        spans.push(Span::styled(
            format!("  {}", t.length),
            Style::new().dark_gray(),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>, active: bool) -> List<'a> {
    let hl = if active {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };
    List::new(items)
        .block(block)
        .highlight_style(hl)
        .highlight_symbol("› ")
}

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let active = app.mode == Mode::Normal && app.focus == Focus::Results;
    let title = if app.searching {
        Line::from(" Results · searching... ".bold())
    } else {
        Line::from(format!(" Results ({}) ", app.results.len()).bold())
    };
    let items = app.results.iter().map(|t| track_line(t, false)).collect();
    f.render_stateful_widget(
        list(items, panel(title, active), active),
        area,
        &mut app.results_state,
    );
}

fn draw_queue(f: &mut Frame, app: &mut App, area: Rect) {
    let active = app.mode == Mode::Normal && app.focus == Focus::Queue;
    let radio = match (app.autoplay, app.radio_busy) {
        (true, true) => Span::styled("radio ↻ ", Style::new().fg(SECONDARY)),
        (true, false) => Span::styled("radio on ", Style::new().fg(SECONDARY)),
        (false, _) => Span::styled("radio off ", Style::new().dark_gray()),
    };
    let title = Line::from(vec![" Up next ".bold(), radio]);
    let items = app
        .queue
        .iter()
        .enumerate()
        .map(|(i, t)| track_line(t, Some(i) == app.pos))
        .collect();
    f.render_stateful_widget(
        list(items, panel(title, active), active),
        area,
        &mut app.queue_state,
    );
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn draw_now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let state = if app.current().is_none() {
        "■"
    } else if app.paused {
        "⏸"
    } else {
        "▶"
    };
    let title = Line::from(vec![
        Span::styled(format!(" {state} "), Style::new().fg(ACCENT).bold()),
        "Now playing ".bold(),
    ]);
    let block = panel(title, false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [l1, l2, l3] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
    let status = app.status_line().map(str::to_owned);

    let Some(t) = app.current().cloned() else {
        let msg = status
            .unwrap_or_else(|| "Nothing playing. Search for something and press Enter".to_owned());
        f.render_widget(Paragraph::new(msg.dark_gray()), l1);
        return;
    };

    let status_w = status.as_ref().map_or(0, |s| s.chars().count() as u16 + 1);
    let [title_area, status_area] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(status_w)]).areas(l1);
    f.render_widget(
        Paragraph::new(Span::styled(t.title.clone(), Style::new().bold())),
        title_area,
    );
    if let Some(msg) = &status {
        f.render_widget(
            Paragraph::new(Span::styled(msg.clone(), Style::new().fg(Color::Yellow)))
                .right_aligned(),
            status_area,
        );
    }

    let mut meta = vec![Span::styled(t.artist.clone(), Style::new().fg(SECONDARY))];
    if !t.album.is_empty() {
        meta.push(Span::styled(
            format!(" · {}", t.album),
            Style::new().dark_gray(),
        ));
    }
    let right = format!("vol {:.0}%", app.volume);
    let [meta_area, vol_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right.len() as u16 + 1),
    ])
    .areas(l2);
    f.render_widget(Paragraph::new(Line::from(meta)), meta_area);
    f.render_widget(Paragraph::new(right.dark_gray()).right_aligned(), vol_area);

    match (app.time, app.duration) {
        (Some(pos), Some(dur)) if dur > 0.0 => {
            let ratio = (pos / dur).clamp(0.0, 1.0);
            let gauge = Gauge::default()
                .gauge_style(Style::new().fg(ACCENT).bg(Color::Reset))
                .use_unicode(true)
                .ratio(ratio)
                .label(Span::styled(
                    format!("{} / {}", fmt_time(pos), fmt_time(dur)),
                    Style::new().fg(Color::Reset).bold(),
                ));
            f.render_widget(gauge, l3);
        }
        _ => f.render_widget(Paragraph::new("Loading...".dark_gray()), l3),
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match (app.mode, app.focus) {
        (Mode::Search, _) => &[
            ("Enter", "search"),
            ("Tab", "songs/videos"),
            ("Esc", "cancel"),
        ],
        (Mode::Normal, Focus::Results) => &[
            ("/", "search"),
            ("⏎", "play"),
            ("a", "next"),
            ("␣", "pause"),
            ("n/b", "skip"),
            ("←→", "seek"),
            ("+-", "vol"),
            ("r", "radio"),
            ("Tab", "⇄"),
            ("q", "quit"),
        ],
        (Mode::Normal, Focus::Queue) => &[
            ("/", "search"),
            ("⏎", "jump"),
            ("d", "remove"),
            ("␣", "pause"),
            ("n/b", "skip"),
            ("←→", "seek"),
            ("+-", "vol"),
            ("r", "radio"),
            ("Tab", "⇄"),
            ("q", "quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, what) in keys {
        spans.push(Span::styled(*k, Style::new().fg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {what}  "), Style::new().dark_gray()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
