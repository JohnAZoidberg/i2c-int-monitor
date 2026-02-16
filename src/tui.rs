use std::collections::{HashMap, VecDeque};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph, Row, Table};

use crate::discovery::{self, I2cTopology, InterruptSourceInfo};
use crate::interrupts;

/// Colors for individual interrupt sources - controllers get one set, HID devices get brighter variants.
const CONTROLLER_COLORS: [Color; 4] = [Color::Blue, Color::Magenta, Color::Red, Color::Yellow];

const HID_COLORS: [Color; 4] = [
    Color::Cyan,
    Color::LightMagenta,
    Color::LightRed,
    Color::LightYellow,
];

/// Color for the TOTAL line.
const TOTAL_COLOR: Color = Color::White;

/// Maximum data points per source (scrolling window).
const MAX_POINTS: usize = 300;

/// Target Y-axis labels.
const TARGET_Y_LABELS: f64 = 5.0;

fn nice_step(max_value: f64) -> f64 {
    if max_value <= 0.0 {
        return 1.0;
    }
    let raw_step = max_value / TARGET_Y_LABELS;
    let exponent = raw_step.log10().floor();
    let fraction = raw_step / 10_f64.powf(exponent);
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_fraction * 10_f64.powf(exponent)
}

fn ceil_to_step(value: f64, step: f64) -> f64 {
    (value / step).ceil() * step
}

/// History for a single interrupt source.
struct SourceHistory {
    /// IRQ number
    irq: String,
    /// Display name
    name: String,
    /// Device type (e.g., "Touchpad")
    device_type: String,
    /// Whether this is a controller
    is_controller: bool,
    /// Assigned color index (stable across visibility changes)
    color_idx: usize,
    /// Time series: (elapsed_s, rate_per_s)
    data: VecDeque<(f64, f64)>,
    /// Previous sample's count
    prev_count: u64,
    /// Latest rate
    latest_rate: f64,
    /// Running statistics
    rate_sum: f64,
    rate_min: f64,
    rate_max: f64,
    /// Whether visible on chart
    visible: bool,
}

impl SourceHistory {
    fn new(info: &InterruptSourceInfo, initial_count: u64, color_idx: usize) -> Self {
        Self {
            irq: info.irq.clone(),
            name: info.name.clone(),
            device_type: info.device_type.clone(),
            is_controller: info.is_controller,
            color_idx,
            data: VecDeque::with_capacity(MAX_POINTS),
            prev_count: initial_count,
            latest_rate: 0.0,
            rate_sum: 0.0,
            rate_min: f64::MAX,
            rate_max: f64::MIN,
            visible: true,
        }
    }

    fn push(&mut self, elapsed_s: f64, count: u64, interval_s: f64) {
        let delta = count.saturating_sub(self.prev_count);
        let rate = delta as f64 / interval_s;

        if self.data.len() >= MAX_POINTS {
            self.data.pop_front();
        }
        self.data.push_back((elapsed_s, rate));

        self.prev_count = count;
        self.latest_rate = rate;
        self.rate_sum += rate;
        self.rate_min = self.rate_min.min(rate);
        self.rate_max = self.rate_max.max(rate);
    }

    fn color(&self) -> Color {
        if self.is_controller {
            CONTROLLER_COLORS[self.color_idx % CONTROLLER_COLORS.len()]
        } else {
            HID_COLORS[self.color_idx % HID_COLORS.len()]
        }
    }

    fn display_name(&self) -> String {
        if self.is_controller {
            self.name.clone()
        } else {
            // Use tree character for hierarchy
            format!("  {} {}", "\u{2514}\u{2500}", self.name)
        }
    }
}

/// Application state.
pub struct App {
    sources: Vec<SourceHistory>,
    total_history: VecDeque<(f64, f64)>,
    total_latest: f64,
    total_sum: f64,
    total_min: f64,
    total_max: f64,
    sample_count: u32,
    start: Instant,
    interval_ms: u64,
    pub should_quit: bool,
    selected_idx: usize,
    total_visible: bool,
    threshold: f64,
}

