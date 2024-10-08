use std::{
    error::Error,
    io::{self},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use ratatui::{
    backend::{Backend, CrosstermBackend},
    crossterm::{
        self,
        cursor::{Hide, Show},
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::{self},
    terminal::{Frame, Terminal},
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, Gauge, List, ListItem, Paragraph, Widget, Wrap,
    },
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    commands::UI,
    exec_tokio, ind_keys,
    indicator::{self, Metric},
    mongobar::{op_logs, Mongobar},
};

use crate::mongobar::op_row;

struct App {
    oplog_scroll: (u16, u16),
    oplogs: Vec<op_row::OpRow>,

    router: Router,

    ui: UI,

    indicator: indicator::Indicator,
    signal: Arc<crate::signal::Signal>, // 0 初始状态，1 是停止，2 是停止成功

    boot_at: i64,
    current_at: Metric,
    start_at: Metric,

    query_chart_data: Vec<(f64, f64)>,
    query_count_max: f64,
    query_count_min: f64,
    last_query_count: usize,
    diff_query_count: usize,

    cost_chart_data: Vec<(f64, f64)>,
    cost_max: f64,
    cost_min: f64,
    last_cost: f64,
    diff_cost: f64,

    show_popup: bool,
    popup_input: Input,
    popup_title: String,
    popup_tip: String,

    v: f64,
}

impl App {
    fn new(ui: UI) -> Self {
        let indic = indicator::Indicator::new().init(ind_keys(), ui.target.clone());
        Self {
            oplog_scroll: (0, 0),
            oplogs: vec![],

            router: Router::new(vec![
                Route::new(RouteType::Push, "Stress", "Stress"),
                Route::new(RouteType::Push, "Replay", "Replay"),
                Route::new(RouteType::Quit, "Quit", "Quit"),
            ]),

            ui: ui,

            indicator: indic,
            signal: Arc::new(crate::signal::Signal::new()),

            boot_at: chrono::Local::now().timestamp(), // s
            current_at: Metric::default(),             // s
            start_at: Metric::default(),               // s

            query_count_max: f64::MIN,
            query_count_min: f64::MAX,
            query_chart_data: vec![],
            last_query_count: 0,
            diff_query_count: 0,

            cost_max: f64::MIN,
            cost_min: f64::MAX,
            cost_chart_data: vec![],
            last_cost: 0.,
            diff_cost: 0.,

            show_popup: false,
            popup_input: Input::new("".to_string()),
            popup_title: "Popup Input".to_string(),
            popup_tip: "Press Enter to confirm.".to_string(),

            v: 0.0,
        }
    }

    fn update_current_at(&self) {
        if self.signal.get() == 0 {
            self.current_at
                .set(chrono::Local::now().timestamp() as usize);
        }
    }

    fn update_start_at(&self) {
        self.start_at.set(chrono::Local::now().timestamp() as usize);
    }

    fn reset(&mut self) {
        self.query_chart_data.clear();
        self.query_count_max = f64::MIN;
        self.query_count_min = f64::MAX;

        self.cost_chart_data.clear();
        self.cost_max = f64::MIN;
        self.cost_min = f64::MAX;

        self.signal.set(0);
    }

    fn on_tick(&mut self, tick_index: usize) {
        if self.signal.get() != 0 {
            return;
        }
        {
            let query_count = self.indicator.take("query_count").unwrap().get() as f64;
            if tick_index == 0 {
                let diff_query_count = query_count - self.last_query_count as f64;
                self.last_query_count = query_count as usize;
                self.diff_query_count = diff_query_count as usize;
            }
            let v = self.diff_query_count as f64;

            if !(v.is_infinite() || v.is_nan()) {
                if v > self.query_count_max {
                    self.query_count_max = v;
                }
                if v < self.query_count_min {
                    self.query_count_min = v;
                }
            }

            let v = normalize_to_100(v, self.query_count_min, self.query_count_max);

            self.query_chart_data
                .push((self.query_chart_data.len() as f64, v));

            if self.query_chart_data.len() > 200 {
                self.query_chart_data.remove(0);
                self.query_chart_data
                    .iter_mut()
                    .enumerate()
                    .for_each(|(i, (x, _))| {
                        *x = i as f64;
                    });
            }
        }
        {
            let cost = self.indicator.take("cost_ms").unwrap().get() as f64;
            if tick_index == 0 {
                let diff_cost = cost - self.last_cost;
                self.last_cost = cost;
                self.diff_cost = diff_cost / self.diff_query_count as f64;
            }
            let v = self.diff_cost as f64;
            if !(v.is_infinite() || v.is_nan()) {
                if v > self.cost_max {
                    self.cost_max = v;
                }
                if v < self.cost_min {
                    self.cost_min = v;
                }
            }
            let v = normalize_to_100(v, self.cost_min, self.cost_max);

            self.cost_chart_data
                .push((self.cost_chart_data.len() as f64, v));

            if self.cost_chart_data.len() > 200 {
                self.cost_chart_data.remove(0);
                self.cost_chart_data
                    .iter_mut()
                    .enumerate()
                    .for_each(|(i, (x, _))| {
                        *x = i as f64;
                    });
            }
        }

        // if (dur as u32) % 5 == 0 {
        //     // self.cost_max = f64::MIN;
        //     self.cost_min = f64::MAX;
        //     // self.query_count_max = f64::MIN;
        //     self.query_count_min = f64::MAX;
        // }
    }
}

pub fn boot(ui: UI) -> Result<(), Box<dyn Error>> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, Hide, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let tick_rate = Duration::from_millis(100);
    let app = App::new(ui);

    let res = run_app(&mut terminal, app, tick_rate);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        Show,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();
    let mut tick_index = 0;
    loop {
        terminal.draw(|f| ui(f, &app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout)? {
            let event = event::read()?;

            match app.router.event(&event) {
                EventType::Click(cptab, rtype, keycode) => {
                    // println!("Enter: {}, {:?}", cptab, rtype);
                    match cptab.as_str() {
                        "/Stress" => {
                            app.router.push(
                                vec![
                                    // Route::new(RouteType::Push, "OpLog", "OpLog"),
                                    Route::new(RouteType::Push, "Start", "Start"),
                                    Route::new(RouteType::Pop, "Back", "Back"),
                                ],
                                0,
                            );
                        }
                        "/Replay" => {
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Revert", "Revert"),
                                    Route::new(RouteType::Push, "Start", "Start"),
                                    Route::new(RouteType::Push, "Resume", "Resume"),
                                    Route::new(RouteType::Pop, "Back", "Back"),
                                ],
                                0,
                            );
                        }
                        "/Stress/OpLog" => {
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "ScrollUP", "ScrollUP"),
                                    Route::new(RouteType::Push, "ScrollDown", "ScrollDown"),
                                    Route::new(RouteType::Push, "ScrollLeft", "ScrollLeft"),
                                    Route::new(RouteType::Push, "ScrollRight", "ScrollRight"),
                                    Route::new(RouteType::Pop, "Back", "Back"),
                                ],
                                0,
                            );
                            let target = app.ui.target.clone();

                            let r = Mongobar::new(&target).init();

                            app.oplogs = op_logs::OpLogs::new(
                                r.op_file_oplogs.clone(),
                                op_logs::OpReadMode::FullLine(app.ui.filter.clone()),
                                Vec::new(),
                            )
                            .limit(0, 100)
                            .to_vec();
                        }
                        "/Stress/OpLog/ScrollUP" => {
                            if app.oplog_scroll.0 > 0 {
                                app.oplog_scroll.0 -= 1;
                            }
                        }
                        "/Stress/OpLog/ScrollDown" => {
                            app.oplog_scroll.0 += 1;
                        }
                        "/Stress/OpLog/ScrollLeft" => {
                            if keycode == KeyCode::Left {
                                if app.oplog_scroll.1 > 10 {
                                    app.oplog_scroll.1 -= 10;
                                } else {
                                    app.oplog_scroll.1 = 0;
                                }
                            } else {
                                if app.oplog_scroll.1 > 0 {
                                    app.oplog_scroll.1 -= 1;
                                }
                            }
                        }
                        "/Stress/OpLog/ScrollRight" => {
                            if keycode == KeyCode::Right {
                                app.oplog_scroll.1 += 10;
                            } else {
                                app.oplog_scroll.1 += 1;
                            }
                        }
                        "/Stress/Start" => {
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Boost+", "Boost+"),
                                    Route::new(RouteType::Push, "CCLimit", "CCLimit"),
                                    Route::new(RouteType::Push, "Stop", "Stop")
                                        .with_span(Span::default().fg(Color::Red)),
                                    Route::new(RouteType::Push, "Back", "Back")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );

                            app.update_start_at();
                            app.reset();

                            let target = app.ui.target.clone();
                            let filter = app.ui.filter.clone();
                            let indicator = app.indicator.clone();
                            let inner_indicator = app.indicator.clone();
                            let signal = app.signal.clone();
                            let ui = app.ui.clone();

                            inner_indicator.reset();

                            thread::spawn(move || {
                                let inner_signal = signal.clone();

                                let cur = Instant::now();

                                exec_tokio(move || async move {
                                    let m = Mongobar::new(&target)
                                        .set_signal(signal)
                                        .set_indicator(indicator)
                                        .set_ignore_field(ui.ignore_field.clone())
                                        .merge_config_loop_count(ui.loop_count.clone())
                                        .merge_config_thread_count(ui.thread_count.clone())
                                        .merge_config_rebuild(ui.rebuild.clone())
                                        .merge_config_uri(ui.uri.clone())
                                        .init();
                                    m.op_stress(filter, ui.readonly).await?;

                                    let _ = m.report()?;

                                    Ok(())
                                });

                                inner_signal.set(2);
                                inner_indicator
                                    .take("logs")
                                    .unwrap()
                                    .push("Done".to_string());
                            });
                        }
                        "/Stress/Start/Back" => {
                            if app.signal.get() == 2 {
                                app.router.pop();
                            } else {
                                app.signal.set(1);
                                app.router.pop();
                            }
                        }
                        "/Stress/Start/Stop" => {
                            app.signal.set(1);
                        }
                        "/Stress/Start/Boost+" => {
                            app.show_popup = true;
                            app.popup_title = "Boost Threads".to_string();
                            app.popup_input = Input::new("10".to_string());
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Confirm", "Confirm"),
                                    Route::new(RouteType::Push, "Cancel", "Cancel")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );
                        }
                        "/Stress/Start/Boost+/Confirm" => {
                            let dyn_threads = app.indicator.take("dyn_threads").unwrap();
                            let res_value = app.popup_input.value().parse::<usize>();
                            if let Ok(value) = res_value {
                                dyn_threads.set(dyn_threads.get() + value);
                                app.show_popup = false;
                                app.router.pop();
                            } else {
                                app.popup_tip = "Invalid input.".to_string();
                            }
                        }
                        "/Stress/Start/Boost+/Cancel" => {
                            app.show_popup = false;
                            app.router.pop();
                        }
                        "/Stress/Start/CCLimit" => {
                            app.show_popup = true;
                            app.popup_input = Input::new("1".to_string());
                            app.popup_title = "CCLimit".to_string();
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Confirm", "Confirm"),
                                    Route::new(RouteType::Push, "Cancel", "Cancel")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );
                        }
                        "/Stress/Start/CCLimit/Confirm" => {
                            let dyn_cc_limit = app.indicator.take("dyn_cc_limit").unwrap();
                            let res_value = app.popup_input.value().parse::<usize>();
                            if let Ok(value) = res_value {
                                dyn_cc_limit.set(value);
                                app.show_popup = false;
                                app.router.pop();
                            } else {
                                app.popup_tip = "Invalid input.".to_string();
                            }
                        }
                        "/Stress/Start/CCLimit/Cancel" => {
                            app.show_popup = false;
                            app.router.pop();
                        }
                        "/Replay/Start" => {
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Boost+", "Boost+"),
                                    Route::new(RouteType::Push, "CCLimit", "CCLimit"),
                                    Route::new(RouteType::Push, "Stop", "Stop")
                                        .with_span(Span::default().fg(Color::Red)),
                                    Route::new(RouteType::Push, "Back", "Back")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );

                            app.update_start_at();
                            app.reset();

                            let target = app.ui.target.clone();
                            let filter = app.ui.filter.clone();
                            let indicator = app.indicator.clone();
                            let inner_indicator = app.indicator.clone();
                            let signal = app.signal.clone();
                            let ui = app.ui.clone();

                            inner_indicator.reset();

                            thread::spawn(move || {
                                let inner_signal = signal.clone();

                                exec_tokio(move || async move {
                                    let m = Mongobar::new(&target)
                                        .set_signal(signal)
                                        .set_indicator(indicator)
                                        .set_ignore_field(ui.ignore_field.clone())
                                        .merge_config_loop_count(ui.loop_count.clone())
                                        .merge_config_thread_count(ui.thread_count.clone())
                                        .merge_config_rebuild(ui.rebuild.clone())
                                        .merge_config_uri(ui.uri.clone())
                                        .init();
                                    m.op_replay().await?;
                                    let _ = m.report()?;

                                    Ok(())
                                });

                                inner_signal.set(2);
                                let query_count: usize =
                                    inner_indicator.take("query_count").unwrap().get();
                                let progress: usize =
                                    inner_indicator.take("progress").unwrap().get();
                                inner_indicator
                                    .take("logs")
                                    .unwrap()
                                    .push(format!("Run {}/{} op done.", query_count, progress));
                            });
                        }
                        "/Replay/Start/Back" => {
                            if app.signal.get() == 2 {
                                app.router.pop();
                            } else {
                                app.signal.set(1);
                                app.router.pop();
                            }
                        }
                        "/Replay/Start/Stop" => {
                            app.signal.set(1);
                        }
                        "/Replay/Start/Boost+" => {
                            app.show_popup = true;
                            app.popup_title = "Boost Threads".to_string();
                            app.popup_input = Input::new("10".to_string());
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Confirm", "Confirm"),
                                    Route::new(RouteType::Push, "Cancel", "Cancel")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );
                        }
                        "/Replay/Start/Boost+/Confirm" => {
                            let dyn_threads = app.indicator.take("dyn_threads").unwrap();
                            let res_value = app.popup_input.value().parse::<usize>();
                            if let Ok(value) = res_value {
                                dyn_threads.set(dyn_threads.get() + value);
                                app.show_popup = false;
                                app.router.pop();
                            } else {
                                app.popup_tip = "Invalid input.".to_string();
                            }
                        }
                        "/Replay/Start/Boost+/Cancel" => {
                            app.show_popup = false;
                            app.router.pop();
                        }
                        "/Replay/Start/CCLimit" => {
                            app.show_popup = true;
                            app.popup_input = Input::new("1".to_string());
                            app.popup_title = "CCLimit".to_string();
                            app.router.push(
                                vec![
                                    Route::new(RouteType::Push, "Confirm", "Confirm"),
                                    Route::new(RouteType::Push, "Cancel", "Cancel")
                                        .with_span(Span::default().fg(Color::Red)),
                                ],
                                0,
                            );
                        }
                        "/Replay/Start/CCLimit/Confirm" => {
                            let dyn_cc_limit = app.indicator.take("dyn_cc_limit").unwrap();
                            let res_value = app.popup_input.value().parse::<usize>();
                            if let Ok(value) = res_value {
                                dyn_cc_limit.set(value);
                                app.show_popup = false;
                                app.router.pop();
                            } else {
                                app.popup_tip = "Invalid input.".to_string();
                            }
                        }
                        "/Replay/Start/CCLimit/Cancel" => {
                            app.show_popup = false;
                            app.router.pop();
                        }
                        "/Replay/Revert" => {
                            app.router.push(
                                vec![Route::new(RouteType::Push, "Stop", "Stop")
                                    .with_span(Span::default().fg(Color::Red))],
                                0,
                            );

                            app.update_start_at();
                            app.reset();

                            let target = app.ui.target.clone();
                            let filter = app.ui.filter.clone();
                            let indicator = app.indicator.clone();
                            let inner_indicator = app.indicator.clone();
                            let signal = app.signal.clone();
                            let ui = app.ui.clone();

                            inner_indicator.reset();

                            thread::spawn(move || {
                                let inner_signal = signal.clone();

                                exec_tokio(move || async move {
                                    Mongobar::new(&target)
                                        .set_signal(signal)
                                        .set_indicator(indicator)
                                        .set_ignore_field(ui.ignore_field.clone())
                                        .merge_config_loop_count(ui.loop_count.clone())
                                        .merge_config_thread_count(ui.thread_count.clone())
                                        .merge_config_rebuild(ui.rebuild.clone())
                                        .merge_config_uri(ui.uri.clone())
                                        .init()
                                        .op_run_revert()
                                        .await?;

                                    Ok(())
                                });
                                inner_signal.set(2);
                                let query_count: usize =
                                    inner_indicator.take("query_count").unwrap().get();
                                let progress: usize =
                                    inner_indicator.take("progress").unwrap().get();
                                inner_indicator
                                    .take("logs")
                                    .unwrap()
                                    .push(format!("Run {}/{} op done.", query_count, progress));
                            });
                        }
                        "/Replay/Resume" => {
                            app.router.push(
                                vec![Route::new(RouteType::Push, "Stop", "Stop")
                                    .with_span(Span::default().fg(Color::Red))],
                                0,
                            );

                            app.update_start_at();
                            app.reset();

                            let target = app.ui.target.clone();
                            let filter = app.ui.filter.clone();
                            let indicator = app.indicator.clone();
                            let inner_indicator = app.indicator.clone();
                            let signal = app.signal.clone();
                            let ui = app.ui.clone();

                            inner_indicator.reset();

                            thread::spawn(move || {
                                let inner_signal = signal.clone();

                                exec_tokio(move || async move {
                                    Mongobar::new(&target)
                                        .set_signal(signal)
                                        .set_indicator(indicator)
                                        .set_ignore_field(ui.ignore_field.clone())
                                        .merge_config_loop_count(ui.loop_count.clone())
                                        .merge_config_thread_count(ui.thread_count.clone())
                                        .merge_config_rebuild(ui.rebuild.clone())
                                        .merge_config_uri(ui.uri.clone())
                                        .init()
                                        .op_run_resume()
                                        .await?;

                                    Ok(())
                                });
                                inner_signal.set(2);
                                let query_count: usize =
                                    inner_indicator.take("query_count").unwrap().get();
                                let progress: usize =
                                    inner_indicator.take("progress").unwrap().get();
                                inner_indicator
                                    .take("logs")
                                    .unwrap()
                                    .push(format!("Run {}/{} op done.", query_count, progress));
                            });
                        }
                        "/Replay/Revert/Stop" => {
                            app.signal.set(1);
                            app.router.pop();
                        }
                        "/Replay/Resume/Stop" => {
                            app.signal.set(1);
                            app.router.pop();
                        }
                        "/Quit" => {
                            return Ok(());
                        }
                        _ => {}
                    }

                    if let RouteType::Pop = rtype {
                        app.router.pop();
                    }
                }
                EventType::Quit => {
                    return Ok(());
                }
                EventType::Inner => {}
            }

            app.popup_input.handle_event(&event);
        }
        if last_tick.elapsed() >= tick_rate {
            app.on_tick(tick_index);
            last_tick = Instant::now();
            tick_index = tick_index + 1;
            tick_index = tick_index % 10;
        }
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let area = frame.size();
    let cp = app.router.current_path();

    if cp.starts_with("/Stress/Start") {
        app.update_current_at();
        render_stress_view(frame, area, app);
    } else if cp.starts_with("/Stress/OpLog") {
        render_oplog_view(frame, area, app);
    } else if cp.starts_with("/Stress") {
        render_stress_start_view(frame, area, app);
    } else if cp.starts_with("/Replay/Start") {
        app.update_current_at();
        render_stress_view(frame, area, app);
    } else if cp.starts_with("/Replay/Revert") {
        app.update_current_at();
        render_stress_view(frame, area, app);
    } else if cp.starts_with("/Replay/Resume") {
        app.update_current_at();
        render_stress_view(frame, area, app);
    } else if cp.starts_with("/Replay/OpLog") {
        render_oplog_view(frame, area, app);
    } else if cp.starts_with("/Replay") {
        render_stress_start_view(frame, area, app);
    } else {
        render_main_view(frame, area, app);
    }

    if app.show_popup {
        render_popup(frame, area, app);
    }
}

