//! Drawing the TUI. Pure rendering: reads `App`, never changes it.
//!
//! ┌ Global offset ─────────┐┌ Local offset (this map) ┐
//! │ Apply / current / mean ││ Apply / current / basis │
//! └────────────────────────┘└─────────────────────────┘
//! ┌ osu! ─────────────────────────────────────────────┐
//! │ connection, current global, live play, message    │
//! └───────────────────────────────────────────────────┘
//! ┌ Hit errors (live or last play) ──────────────────┐
//! │ histogram across the 50 window, coloured by zone │
//! └───────────────────────────────────────────────────┘
//! ┌ Recent plays ─────────────────────────────────────┐
//! └───────────────────────────────────────────────────┘
//!  s settings · g/m/a reset · q quit

use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Bar, BarChart, Block, Cell, Clear, Paragraph, Row, Table},
};

use crate::{
    app::{App, Reset, SettingsMenu},
    calc::{self, Aggregation},
    tracker::SkipReason,
    worker::Connection,
};

// osu!'s own judgement colours.
const COLOR_300: Color = Color::Rgb(102, 204, 255);
const COLOR_100: Color = Color::Rgb(136, 221, 85);
const COLOR_50: Color = Color::Rgb(255, 204, 34);

pub fn draw(frame: &mut Frame, app: &App) {
    let [top, status, graph, table, footer] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Min(8),
        Constraint::Length(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [suggestion, local] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(top);

    draw_suggestion(frame, suggestion, app);
    draw_local(frame, local, app);
    draw_status(frame, status, app);
    draw_graph(frame, graph, app);
    draw_history(frame, table, app);
    frame.render_widget(
        Line::from(
            " s settings · g reset global · m reset this map · a reset all maps · q quit",
        )
        .style(Style::new().dark_gray()),
        footer,
    );

    if let Some(menu) = &app.menu {
        draw_settings(frame, menu);
    }
    if let Some(reset) = &app.confirm {
        draw_confirm(frame, app, reset);
    }
}

fn draw_suggestion(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" Global offset ");
    let lines = match app.history.suggestion() {
        None => vec![
            Line::from("No plays recorded yet."),
            Line::from("Play some maps (50+ hits) to get a suggestion.").dark_gray(),
        ],
        Some(suggestion) => {
            let mut lines = vec![Line::from(vec![
                Span::raw("Apply:   "),
                Span::styled(
                    format!("{:+} ms", suggestion.apply),
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ])];
            if let Some(current) = app.global_offset {
                let change = suggestion.apply - current;
                let hint = match change {
                    0 => "already set".to_string(),
                    _ => format!("change {change:+} ms"),
                };
                lines.push(Line::from(format!("Current: {current:+} ms  ({hint})")));
            }
            let plays = app.history.plays().len();
            lines.push(Line::from(format!(
                "Mean:    {:+.2} ms over {plays} play{}",
                suggestion.mean,
                if plays == 1 { "" } else { "s" }
            )));
            if let Some(spread) = spread(app) {
                lines.push(Line::from(format!("Spread:  ±{spread:.1} ms (SD of S)")).dark_gray());
            }
            lines
        }
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_local(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" Local offset (this map) ");
    let Some(beatmap) = &app.beatmap else {
        frame.render_widget(
            Paragraph::new("No beatmap selected.").dark_gray().block(block),
            area,
        );
        return;
    };
    let how = app.settings.map_aggregation;
    let mut lines = vec![Line::from(beatmap.name.clone()).dark_gray()];
    match app
        .map_history
        .suggestion(&beatmap.checksum, how, app.global_offset)
    {
        None => {
            lines.push(Line::from(format!(
                "Current: {:+} ms",
                beatmap.local_offset
            )));
            lines.push(Line::from("No plays on this map yet (this session).").dark_gray());
        }
        Some(local) => {
            lines.push(Line::from(vec![
                Span::raw("Apply:   "),
                Span::styled(
                    format!("{:+} ms", local.apply),
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ]));
            let change = local.apply - beatmap.local_offset;
            let hint = match change {
                0 => "already set".to_string(),
                _ => format!("change {change:+} ms"),
            };
            lines.push(Line::from(format!(
                "Current: {:+} ms  ({hint})",
                beatmap.local_offset
            )));
            let label = match how {
                Aggregation::Mean => "Mean",
                Aggregation::Median => "Median",
            };
            lines.push(Line::from(format!(
                "{label}:    {:+.2} ms over {} play{}",
                local.value,
                local.plays,
                if local.plays == 1 { "" } else { "s" }
            )));
            if let Some(adjusted) = local.adjusted_for_global {
                lines.push(
                    Line::from(format!("With current global: {adjusted:+} ms")).dark_gray(),
                );
            }
        }
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Standard deviation of the per-play suggestions.
fn spread(app: &App) -> Option<f64> {
    let values: Vec<f64> = app.history.plays().iter().map(|p| p.suggestion()).collect();
    if values.len() < 2 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    Some(variance.sqrt())
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" osu! ");
    let connection = match &app.connection {
        Connection::WaitingForOsu => Line::from("Waiting for osu!…").yellow(),
        Connection::Scanning(pid) => Line::from(format!("Scanning memory (pid {pid})…")).yellow(),
        Connection::Connected(pid) => Line::from(format!("Connected (pid {pid})")).green(),
        Connection::Failed(error) => Line::from(format!("Attach failed, retrying: {error}")).red(),
    };
    let mut lines = vec![connection];
    if let Some(offset) = app.global_offset {
        lines.push(Line::from(format!("Global offset in osu!: {offset:+} ms")));
    }
    match &app.live {
        Some(live) => {
            lines.push(Line::from(format!("Playing: {}", live.map_name)));
            lines.push(Line::from(format!(
                "Mods: {}   Hits so far: {}",
                mods_string(live.mods),
                live.errors.len()
            )));
        }
        None => lines.push(Line::from("Not playing").dark_gray()),
    }
    if let Some(message) = &app.message {
        lines.push(Line::from(message.clone()).italic());
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_graph(frame: &mut Frame, area: Rect, app: &App) {
    // Live play if there is one, otherwise the last finished play.
    let (label, errors, od, mods) = match (&app.live, &app.last_play) {
        (Some(live), _) => ("Live", &live.errors, live.od, live.mods),
        (None, Some(last)) => ("Last play", &last.errors, last.od, last.mods),
        (None, None) => {
            let block = Block::bordered().title(" Hit errors ");
            frame.render_widget(
                Paragraph::new("No play yet.").dark_gray().block(block),
                area,
            );
            return;
        }
    };

    let w300 = calc::window_300(od);
    let w100 = calc::cutoff_100(od);
    let w50 = calc::window_50(od);
    let stats = calc::analyze_with_effective_od(errors, od, calc::rate(mods));
    let title = match stats {
        Some(stats) => format!(
            " {label}: M {:+.1} ms · UR {:.1} · {} hits · 300 ±{w300} 100 ±{w100} 50 ±{w50} ",
            stats.median, stats.unstable_rate, stats.hits
        ),
        None => format!(" {label}: {} hits · 300 ±{w300} 100 ±{w100} 50 ±{w50} ", errors.len()),
    };
    let block = Block::bordered()
        .title(title)
        .title_bottom(Line::from(format!(" early −{w50} ms ")).left_aligned())
        .title_bottom(Line::from(" 0 ").centered())
        .title_bottom(Line::from(format!(" late +{w50} ms ")).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // One 1-column bar per bin across [-w50, +w50].
    let bins = (inner.width as usize).max(1);
    let span = (2 * w50 + 1).max(1) as f64;
    let mut counts = vec![0u64; bins];
    for &error in errors {
        if error.abs() > w50 {
            continue;
        }
        let position = (f64::from(error + w50) / span * bins as f64) as usize;
        counts[position.min(bins - 1)] += 1;
    }
    let bars: Vec<Bar> = counts
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            // The ms value at the centre of this bin decides its colour.
            let centre = ((i as f64 + 0.5) / bins as f64 * span) as i32 - w50;
            let color = match centre.abs() {
                d if d < w300 => COLOR_300,
                d if d < w100 => COLOR_100,
                _ => COLOR_50,
            };
            Bar::new(count).style(Style::new().fg(color)).text_value("")
        })
        .collect();
    frame.render_widget(BarChart::new(bars).bar_width(1).bar_gap(0), inner);
}

fn draw_history(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new(["Map", "Mods", "M", "UR", "S"]).style(Style::new().bold());
    let rows = app.history.plays().iter().rev().map(|play| {
        Row::new([
            Cell::from(play.map_name.clone().unwrap_or_else(|| "?".into())),
            Cell::from(mods_string(play.mods)),
            Cell::from(format!("{:+.1}", play.median)),
            Cell::from(format!("{:.0}", play.unstable_rate)),
            Cell::from(format!("{:+.1}", play.suggestion())),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(Block::bordered().title(format!(
        " Recent plays ({}/{}) ",
        app.history.plays().len(),
        app.settings.history_size
    )));
    frame.render_widget(table, area);
}

fn draw_settings(frame: &mut Frame, menu: &SettingsMenu) {
    let [area] = Layout::horizontal([Constraint::Length(46)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(11)])
        .flex(Flex::Center)
        .areas(area);

    let items = [
        format!("History size        < {} plays >", menu.draft.history_size),
        format!("Poll interval       < {} ms >", menu.draft.poll_interval_ms),
        format!(
            "Count Autopilot     [{}]",
            if menu.draft.count_autopilot { "x" } else { " " }
        ),
        format!("Plays per map       < {} >", menu.draft.map_history_size),
        format!(
            "Per-map combine     < {} >",
            match menu.draft.map_aggregation {
                Aggregation::Mean => "mean",
                Aggregation::Median => "median",
            }
        ),
    ];
    let mut lines: Vec<Line> = items
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            if i == menu.selected {
                Line::from(format!("> {text}")).style(Style::new().fg(Color::Cyan).bold())
            } else {
                Line::from(format!("  {text}"))
            }
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from("↑↓ select · ←→ change · Enter save · Esc cancel").dark_gray());

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Settings ")),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, app: &App, reset: &Reset) {
    let question = match reset {
        Reset::Global => format!(
            "Reset the global history ({} plays)?",
            app.history.plays().len()
        ),
        Reset::Map(_, name) => format!("Reset this map's plays?\n{name}"),
        Reset::AllMaps => "Reset the per-map history for every map?".into(),
    };
    let [area] = Layout::horizontal([Constraint::Length(54)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(6)])
        .flex(Flex::Center)
        .areas(area);
    let mut lines: Vec<Line> = question.lines().map(|l| Line::from(l.to_string())).collect();
    lines.push(Line::from(""));
    lines.push(Line::from("y confirm · any other key cancels").dark_gray());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Reset ").red()),
        area,
    );
}

pub fn skip_reason(reason: &SkipReason) -> String {
    match reason {
        SkipReason::NotStandard => "not osu!standard".into(),
        SkipReason::ExcludedMods => "excluded mod (Relax/Auto/Cinema/Autopilot)".into(),
        SkipReason::TooFewHits(hits) => format!("only {hits} hits without 50s (need 50)"),
    }
}

/// Short mod names in osu!'s usual order, e.g. "HDDT".
pub fn mods_string(mods: u32) -> String {
    const NAMES: [(u32, &str); 12] = [
        (1 << 0, "NF"),
        (1 << 1, "EZ"),
        (1 << 3, "HD"),
        (1 << 4, "HR"),
        (1 << 5, "SD"),
        (1 << 9, "NC"),
        (1 << 6, "DT"),
        (1 << 8, "HT"),
        (1 << 10, "FL"),
        (1 << 12, "SO"),
        (1 << 13, "AP"),
        (1 << 14, "PF"),
    ];
    let mut text = String::new();
    for (bit, name) in NAMES {
        // NC implies DT, SD is replaced by PF: show only the stronger one.
        let hidden = (name == "DT" && mods & (1 << 9) != 0) || (name == "SD" && mods & (1 << 14) != 0);
        if mods & bit != 0 && !hidden {
            text.push_str(name);
        }
    }
    if text.is_empty() { "NM".into() } else { text }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{
        calc::PlayStats,
        history::{History, PlayRecord},
        settings::Settings,
        tracker::{FinishedPlay, Outcome},
    };

    fn sample_app() -> App {
        let mut history = History::default();
        for (g, median) in [(-35, 2.0), (-35, 4.0), (-30, -1.0)] {
            let mut record = PlayRecord::new(
                g,
                0,
                64,
                PlayStats { median, unstable_rate: 120.0, hits: 300 },
            );
            record.map_name = Some("Artist - Song [Insane]".into());
            history.push(record, 50);
        }
        let (command_tx, _) = mpsc::channel();
        let (_, update_rx) = mpsc::channel();
        let mut app = App::new(
            Settings::default(),
            history,
            "settings.toml".into(),
            "history.json".into(),
            command_tx,
            update_rx,
        );
        app.global_offset = Some(-35);
        app.beatmap = Some(crate::osu::Beatmap {
            checksum: "abc".into(),
            name: "Artist - Song [Insane]".into(),
            od: 8.0,
            local_offset: 0,
        });
        let mut local = PlayRecord::new(-30, 0, 0, PlayStats { median: 4.6, unstable_rate: 80.0, hits: 200 });
        local.map_checksum = Some("abc".into());
        app.map_history.push(local, 1);
        app.connection = Connection::Connected(1234);
        let errors: Vec<i32> = (0..400).map(|i| (i * 37 % 120) - 55).collect();
        app.last_play = Some(FinishedPlay {
            map_name: "Artist - Song [Insane]".into(),
            mods: 64,
            od: 8.0,
            cutoff: calc::cutoff_100(8.0),
            outcome: Outcome::Skipped(SkipReason::TooFewHits(3)),
            errors,
        });
        app
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_main_screen() {
        let screen = render(&sample_app(), 100, 32);
        println!("{screen}");
        assert!(screen.contains("Global offset"));
        assert!(screen.contains("Local offset (this map)"));
        // +4.6 -> apply +5; global moved -30 -> -35, so adjusted is 0.
        assert!(screen.contains("Apply:   +5 ms"));
        assert!(screen.contains("With current global: +0 ms"));
        assert!(screen.contains("Connected (pid 1234)"));
        assert!(screen.contains("Recent plays (3/50)"));
        assert!(screen.contains("Last play: M"));
    }

    #[test]
    fn renders_settings_menu_and_tiny_terminals() {
        let mut app = sample_app();
        app.menu = Some(SettingsMenu { draft: Settings::default(), selected: 1 });
        let screen = render(&app, 100, 32);
        assert!(screen.contains("> Poll interval       < 100 ms >"));
        assert!(screen.contains("Plays per map       < 1 >"));
        assert!(screen.contains("Per-map combine     < mean >"));
        app.menu = None;
        app.confirm = Some(Reset::Global);
        assert!(render(&app, 100, 32).contains("Reset the global history (3 plays)?"));
        // Must not panic on a very small terminal.
        render(&app, 10, 5);
    }

    #[test]
    fn mod_names() {
        assert_eq!(mods_string(0), "NM");
        assert_eq!(mods_string(8 | 64), "HDDT");
        assert_eq!(mods_string(64 | 512), "NC");
        assert_eq!(mods_string(16 | 64), "HRDT");
    }
}