impl App {
    pub fn new(interval_ms: u64, threshold: f64) -> Self {
        Self {
            sources: Vec::new(),
            total_history: VecDeque::with_capacity(MAX_POINTS),
            total_latest: 0.0,
            total_sum: 0.0,
            total_min: f64::MAX,
            total_max: f64::MIN,
            sample_count: 0,
            start: Instant::now(),
            interval_ms,
            should_quit: false,
            selected_idx: 0,
            total_visible: true,
            threshold,
        }
    }

    /// Initialize from discovered topology.
    pub fn init_from_topology(
        &mut self,
        topology: &I2cTopology,
        initial_counts: &HashMap<String, u64>,
    ) {
        self.sources.clear();

        let sources = topology.all_sources();
        let mut controller_idx = 0usize;
        let mut hid_idx = 0usize;

        for info in &sources {
            let count = initial_counts.get(&info.irq).copied().unwrap_or(0);
            let color_idx = if info.is_controller {
                let idx = controller_idx;
                controller_idx += 1;
                idx
            } else {
                let idx = hid_idx;
                hid_idx += 1;
                idx
            };
            self.sources
                .push(SourceHistory::new(info, count, color_idx));
        }
    }

    fn selectable_count(&self) -> usize {
        self.sources.len() + 1
    }

    fn select_prev(&mut self) {
        let count = self.selectable_count();
        if count == 0 {
            return;
        }
        if self.selected_idx > 0 {
            self.selected_idx -= 1;
        } else {
            self.selected_idx = count - 1;
        }
    }

    fn select_next(&mut self) {
        let count = self.selectable_count();
        if count == 0 {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % count;
    }

    fn toggle_visibility(&mut self) {
        if self.selected_idx < self.sources.len() {
            self.sources[self.selected_idx].visible = !self.sources[self.selected_idx].visible;
        } else {
            self.total_visible = !self.total_visible;
        }
    }

    fn elapsed_s(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn y_max(&self) -> f64 {
        let mut max = 0.0f64;
        for source in &self.sources {
            if !source.visible {
                continue;
            }
            for &(_, rate) in &source.data {
                max = max.max(rate);
            }
        }
        if self.total_visible {
            for &(_, rate) in &self.total_history {
                max = max.max(rate);
            }
        }
        let raw_max = (max * 1.1).max(10.0);
        let step = nice_step(raw_max);
        ceil_to_step(raw_max, step)
    }

    fn y_labels(&self, y_max: f64) -> Vec<Span<'static>> {
        let step = nice_step(y_max);
        let mut labels = Vec::new();
        let mut y = 0.0;
        while y <= y_max + step * 0.01 {
            if y == y.floor() {
                labels.push(Span::raw(format!("{:.0}/s", y)));
            } else {
                labels.push(Span::raw(format!("{:.1}/s", y)));
            }
            y += step;
        }
        labels
    }

    fn x_bounds(&self) -> [f64; 2] {
        let elapsed = self.elapsed_s();
        if elapsed <= 60.0 {
            [0.0, 60.0f64.max(elapsed)]
        } else {
            [elapsed - 60.0, elapsed]
        }
    }

    /// Update with new interrupt data.
    pub fn sample(&mut self, irq_counts: &HashMap<String, u64>) {
        let elapsed = self.elapsed_s();
        let interval_s = self.interval_ms as f64 / 1000.0;
        let mut total_rate = 0.0;

        for source in &mut self.sources {
            if let Some(&count) = irq_counts.get(&source.irq) {
                source.push(elapsed, count, interval_s);
                // Sum all sources for total (both controllers and HID devices represent real interrupts)
                total_rate += source.latest_rate;
            }
        }

        if self.total_history.len() >= MAX_POINTS {
            self.total_history.pop_front();
        }
        self.total_history.push_back((elapsed, total_rate));
        self.total_latest = total_rate;
        self.total_sum += total_rate;
        self.total_min = self.total_min.min(total_rate);
        self.total_max = self.total_max.max(total_rate);
        self.sample_count += 1;
    }
}

/// RAII terminal guard.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        stdout
            .execute(EnterAlternateScreen)
            .context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = self.terminal.backend_mut().execute(LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Run the TUI dashboard.
pub fn run(interval_ms: u64, threshold: f64) -> Result<()> {
    // Discover topology
    let topology = discovery::discover()?;

    if topology.controllers.is_empty() {
        anyhow::bail!(
            "No I2C controllers with HID devices found.\n\
             This may mean:\n\
             - No I2C HID device is present\n\
             - The touchpad uses a different driver (PS/2, USB)\n\
             - The I2C controller uses a different driver"
        );
    }

    // Get initial interrupt counts
    let initial_sources = interrupts::read_interrupts()?;
    let initial_counts: HashMap<String, u64> = initial_sources
        .iter()
        .map(|s| (s.irq.clone(), s.count))
        .collect();

    let mut app = App::new(interval_ms, threshold);
    app.init_from_topology(&topology, &initial_counts);

    if app.sources.is_empty() {
        anyhow::bail!("No interrupt sources found for the discovered I2C devices.");
    }

    let mut guard = TerminalGuard::new()?;
    let interval_duration = Duration::from_millis(interval_ms);
    let mut next_sample = Instant::now() + interval_duration;

    while !app.should_quit {
        guard.terminal.draw(|frame| ui(frame, &app))?;

        let now = Instant::now();
        let timeout = if next_sample > now {
            next_sample - now
        } else {
            Duration::ZERO
        };

        if event::poll(timeout).context("event poll failed")?
            && let Event::Key(key) = event::read().context("event read failed")?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code);
        }

        if Instant::now() >= next_sample {
            let sources = interrupts::read_interrupts()?;
            let counts: HashMap<String, u64> =
                sources.iter().map(|s| (s.irq.clone(), s.count)).collect();
            app.sample(&counts);
            next_sample = Instant::now() + interval_duration;
        }
    }