fn render_oplog_view(frame: &mut Frame, area: Rect, app: &App) {
    let [tab, content] =
        Layout::horizontal([Constraint::Percentage(10), Constraint::Percentage(90)]).areas(area);

    render_tabs(frame, tab, app);
    render_oplogs(frame, content, app);
}

fn render_oplogs(frame: &mut Frame, area: Rect, app: &App) {
    let logs = &app.oplogs;
    let block = Block::new()
        .borders(Borders::ALL)
        .title(format!("OpLogs: {}", logs.len()));
    let paragraph = Paragraph::new(
        logs.iter()
            .map(|v| {
                Line::from(format!(
                    "> id: {}, op: {:?}, ns: {}, ts: {}, cmd:{:?}",
                    v.id, v.op, v.ns, v.ts, v.cmd
                ))
            })
            .collect::<Vec<_>>(),
    )
    .style(Style::default().fg(Color::Gray))
    .block(block)
    .scroll(app.oplog_scroll);
    frame.render_widget(paragraph, area);
}

fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
    // take up a third of the screen vertically and half horizontally
    let popup_area = Rect {
        x: area.width / 3,
        y: area.height / 4,
        width: area.width / 3,
        height: 4,
    };
    Clear.render(popup_area, frame.buffer_mut());

    let bad_popup = Paragraph::new(vec![
        Line::from(format!("Input: [{}]", app.popup_input.value())),
        Line::from(app.popup_tip.as_str()),
    ])
    .wrap(Wrap { trim: true })
    .style(Style::new().yellow().bg(Color::Blue))
    .block(
        Block::new()
            .title(app.popup_title.as_str())
            .title_style(Style::new().white().bold())
            .borders(Borders::ALL)
            .border_style(Style::new().red()),
    );

    frame.render_widget(bad_popup, popup_area);
}

fn render_replay_view(frame: &mut Frame, area: Rect, app: &App) {
    let [tab, content] =
        Layout::horizontal([Constraint::Percentage(10), Constraint::Percentage(90)]).areas(area);

    render_tabs(frame, tab, app);
    render_title(frame, content, app, "will realize soon...");
}

fn render_stress_start_view(frame: &mut Frame, area: Rect, app: &App) {
    let [tab, content] =
        Layout::horizontal([Constraint::Percentage(10), Constraint::Percentage(90)]).areas(area);

    render_tabs(frame, tab, app);
    render_title(
        frame,
        content,
        app,
        &format!(
            "Stress\n\nStatus:[{}]\n\nPress Enter to start...",
            match app.signal.get() {
                0 => "Init",
                1 => "Stop",
                2 => "Stopped",
                _ => "Unknown",
            }
        ),
    );
}

fn render_main_view(frame: &mut Frame, area: Rect, app: &App) {
    let [tab, content] =
        Layout::horizontal([Constraint::Percentage(10), Constraint::Percentage(90)]).areas(area);

    render_tabs(frame, tab, app);
    render_title(
        frame,
        content,
        app,
        &format!(
            "Welcome to Mongobar\n\nCurrent: {}\n\nPress Enter to start...",
            &app.ui.target,
        ),
    );
}