    drop(guard);
    print_summary(&app);

    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Char(' ') => app.toggle_visibility(),
        _ => {}
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let table_height = (app.sources.len() + 4) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(table_height.min(15)),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_chart(frame, app, chunks[0]);
    render_table(frame, app, chunks[1]);
    render_status_bar(frame, app, chunks[2]);
}

fn render_chart(frame: &mut Frame, app: &App, area: Rect) {
    let x_bounds = app.x_bounds();
    let y_max = app.y_max();

    // Collect data for all sources
    let data_vecs: Vec<Vec<(f64, f64)>> = app
        .sources
        .iter()
        .map(|s| s.data.iter().copied().collect())
        .collect();

    let total_data_vec: Vec<(f64, f64)> = app.total_history.iter().copied().collect();

    // Build datasets - use the source's assigned color
    let mut datasets: Vec<Dataset> = Vec::new();

    for (i, source) in app.sources.iter().enumerate() {
        if !source.visible || data_vecs[i].is_empty() {
            continue;
        }
        datasets.push(
            Dataset::default()
                .name(source.name.as_str())
                .marker(Marker::Braille)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(source.color()))
                .data(&data_vecs[i]),
        );
    }

    if app.total_visible && !total_data_vec.is_empty() {
        datasets.push(
            Dataset::default()
                .name("TOTAL")
                .marker(Marker::Braille)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(
                    Style::default()
                        .fg(TOTAL_COLOR)
                        .add_modifier(Modifier::BOLD),
                )
                .data(&total_data_vec),
        );
    }

    let x_labels = vec![
        Span::raw(format!("{:.0}s", x_bounds[0])),
        Span::raw(format!("{:.0}s", (x_bounds[0] + x_bounds[1]) / 2.0)),
        Span::raw(format!("{:.0}s", x_bounds[1])),
    ];
    let y_labels = app.y_labels(y_max);

    let title = if app.threshold > 0.0 {
        format!(" Interrupt Monitor (threshold: {:.0}/s) ", app.threshold)
    } else {
        " Interrupt Monitor ".to_string()
    };

    let chart = Chart::new(datasets)
        .block(Block::default().title(title).borders(Borders::ALL))
        .x_axis(
            Axis::default()
                .title("Time")
                .bounds(x_bounds)
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .title("Interrupts/s")
                .bounds([0.0, y_max])
                .labels(y_labels),
        );

    frame.render_widget(chart, area);
}