fn render_title(f: &mut Frame, area: Rect, app: &App, title: &str) {
    let block = Block::new()
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::LightGreen));
    f.render_widget(block, area);
    let [_, title_block] =
        Layout::vertical([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(area);
    let title = Paragraph::new(title).alignment(Alignment::Center);

    f.render_widget(title, title_block);
}

fn render_stress_view(frame: &mut Frame, area: Rect, app: &App) {
    let [tab, content] =
        Layout::horizontal([Constraint::Percentage(10), Constraint::Percentage(90)]).areas(area);
    let [chart, progress, log] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Length(3),
        Constraint::Percentage(60),
    ])
    .areas(content);

    render_tabs(frame, tab, app);
    render_chart(frame, chart, app);
    render_progress(frame, progress, app);
    render_log(frame, log, app);
}

fn render_progress(f: &mut Frame, area: Rect, app: &App) {
    let progress = app.indicator.take("progress").unwrap().get();
    let progress_total = app.indicator.take("progress_total").unwrap().get();
    if progress_total == 0 {
        let block = Block::new().borders(Borders::ALL);
        let gauge = Gauge::default()
            .block(block)
            .gauge_style(ratatui::style::Style::default().fg(ratatui::style::Color::Green))
            .label(format!("count: {}", progress))
            .ratio(0.);
        f.render_widget(gauge, area);
    } else {
        let mut current_progress = progress as f64 / progress_total as f64;
        if current_progress.is_nan() {
            current_progress = 0.0;
        }

        if current_progress > 0.999 {
            current_progress = 1.0
        }

        let block = Block::new().borders(Borders::ALL);
        let gauge = Gauge::default()
            .block(block)
            .gauge_style(ratatui::style::Style::default().fg(ratatui::style::Color::Green))
            .label(format!(
                "{:.2}% {}/{}",
                current_progress * 100.0,
                progress,
                progress_total
            ))
            .ratio(current_progress);
        f.render_widget(gauge, area);
    }
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    app.router.render(f, area);
}

fn render_log(f: &mut Frame, area: Rect, app: &App) {
    let logs = app.indicator.take("logs").unwrap();
    let cost_ms = app.indicator.take("cost_ms").unwrap().get();
    let query_count = app.indicator.take("query_count").unwrap().get();
    let querying = app.indicator.take("querying").unwrap().get();
    let thread_count = app.indicator.take("thread_count").unwrap().get();
    let boot_worker = app.indicator.take("boot_worker").unwrap().get();
    let dyn_threads = app.indicator.take("dyn_threads").unwrap().get();
    let dyn_cc_limit = app.indicator.take("dyn_cc_limit").unwrap().get();
    let start_at = app.start_at.get();
    let current_at = app.current_at.get();
    // let query_qps = app.indicator.take("query_qps").unwrap().get();

    let mut text = vec![
        Line::from("> OPStress Bootstrapping"),
        Line::from(format!(
            "> Thread: {}/({}+{}) cc({}<{}) latency({}s)",
            boot_worker,
            thread_count,
            dyn_threads,
            querying,
            dyn_cc_limit,
            current_at - start_at
        )),
        Line::from(format!(
            "> Query : avg_qps({:.2}/s) qps({}/s)",
            (query_count as f64) / (app.current_at.get() - app.start_at.get()) as f64,
            app.diff_query_count,
        )),
        Line::from(format!(
            "> Cost  : avg_dur({:.2}ms) dur({:.2}ms)",
            (cost_ms as f64) / query_count as f64,
            app.diff_cost
        )),
        Line::from(format!(
            "> Query Stats: min({:.2}) max({:.2})",
            app.query_count_min, app.query_count_max,
        )),
        Line::from(format!(
            "> Cost Stats: min({:.2}) max({:.2})",
            app.cost_min, app.cost_max,
        )),
    ];
    logs.logs().iter().for_each(|v| {
        text.push(Line::from(format!("> {}", v.as_str())));
    });
    let block = Block::new().borders(Borders::ALL).title(format!("Console"));
    let paragraph = Paragraph::new(text.clone())
        .style(Style::default().fg(Color::Gray))
        .block(block)
        .scroll(app.oplog_scroll);
    f.render_widget(paragraph, area);
}