fn render_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec!["", "Source", "Type", "IRQ", "Rate", "Avg", "Max"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(0);

    let mut rows: Vec<Row> = Vec::new();

    for (i, source) in app.sources.iter().enumerate() {
        let is_selected = app.selected_idx == i;
        let color = if !source.visible {
            Color::DarkGray
        } else {
            source.color()
        };

        let status = if is_selected { ">" } else { " " }.to_string();

        let rate_str = format!("{:.1}/s", source.latest_rate);
        let avg = if app.sample_count > 0 {
            source.rate_sum / app.sample_count as f64
        } else {
            0.0
        };
        let avg_str = format!("{:.1}/s", avg);
        let max_str = if source.rate_max == f64::MIN {
            "-".to_string()
        } else {
            format!("{:.1}/s", source.rate_max)
        };

        let mut style = Style::default().fg(color);
        if source.latest_rate > app.threshold && app.threshold > 0.0 && source.visible {
            style = style.bg(Color::DarkGray);
        }
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }

        // Show hierarchy with indentation
        let display_name = source.display_name();
        let type_str = if source.is_controller {
            "Controller".to_string()
        } else {
            source.device_type.clone()
        };

        rows.push(
            Row::new(vec![
                status,
                display_name,
                type_str,
                format!("IRQ {}", source.irq),
                rate_str,
                avg_str,
                max_str,
            ])
            .style(style),
        );
    }

    // Total row
    let is_total_selected = app.selected_idx == app.sources.len();
    let total_color = if !app.total_visible {
        Color::DarkGray
    } else {
        TOTAL_COLOR
    };
    let total_status = if is_total_selected { ">" } else { " " }.to_string();
    let mut total_style = Style::default()
        .fg(total_color)
        .add_modifier(Modifier::BOLD);
    if is_total_selected {
        total_style = total_style.add_modifier(Modifier::REVERSED);
    }

    let total_avg = if app.sample_count > 0 {
        app.total_sum / app.sample_count as f64
    } else {
        0.0
    };
    let total_max_str = if app.total_max == f64::MIN {
        "-".to_string()
    } else {
        format!("{:.1}/s", app.total_max)
    };

    rows.push(
        Row::new(vec![
            total_status,
            "TOTAL".to_string(),
            String::new(),
            String::new(),
            format!("{:.1}/s", app.total_latest),
            format!("{:.1}/s", total_avg),
            total_max_str,
        ])
        .style(total_style),
    );

    let widths = [
        Constraint::Length(1),
        Constraint::Min(35),
        Constraint::Length(15),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL));

    frame.render_widget(table, area);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let elapsed = app.elapsed_s();
    let text = format!(
        " [q]uit [j/k]sel [space]hide | {:.0}s {}ms #{}",
        elapsed, app.interval_ms, app.sample_count,
    );
    let bar = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(bar, area);
}

fn print_summary(app: &App) {
    if app.sample_count == 0 {
        return;
    }

    println!("\n=== Interrupt Rate Summary ===\n");
    println!(
        "{:<40} {:>12} {:>12} {:>12}",
        "Source", "Avg Rate", "Max Rate", "Type"
    );
    println!("{}", "-".repeat(80));

    for source in &app.sources {
        let avg = source.rate_sum / app.sample_count as f64;
        let max = if source.rate_max == f64::MIN {
            0.0
        } else {
            source.rate_max
        };
        let type_str = if source.is_controller {
            "Controller"
        } else {
            &source.device_type
        };
        println!(
            "{:<40} {:>10.1}/s {:>10.1}/s {:>12}",
            source.display_name(),
            avg,
            max,
            type_str
        );
    }

    let total_avg = app.total_sum / app.sample_count as f64;
    let total_max = if app.total_max == f64::MIN {
        0.0
    } else {
        app.total_max
    };

    println!("{}", "-".repeat(80));
    println!(
        "{:<40} {:>10.1}/s {:>10.1}/s",
        "TOTAL", total_avg, total_max
    );

    println!(
        "\nSamples: {} over {:.1}s\n",
        app.sample_count,
        app.elapsed_s()
    );
}