fn render_chart(f: &mut Frame, area: Rect, app: &App) {
    // let x_labels = vec![
    //     Span::styled(
    //         format!("{}", app.window[0]),
    //         Style::default().add_modifier(Modifier::BOLD),
    //     ),
    //     Span::raw(format!("{}", (app.window[0] + app.window[1]) / 2.0)),
    //     Span::styled(
    //         format!("{}", app.window[1]),
    //         Style::default().add_modifier(Modifier::BOLD),
    //     ),
    // ];
    let datasets = vec![
        Dataset::default()
            .name("Query")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Cyan))
            .data(&app.query_chart_data),
        Dataset::default()
            .name("Cost")
            .marker(symbols::Marker::Dot)
            .style(Style::default().fg(Color::Yellow))
            .data(&app.cost_chart_data),
    ];

    let chart: Chart = Chart::new(datasets)
        .block(Block::bordered().title(app.router.current_path()))
        .x_axis(
            Axis::default()
                // .title("Progress")
                .style(Style::default().fg(Color::Gray))
                // .labels(x_labels)
                .bounds([0., 200.]),
        )
        .y_axis(
            Axis::default()
                // .title("Query")
                .style(Style::default().fg(Color::Gray))
                // .labels(vec!["-20".bold(), "0".into(), "20".bold()])
                .bounds([0., 100.]),
        );

    f.render_widget(chart, area);
}

fn normalize_to_100(x: f64, min: f64, max: f64) -> f64 {
    ((x - min) / (max - min)) * 100.0
}

#[derive(Debug, Clone, Copy)]
enum RouteType {
    Pop,
    Push,
    Quit,
}

#[derive(Debug, Clone)]
struct Route {
    rtype: RouteType,
    label: String,
    name: String,
    span: Span<'static>,
}

impl Route {
    fn new(rtype: RouteType, name: &str, label: &str) -> Self {
        Self {
            rtype,
            label: label.to_string(),
            span: match rtype {
                RouteType::Push => Span::from(label.to_string()).fg(Color::Blue),
                _ => Span::from(label.to_string()).fg(Color::Red),
            },
            name: name.to_string(),
        }
    }

    fn with_span(mut self, span: Span<'static>) -> Self {
        self.span = span.content(self.label.clone());
        self
    }
}

#[derive(Debug)]
struct Router {
    active_tabs: Vec<Route>,
    active_tab: usize,
    tabs_stack: Vec<(usize, Vec<Route>)>,
}

impl Router {
    fn new(init_tabs: Vec<Route>) -> Self {
        Self {
            active_tabs: init_tabs,
            active_tab: 0,
            tabs_stack: vec![],
        }
    }

    fn current_tab(&self) -> &Route {
        self.active_tabs.get(self.active_tab).unwrap()
    }

    fn current_path(&self) -> String {
        let r: Vec<String> = self
            .tabs_stack
            .iter()
            .map(|v: &(usize, Vec<Route>)| v.1[v.0].name.clone())
            .collect();
        if r.is_empty() {
            "".to_string()
        } else {
            format!("/{}", r.join("/"))
        }
    }

    fn push(&mut self, tabs: Vec<Route>, active: usize) {
        self.tabs_stack
            .push((self.active_tab, self.active_tabs.drain(..).collect()));
        self.active_tabs = tabs;
        self.active_tab = active;
    }

    fn pop(&mut self) {
        if let Some((active_tab, active_tabs)) = self.tabs_stack.pop() {
            self.active_tab = active_tab;
            self.active_tabs = active_tabs;
        }
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::ALL).title("Mongobar");
        let items: Vec<ListItem> = self
            .active_tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.active_tab {
                    ListItem::new(t.span.clone()).bg(Color::DarkGray)
                } else {
                    ListItem::new(t.span.clone())
                }
            })
            .collect();
        let list = List::new(items).block(block);

        f.render_widget(list, area);
    }

    fn event(&mut self, event: &Event) -> EventType {
        if let Event::Key(key) = event {
            if key.code == KeyCode::Char('q') {
                return EventType::Quit;
            }
            if key.code == KeyCode::Up {
                if self.active_tab > 0 {
                    self.active_tab -= 1;
                } else {
                    self.active_tab = self.active_tabs.len() - 1;
                }
            } else if key.code == KeyCode::Down {
                if self.active_tab < self.active_tabs.len() - 1 {
                    self.active_tab += 1;
                } else {
                    self.active_tab = 0;
                }
            } else if key.code == KeyCode::Enter {
                let cp = self.current_path();
                let ctab = self.current_tab();
                let cptab = cp + "/" + ctab.name.as_str();
                return EventType::Click(cptab, ctab.rtype, key.code);
            } else if key.code == KeyCode::Left {
                let cp = self.current_path();
                let ctab = self.current_tab();
                let cptab = cp + "/" + ctab.name.as_str();
                return EventType::Click(cptab, ctab.rtype, key.code);
            } else if key.code == KeyCode::Right {
                let cp = self.current_path();
                let ctab = self.current_tab();
                let cptab = cp + "/" + ctab.name.as_str();
                return EventType::Click(cptab, ctab.rtype, key.code);
            } else if key.code == KeyCode::Esc {
                let cp = self.current_path();
                let ctab = self.current_tab();
                let cptab = cp + "/" + ctab.name.as_str();
                return EventType::Click(cptab, RouteType::Pop, key.code);
            }
        }

        return EventType::Inner;
    }
}

#[derive(Debug, Clone)]
enum EventType {
    Quit,
    Click(String, RouteType, KeyCode),
    Inner,
}
