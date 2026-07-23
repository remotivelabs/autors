use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};
use ratatui::Frame;

use crate::app::{App, PromptKind};
use crate::catalog::CapabilityGroup;

const CYAN: Color = Color::Rgb(53, 214, 211);
const BLUE: Color = Color::Rgb(38, 91, 168);
const PANEL: Color = Color::Rgb(21, 27, 38);
const MUTED: Color = Color::Rgb(135, 145, 160);

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::default().bg(Color::Rgb(10, 14, 22))),
        area,
    );
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, layout[0], app);
    if app.show_bus {
        render_bus(frame, layout[1], app);
    } else if app.show_lin_bus {
        render_lin_bus(frame, layout[1], app);
    } else if app.show_protocol_lab {
        render_protocol_lab(frame, layout[1], app);
    } else if app.show_odx_lab {
        render_odx_lab(frame, layout[1], app);
    } else if app.show_a2l_lab {
        render_a2l_lab(frame, layout[1], app);
    } else if app.show_prm_lab {
        render_prm_lab(frame, layout[1], app);
    } else if app.show_symbol_lab {
        render_symbol_lab(frame, layout[1], app);
    } else if app.document.is_some() {
        render_document(frame, layout[1], app);
    } else {
        render_catalog(frame, layout[1], app);
    }
    render_footer(frame, layout[2], app);
    if app.show_help {
        render_help(frame, area);
    }
    if let Some(prompt) = app.prompt {
        render_prompt(frame, area, app, prompt);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(BLUE))
        .title(Line::from(vec![
            Span::styled(
                " AUTO",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("RS", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
            Span::styled("  Engineering Workbench ", Style::default().fg(MUTED)),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if app.show_bus {
        let title = Line::from(vec![
            Span::styled("◉ ", Style::default().fg(CYAN)),
            Span::styled(
                "CAN / Remaining Bus",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_lin_bus {
        let title = Line::from(vec![
            Span::styled("◉ ", Style::default().fg(CYAN)),
            Span::styled(
                "LIN / LDF Scheduler",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_protocol_lab {
        let title = Line::from(vec![
            Span::styled("◆ ", Style::default().fg(CYAN)),
            Span::styled(
                "Diagnostic & Calibration Protocol Lab",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_odx_lab {
        let title = Line::from(vec![
            Span::styled("◆ ", Style::default().fg(CYAN)),
            Span::styled(
                "ODX Database-Driven Diagnostics",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_a2l_lab {
        let title = Line::from(vec![
            Span::styled("◆ ", Style::default().fg(CYAN)),
            Span::styled(
                "A2L Measurement / Calibration / DAQ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_prm_lab {
        let title = Line::from(vec![
            Span::styled("◆ ", Style::default().fg(CYAN)),
            Span::styled(
                "PRM Flash Procedure Preflight",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if app.show_symbol_lab {
        let title = Line::from(vec![
            Span::styled("◆ ", Style::default().fg(CYAN)),
            Span::styled(
                "A2L Symbol Synchronization",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else if let Some(document) = &app.document {
        let title = Line::from(vec![
            Span::styled("◈ ", Style::default().fg(CYAN)),
            Span::styled(
                document.title(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", document.kind), Style::default().fg(MUTED)),
        ]);
        frame.render_widget(Paragraph::new(title).alignment(Alignment::Right), inner);
    } else {
        let tabs = Tabs::new(CapabilityGroup::ALL.iter().map(|group| group.title()))
            .select(app.group_index)
            .divider(" · ")
            .highlight_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD));
        frame.render_widget(tabs, inner);
    }
}

fn render_catalog(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Percentage(36),
            Constraint::Min(28),
        ])
        .split(area);

    let category_items = CapabilityGroup::ALL.iter().map(|group| {
        let count = app.catalog.group_count(*group);
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("{:<13}", group.title()),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("{count:>2}"), Style::default().fg(MUTED)),
        ]))
    });
    let categories = List::new(category_items)
        .block(panel("Capability areas"))
        .highlight_symbol("▌ ")
        .highlight_style(
            Style::default()
                .fg(CYAN)
                .bg(PANEL)
                .add_modifier(Modifier::BOLD),
        );
    let mut category_state = ListState::default().with_selected(Some(app.group_index));
    frame.render_stateful_widget(categories, columns[0], &mut category_state);

    let visible = app.visible_capability_indices();
    let package_items = visible
        .iter()
        .filter_map(|index| app.catalog.capabilities.get(*index))
        .map(|capability| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    capability.name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    capability.description.clone(),
                    Style::default().fg(MUTED),
                )),
            ])
        });
    let title = if app.query.is_empty() {
        format!("{} crates · {}", app.group().title(), visible.len())
    } else {
        format!("{} matches · /{}", visible.len(), app.query)
    };
    let packages = List::new(package_items)
        .block(panel(title))
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().bg(BLUE).fg(Color::White));
    let mut package_state =
        ListState::default().with_selected((!visible.is_empty()).then_some(app.list_index));
    frame.render_stateful_widget(packages, columns[1], &mut package_state);

    let lines = app
        .selected_capability()
        .map(|capability| capability.detail_lines())
        .unwrap_or_else(|| vec!["No crate matches this view.".to_owned()]);
    let details = Paragraph::new(lines.join("\n"))
        .block(panel("Capability details"))
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });
    frame.render_widget(details, columns[2]);
}

fn render_document(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(document) = &app.document else {
        return;
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(25),
            Constraint::Percentage(52),
            Constraint::Min(28),
        ])
        .split(area);
    let summary = document
        .summary
        .iter()
        .flat_map(|(key, value)| {
            [
                Line::from(Span::styled(key.clone(), Style::default().fg(MUTED))),
                Line::from(Span::styled(
                    value.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::default(),
            ]
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(summary)
            .block(panel("Inspector"))
            .wrap(Wrap { trim: true }),
        columns[0],
    );

    let visible = app.visible_row_indices();
    let rows = visible
        .iter()
        .filter_map(|index| document.rows.get(*index))
        .map(|row| Row::new(row.iter().cloned().map(Cell::from)).height(1));
    let header = Row::new(document.columns.iter().cloned().map(Cell::from))
        .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let widths = table_widths(document.columns.len());
    let title = if app.query.is_empty() {
        format!("Data · {} rows", document.rows.len())
    } else {
        format!("{} matches · /{}", visible.len(), app.query)
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(panel(title))
        .row_highlight_style(
            Style::default()
                .bg(BLUE)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut table_state =
        TableState::default().with_selected((!visible.is_empty()).then_some(app.row_index));
    frame.render_stateful_widget(table, columns[1], &mut table_state);

    let detail = app
        .selected_row()
        .and_then(|index| document.details.get(index))
        .map(|lines| lines.join("\n"))
        .unwrap_or_else(|| "No row selected.".to_owned());
    frame.render_widget(
        Paragraph::new(detail)
            .block(panel("Selection details"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_bus(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(31),
            Constraint::Percentage(43),
            Constraint::Min(36),
        ])
        .split(area);

    let connection = if app.bus.connected {
        "CONNECTED"
    } else {
        "DISCONNECTED"
    };
    let simulation = if app.bus.running { "RUNNING" } else { "PAUSED" };
    let mut status = vec![
        Line::from(Span::styled("Channel", Style::default().fg(MUTED))),
        Line::from(Span::styled(
            format!(
                "{}/CAN{}  {connection}",
                app.bus.adapter_name(),
                app.bus.channel() + 1
            ),
            Style::default()
                .fg(if app.bus.connected {
                    CYAN
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(app.bus.selected_channel_name()),
        Line::from(format!("Driver       {}", app.bus.driver_name())),
        Line::from(format!("Hardware     {}", app.bus.hardware_type())),
        Line::from(format!(
            "Discovered   {} channel(s)",
            app.bus.discovered_channels().len()
        )),
        Line::default(),
        Line::from(Span::styled("Network", Style::default().fg(MUTED))),
        Line::from(app.bus.network_name()),
        Line::default(),
        Line::from(Span::styled("Simulation", Style::default().fg(MUTED))),
        Line::from(Span::styled(
            simulation,
            Style::default()
                .fg(if app.bus.running { Color::Green } else { MUTED })
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!(
            "Elapsed      {:.3} s",
            app.bus.elapsed().as_secs_f64()
        )),
        Line::from(format!("Tx frames    {}", app.bus.tx_frames())),
        Line::from(format!("Rx frames    {}", app.bus.rx_frames())),
        Line::from(format!("Trace rows   {}", app.bus.trace().len())),
        Line::from(format!(
            "Bus rate     {:.0} bit/s",
            app.bus.average_bits_per_second()
        )),
        Line::from(format!(
            "Frame rate   {:.1} frame/s",
            app.bus.average_frames_per_second()
        )),
    ];
    if let Some(error) = &app.bus.last_error {
        status.extend([
            Line::default(),
            Line::from(Span::styled("Last error", Style::default().fg(Color::Red))),
            Line::from(error.clone()),
        ]);
    }
    frame.render_widget(
        Paragraph::new(status)
            .block(panel("CAN channel"))
            .wrap(Wrap { trim: false }),
        columns[0],
    );

    let messages = app.bus.messages();
    let message_rows = messages.iter().map(|message| {
        Row::new([
            if message.enabled { "●" } else { "○" }.to_owned(),
            format!("{:X}", message.id & 0x1fff_ffff),
            message.name.clone(),
            message.node.clone(),
            message
                .period
                .map(|value| format!("{} ms", value.as_millis()))
                .unwrap_or_else(|| "event".to_owned()),
            hex_data(&message.payload),
        ])
    });
    let table = Table::new(
        message_rows,
        [
            Constraint::Length(2),
            Constraint::Length(9),
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Fill(2),
        ],
    )
    .header(
        Row::new(["", "ID", "Message", "Node", "Cycle", "Payload"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!(
        "DBC scheduler · {} messages",
        messages.len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut message_state =
        TableState::default().with_selected((!messages.is_empty()).then_some(app.bus_index));
    frame.render_stateful_widget(table, columns[1], &mut message_state);

    let trace_rows = app.bus.trace().iter().map(|trace| {
        Row::new([
            format!("{:.3}", trace.timestamp.as_secs_f64()),
            if trace.is_tx { "Tx" } else { "Rx" }.to_owned(),
            format!("{:X}", trace.id & 0x1fff_ffff),
            trace.message.clone(),
            format!("{:?}", trace.frame_type),
            hex_data(&trace.data),
        ])
    });
    let latest_signals = app
        .bus
        .trace()
        .back()
        .filter(|trace| !trace.signals.is_empty())
        .map(|trace| format!(" · {}", trace.signals.join(", ")))
        .unwrap_or_default();
    let trace_table = Table::new(
        trace_rows,
        [
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(9),
            Constraint::Fill(2),
            Constraint::Length(9),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new(["Time", "", "ID", "Message", "Type", "Data"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!(
        "Live trace · {}{}",
        app.bus.trace().len(),
        latest_signals
    )))
    .row_highlight_style(Style::default().bg(BLUE));
    let mut trace_state = TableState::default()
        .with_selected((!app.bus.trace().is_empty()).then_some(app.bus.trace().len() - 1));
    frame.render_stateful_widget(trace_table, columns[2], &mut trace_state);
}

fn render_lin_bus(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(31),
            Constraint::Percentage(43),
            Constraint::Min(36),
        ])
        .split(area);

    let connection = if app.lin_bus.connected {
        "CONNECTED"
    } else {
        "DISCONNECTED"
    };
    let simulation = if app.lin_bus.running {
        "RUNNING"
    } else {
        "PAUSED"
    };
    let mut status = vec![
        Line::from(Span::styled("Channel", Style::default().fg(MUTED))),
        Line::from(Span::styled(
            format!(
                "{}/LIN{}  {connection}",
                app.lin_bus.adapter_name(),
                app.lin_bus.channel() + 1
            ),
            Style::default()
                .fg(if app.lin_bus.connected {
                    CYAN
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Driver       {}", app.lin_bus.driver_name())),
        Line::from(format!("Hardware     {}", app.lin_bus.hardware_type())),
        Line::default(),
        Line::from(Span::styled("Network", Style::default().fg(MUTED))),
        Line::from(app.lin_bus.network_name()),
        Line::from(format!("{} bit/s", app.lin_bus.baud_rate())),
        Line::default(),
        Line::from(Span::styled("Schedule", Style::default().fg(MUTED))),
        Line::from(app.lin_bus.active_schedule().unwrap_or("not selected")),
        Line::from(Span::styled(
            simulation,
            Style::default()
                .fg(if app.lin_bus.running {
                    Color::Green
                } else {
                    MUTED
                })
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!(
            "Elapsed      {:.3} s",
            app.lin_bus.elapsed().as_secs_f64()
        )),
        Line::from(format!("Tx/Header    {}", app.lin_bus.tx_frames())),
        Line::from(format!("Rx frames    {}", app.lin_bus.rx_frames())),
        Line::from(format!("Trace rows   {}", app.lin_bus.trace().len())),
        Line::from(format!(
            "Bus rate     {:.0} bit/s",
            app.lin_bus.average_bits_per_second()
        )),
        Line::from(format!(
            "Frame rate   {:.1} frame/s",
            app.lin_bus.average_frames_per_second()
        )),
    ];
    if let Some(error) = &app.lin_bus.last_error {
        status.extend([
            Line::default(),
            Line::from(Span::styled("Last error", Style::default().fg(Color::Red))),
            Line::from(error.clone()),
        ]);
    }
    frame.render_widget(
        Paragraph::new(status)
            .block(panel("LIN channel"))
            .wrap(Wrap { trim: false }),
        columns[0],
    );

    let frames = app.lin_bus.frames();
    let frame_rows = frames.iter().map(|frame| {
        Row::new([
            if frame.simulated { "●" } else { "○" }.to_owned(),
            format!("{:02X}", frame.id),
            frame.name.clone(),
            frame
                .publisher
                .clone()
                .unwrap_or_else(|| "diagnostic".to_owned()),
            if frame.enabled { "enabled" } else { "disabled" }.to_owned(),
            hex_data(&frame.payload),
        ])
    });
    let schedules = app.lin_bus.schedules();
    let schedule_summary = if schedules.is_empty() {
        "no schedules".to_owned()
    } else {
        format!(
            "{} of {} · [/] switch",
            app.lin_schedule_index.min(schedules.len() - 1) + 1,
            schedules.len()
        )
    };
    let table = Table::new(
        frame_rows,
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Fill(2),
        ],
    )
    .header(
        Row::new(["", "ID", "Frame", "Publisher", "State", "Payload"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!("LDF frames · {schedule_summary}")))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut frame_state =
        TableState::default().with_selected((!frames.is_empty()).then_some(app.lin_bus_index));
    frame.render_stateful_widget(table, columns[1], &mut frame_state);

    let trace_rows = app.lin_bus.trace().iter().map(|trace| {
        Row::new([
            format!("{:.3}", trace.timestamp.as_secs_f64()),
            if trace.is_tx { "Tx" } else { "Rx" }.to_owned(),
            format!("{:02X}", trace.id),
            trace.frame.clone(),
            trace.operation.to_owned(),
            hex_data(&trace.data),
        ])
    });
    let latest_signals = app
        .lin_bus
        .trace()
        .back()
        .filter(|trace| !trace.signals.is_empty())
        .map(|trace| format!(" · {}", trace.signals.join(", ")))
        .unwrap_or_default();
    let trace_table = Table::new(
        trace_rows,
        [
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Length(7),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new(["Time", "", "ID", "Frame", "Op", "Data"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!(
        "Live LIN trace · {}{}",
        app.lin_bus.trace().len(),
        latest_signals
    )))
    .row_highlight_style(Style::default().bg(BLUE));
    let mut trace_state = TableState::default()
        .with_selected((!app.lin_bus.trace().is_empty()).then_some(app.lin_bus.trace().len() - 1));
    frame.render_stateful_widget(trace_table, columns[2], &mut trace_state);
}

fn render_protocol_lab(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(32),
            Constraint::Percentage(45),
            Constraint::Min(38),
        ])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Min(8),
        ])
        .split(columns[0]);

    let protocols = crate::protocol::ProtocolKind::ALL
        .iter()
        .enumerate()
        .map(|(index, protocol)| ListItem::new(format!("{}. {}", index + 1, protocol.title())));
    let list = List::new(protocols)
        .block(panel("Protocol codecs"))
        .highlight_symbol("▶ ")
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(BLUE)
                .add_modifier(Modifier::BOLD),
        );
    let mut protocol_state =
        ListState::default().with_selected(Some(app.protocol_lab.protocol_index));
    frame.render_stateful_widget(list, left[0], &mut protocol_state);

    frame.render_widget(
        Paragraph::new(app.protocol_lab.transport_summary(app.bus.connected))
            .block(panel("Live transport"))
            .wrap(Wrap { trim: false }),
        left[1],
    );

    let help = app
        .protocol_lab
        .protocol()
        .help()
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(MUTED))))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(help)
            .block(panel("Input templates"))
            .wrap(Wrap { trim: false }),
        left[2],
    );

    let rows = app.protocol_lab.records().iter().map(|record| {
        Row::new([
            record.protocol.title().to_owned(),
            record.input.clone(),
            record.summary.clone(),
            crate::protocol::hex_data(&record.bytes),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Fill(3),
        ],
    )
    .header(
        Row::new(["Protocol", "Input", "Decoded", "Wire bytes"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!(
        "Codec history · {} records",
        app.protocol_lab.records().len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut history_state = TableState::default().with_selected(
        (!app.protocol_lab.records().is_empty()).then_some(app.protocol_lab.record_index),
    );
    frame.render_stateful_widget(table, columns[1], &mut history_state);

    let record_details = app
        .protocol_lab
        .selected()
        .map(|record| {
            let mut lines = vec![
                format!("{} · {}", record.protocol.title(), record.summary),
                String::new(),
            ];
            lines.extend(record.details.clone());
            lines.join("\n")
        })
        .unwrap_or_else(|| {
            "Press e/Enter for offline codec inspection, or r to send the same command through the configured live transport. CAN protocols share the adapter selected in the CAN workbench.".to_owned()
        });
    let details = if app.protocol_lab.protocol() == crate::protocol::ProtocolKind::DoIp {
        let mut lines = app.protocol_lab.doip_discovery_details();
        lines.push(String::new());
        lines.push(record_details);
        lines.join("\n")
    } else {
        record_details
    };
    frame.render_widget(
        Paragraph::new(details)
            .block(panel("Decoded fields"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_odx_lab(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(29),
            Constraint::Percentage(48),
            Constraint::Min(42),
        ])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(9)])
        .split(columns[0]);

    let session = vec![
        Line::from(format!("Loaded       {}", app.odx_lab.is_loaded())),
        Line::from(format!("Variants     {}", app.odx_lab.variants.len())),
        Line::from(format!("Service rows {}", app.odx_lab.services.len())),
        Line::from(format!("Transport    {}", app.odx_lab.transport.title())),
        Line::from(format!(
            "Source       {}",
            app.odx_lab
                .path
                .as_deref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("open an ODX file")
        )),
    ];
    frame.render_widget(
        Paragraph::new(session)
            .block(panel("Diagnostic session"))
            .wrap(Wrap { trim: false }),
        left[0],
    );

    let variants = app.odx_lab.variants.iter().map(|variant| {
        ListItem::new(format!(
            "{} · {} service(s)",
            variant.name,
            variant.service_end - variant.service_start
        ))
    });
    let variant_list = List::new(variants)
        .block(panel("ECU variants"))
        .highlight_symbol("▶ ")
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(BLUE)
                .add_modifier(Modifier::BOLD),
        );
    let mut variant_state = ListState::default()
        .with_selected((!app.odx_lab.variants.is_empty()).then_some(app.odx_lab.variant_index));
    frame.render_stateful_widget(variant_list, left[1], &mut variant_state);

    let services = app.odx_lab.visible_services();
    let rows = services.iter().map(|service| {
        Row::new([
            service.selector(),
            service.name.clone(),
            service.semantic.clone(),
            hex_data(&service.request_prefix),
            service.request_params.len().to_string(),
            service.response_params.len().to_string(),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(13),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(6),
            Constraint::Length(6),
        ],
    )
    .header(
        Row::new([
            "SID/Sub/ID",
            "Service",
            "Semantic",
            "Request",
            "Req",
            "Resp",
        ])
        .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!("ODX services · {} row(s)", services.len())))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut service_state = TableState::default()
        .with_selected((!services.is_empty()).then_some(app.odx_lab.service_index));
    frame.render_stateful_widget(table, columns[1], &mut service_state);

    let mut details = Vec::new();
    if let Some(variant) = app.odx_lab.selected_variant() {
        details.extend([
            format!("Variant      {} · {}", variant.name, variant.kind),
            format!(
                "Protocols     {}",
                if variant.protocols.is_empty() {
                    "-".to_owned()
                } else {
                    variant.protocols.join(", ")
                }
            ),
            format!("DTCs          {}", variant.dtc_count),
        ]);
        if let Some(can) = &variant.can {
            details.extend([
                format!(
                    "Physical CAN  {:X} → {:X}",
                    can.config.command_id, can.config.response_id
                ),
                format!(
                    "Functional    {}",
                    can.functional_id
                        .map_or_else(|| "-".to_owned(), |id| format!("{id:X}"))
                ),
                format!("Timing        {} · P2 {} ms", can.baudrate, can.p2_ms),
            ]);
        } else {
            details.push("Physical CAN  unresolved in ODX ComParams".to_owned());
        }
        details.push(String::new());
    }
    if let Some(service) = app.odx_lab.selected_service() {
        details.extend([
            format!("{} · {}", service.selector(), service.name),
            format!(
                "Addressing {} · {} positive / {} negative response(s)",
                service.addressing, service.positive_responses, service.negative_responses
            ),
            format!("Fixed request  {}", hex_data(&service.request_prefix)),
            "Dynamic request parameters (enter their raw bytes as a suffix)".to_owned(),
        ]);
        for parameter in &service.request_params {
            details.push(format!(
                "  {} · {:?} · {} bit · offset {}{}",
                parameter.name,
                parameter.data_type,
                parameter.bit_length,
                parameter.byte_offset + parameter.par_value.byte_position,
                if parameter.unit.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", parameter.unit)
                }
            ));
        }
        if service.request_params.is_empty() {
            details.push("  none".to_owned());
        }
        for warning in &service.warnings {
            details.push(format!("Warning: {warning}"));
        }
    } else {
        details.push("Open an ODX file containing an ECU or base variant.".to_owned());
    }
    if !app.odx_lab.last_request.is_empty() || !app.odx_lab.last_response.is_empty() {
        details.extend([
            String::new(),
            format!("Last request   {}", hex_data(&app.odx_lab.last_request)),
            format!("Last response  {}", hex_data(&app.odx_lab.last_response)),
        ]);
        details.extend(app.odx_lab.response_details.clone());
    }
    frame.render_widget(
        Paragraph::new(details.join("\n"))
            .block(panel("Variant / service / physical response"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_a2l_lab(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(29),
            Constraint::Percentage(50),
            Constraint::Min(42),
        ])
        .split(area);
    let state = if app.a2l_lab.running {
        "RUNNING"
    } else {
        "PAUSED"
    };
    let mut summary = vec![
        Line::from(Span::styled("Project", Style::default().fg(MUTED))),
        Line::from(Span::styled(
            app.a2l_lab.project_name.clone(),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Modules       {}", app.a2l_lab.module_count)),
        Line::from(format!("Measurements  {}", app.a2l_lab.measurement_count())),
        Line::from(format!("Calibrations  {}", app.a2l_lab.calibration_count())),
        Line::from(format!("Total objects  {}", app.a2l_lab.object_count())),
        Line::default(),
        Line::from(Span::styled(
            "Virtual ECU / DAQ",
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            state,
            Style::default()
                .fg(if app.a2l_lab.running {
                    Color::Green
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("Sample rate   10 Hz"),
        Line::from(format!(
            "Elapsed       {:.3} s",
            app.a2l_lab.elapsed.as_secs_f64()
        )),
        Line::from(format!("DAQ armed     {}", app.a2l_lab.armed_count())),
        Line::from(format!(
            "Unplaced      {}",
            app.a2l_lab.unplaced_measurements
        )),
        Line::default(),
        Line::from(Span::styled("View", Style::default().fg(MUTED))),
        Line::from(app.a2l_lab.view_mode.title()),
    ];
    if !app.a2l_lab.is_loaded() {
        summary.extend([
            Line::default(),
            Line::from("Open an .a2l file with o."),
            Line::from("The workbench retains it for"),
            Line::from("measurement and calibration."),
        ]);
    }
    if let Some(error) = &app.a2l_lab.last_error {
        summary.extend([
            Line::default(),
            Line::from(Span::styled("Last error", Style::default().fg(Color::Red))),
            Line::from(error.clone()),
        ]);
    }
    frame.render_widget(
        Paragraph::new(summary)
            .block(panel("A2L session"))
            .wrap(Wrap { trim: false }),
        columns[0],
    );

    let visible = app.a2l_lab.visible_indices();
    let rows = visible.iter().filter_map(|index| {
        let object = app.a2l_lab.object_view(*index)?;
        Some(Row::new([
            if object.kind == crate::a2l_lab::A2lObjectKind::Measurement {
                if object.armed {
                    "●"
                } else {
                    "○"
                }
            } else if object.writable {
                "✎"
            } else {
                "·"
            }
            .to_owned(),
            format!("{}::{}", object.module, object.name),
            object.address,
            format!("{} {}", object.data_type, object.shape),
            object.value,
            object.unit,
            object.samples.to_string(),
        ]))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Fill(3),
            Constraint::Length(11),
            Constraint::Length(11),
            Constraint::Fill(2),
            Constraint::Length(8),
            Constraint::Length(7),
        ],
    )
    .header(
        Row::new([
            "", "Object", "Address", "Type", "Physical", "Unit", "Samples",
        ])
        .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel(format!(
        "{} · {} object(s)",
        app.a2l_lab.view_mode.title(),
        visible.len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut table_state =
        TableState::default().with_selected((!visible.is_empty()).then_some(app.a2l_index));
    frame.render_stateful_widget(table, columns[1], &mut table_state);

    let details = visible
        .get(app.a2l_index)
        .map(|index| app.a2l_lab.object_details(*index).join("\n"))
        .unwrap_or_else(|| "No object in the current A2L view.".to_owned());
    frame.render_widget(
        Paragraph::new(details)
            .block(panel("Object / acquisition details"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_prm_lab(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Percentage(48),
            Constraint::Min(40),
        ])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(8)])
        .split(columns[0]);

    let mut session = vec![
        Line::from(format!("Loaded       {}", app.prm_lab.is_loaded())),
        Line::from(format!("Protocol     {}", app.prm_lab.mode())),
        Line::from(format!("Entries      {}", app.prm_lab.entries().len())),
    ];
    if let Some(report) = &app.prm_lab.report {
        session.push(Line::from(format!("Last steps   {}", report.steps.len())));
        session.push(Line::from(format!("Final state  {}", report.final_state)));
    } else {
        session.push(Line::from("Run          not started"));
    }
    frame.render_widget(
        Paragraph::new(session)
            .block(panel("PRM session"))
            .wrap(Wrap { trim: false }),
        left[0],
    );

    let entries = app
        .prm_lab
        .entries()
        .into_iter()
        .map(|entry| ListItem::new(entry.to_owned()));
    let list = List::new(entries)
        .block(panel("Entry command sets"))
        .highlight_symbol("▶ ")
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(BLUE)
                .add_modifier(Modifier::BOLD),
        );
    let mut entry_state = ListState::default()
        .with_selected(app.prm_lab.is_loaded().then_some(app.prm_lab.entry_index));
    frame.render_stateful_widget(list, left[1], &mut entry_state);

    let steps = app.prm_lab.report.as_ref().into_iter().flat_map(|report| {
        report.steps.iter().map(|step| {
            Row::new([
                (step.command_index + 1).to_string(),
                step.scope.clone(),
                step.command_set.clone(),
                step.command.clone(),
                step.state.to_string(),
                step.next.clone().unwrap_or_else(|| "-".to_owned()),
                if step.simulated {
                    "I/O skipped"
                } else {
                    "executed"
                }
                .to_owned(),
            ])
        })
    });
    let table = Table::new(
        steps,
        [
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Fill(2),
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Length(11),
        ],
    )
    .header(
        Row::new(["Step", "Scope", "Set", "Command", "State", "Next", "Mode"])
            .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel("Interpreter execution report"))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut step_state = TableState::default().with_selected(
        app.prm_lab
            .report
            .as_ref()
            .is_some_and(|report| !report.steps.is_empty())
            .then_some(app.prm_lab.step_index),
    );
    frame.render_stateful_widget(table, columns[1], &mut step_state);

    let details = if let Some(error) = &app.prm_lab.last_error {
        format!("Preflight error\n\n{error}")
    } else if let Some(step) = app
        .prm_lab
        .report
        .as_ref()
        .and_then(|report| report.steps.get(app.prm_lab.step_index))
    {
        let mut lines = vec![
            format!(
                "{} / {} / step {}",
                step.scope,
                step.command_set,
                step.command_index + 1
            ),
            format!("Command      {}", step.command),
            format!("State        {}", step.state),
            format!(
                "Next target  {}",
                step.next.as_deref().unwrap_or("sequential")
            ),
            format!(
                "Execution    {}",
                if step.simulated {
                    "transport/wait suppressed"
                } else {
                    "semantic action executed"
                }
            ),
        ];
        if !app.prm_lab.messages.is_empty() {
            lines.push(String::new());
            lines.push("Messages".to_owned());
            lines.extend(app.prm_lab.messages.iter().cloned());
        }
        lines.join("\n")
    } else {
        "Select an entry with Tab and press Space or Enter. Preflight uses the real PRM interpreter and branch state while suppressing bus I/O, waits, flashing, and seed/key DLL calls.".to_owned()
    };
    frame.render_widget(
        Paragraph::new(details)
            .block(panel("Step / message details"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_symbol_lab(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30),
            Constraint::Percentage(55),
            Constraint::Min(40),
        ])
        .split(area);
    let lab = &app.symbol_lab;
    let source = lab
        .source_path
        .as_deref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("-");
    let session = vec![
        Line::from(format!("Source       {source}")),
        Line::from(format!("Parser       {}", lab.source_kind)),
        Line::from(format!("Multiplier   {}", lab.address_multiplier)),
        Line::from(format!("Candidates   {}", lab.candidates.len())),
        Line::from(format!("Selected     {}", lab.selected_count())),
        Line::from(format!(
            "Matched      {}",
            lab.count(autors_symbols::update::UpdateType::Matched)
        )),
        Line::from(format!(
            "Address      {}",
            lab.count(autors_symbols::update::UpdateType::AdjustAddress)
        )),
        Line::from(format!(
            "Size differs {}",
            lab.count(autors_symbols::update::UpdateType::AdjustAddressAndSize)
        )),
        Line::from(format!(
            "Not matched  {}",
            lab.count(autors_symbols::update::UpdateType::NotMatched)
        )),
        Line::default(),
        Line::from(format!(
            "Data section 0x{:X} + 0x{:X}",
            lab.data_section_start, lab.data_section_len
        )),
        Line::from(format!(
            "Size updates {}",
            if lab.allow_size_mismatch {
                "ENABLED"
            } else {
                "blocked"
            }
        )),
        Line::from(format!(
            "Bit masks    {}",
            if lab.preserve_bit_mask {
                "preserved"
            } else {
                "updated"
            }
        )),
    ];
    frame.render_widget(
        Paragraph::new(session)
            .block(panel("Symbol session"))
            .wrap(Wrap { trim: false }),
        columns[0],
    );

    let rows = lab.candidates.iter().map(|candidate| {
        Row::new([
            if candidate.selected {
                "●"
            } else if candidate.can_select(lab.allow_size_mismatch) {
                "○"
            } else {
                "·"
            }
            .to_owned(),
            format!("{}::{}", candidate.module, candidate.object),
            candidate.symbol.clone(),
            candidate
                .current_address
                .map(|address| format!("0x{address:08X}"))
                .unwrap_or_else(|| "unset".to_owned()),
            if candidate.record.address == u64::MAX {
                "-".to_owned()
            } else {
                format!("0x{:08X}", candidate.record.address)
            },
            candidate.record.size.to_string(),
            candidate.status().to_owned(),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(22),
        ],
    )
    .header(
        Row::new([
            "",
            "A2L object",
            "Symbol",
            "Current",
            "Proposed",
            "Size",
            "Status",
        ])
        .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
    )
    .block(panel("Address comparison"))
    .row_highlight_style(
        Style::default()
            .bg(BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");
    let mut state = TableState::default()
        .with_selected((!lab.candidates.is_empty()).then_some(lab.candidate_index));
    frame.render_stateful_widget(table, columns[1], &mut state);

    let details = lab
        .candidates
        .get(lab.candidate_index)
        .map(|candidate| {
            [
                format!("{}::{}", candidate.module, candidate.object),
                format!("Resolved symbol  {}", candidate.symbol),
                format!("Current address  {:?}", candidate.current_address),
                format!("Proposed address 0x{:X}", candidate.record.address),
                format!("Symbol size      {} byte(s)", candidate.record.size),
                format!("Bit mask         0x{:X}", candidate.record.bit_mask),
                format!("Classification   {}", candidate.status()),
                format!("Selected         {}", candidate.selected),
                String::new(),
                "Enter toggles safe address-only updates. Size mismatches remain blocked until z is explicitly enabled. Saving always writes a separate target path and never overwrites the retained source implicitly.".to_owned(),
            ]
            .join("\n")
        })
        .unwrap_or_else(|| {
            "Open an A2L first, then an ELF/AXF or MAP file. Matching honors A2L SYMBOL_LINK, CANAPE_EXT LINK_MAP, record layouts, arrays, and typed DWARF paths.".to_owned()
        });
    frame.render_widget(
        Paragraph::new(details)
            .block(panel("Candidate details"))
            .wrap(Wrap { trim: false }),
        columns[2],
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let shortcuts = if app.show_bus {
        " a Adapter  e Enumerate  ,/. Channel  v Config  c Connect  Space Run  s Send  i Inject  w Save ASC  b Back".to_owned()
    } else if app.show_lin_bus {
        " a Adapter  ,/. Channel  v Config  c Connect  Space Run  [/] Schedule  s Send  i Inject  w Save LTRC  n Back".to_owned()
    } else if app.show_protocol_lab {
        " Tab/Shift-Tab Protocol  1-4 Select  e Offline  c Transport  r Live  d Discover DoIP  [/] Entity  ↑↓ History  x Clear  g Back".to_owned()
    } else if app.show_odx_lab {
        " Tab/Shift-Tab Variant  ↑↓ Service  t CAN/DoIP  c Apply ODX CAN  e Encode  r Live  x Clear  d Back".to_owned()
    } else if app.show_a2l_lab {
        " Tab View  ↑↓ Select  Enter Arm DAQ  Space Run/Pause  e Edit  w Save MDF  x Clear  a Back"
            .to_owned()
    } else if app.show_prm_lab {
        " Tab/Shift-Tab Entry  Space/Enter Preflight  ↑↓ Steps  x Clear report  f Back".to_owned()
    } else if app.show_symbol_lab {
        " ↑↓ Candidate  Enter Select  m Multiplier  z Size policy  p Bit masks  r Refresh  s Write copy  y Back".to_owned()
    } else if app
        .document
        .as_ref()
        .is_some_and(|document| document.is_trace())
    {
        format!(
            " {} {:.3}s {}×  Space Play/Pause  [/] Speed  r Reset  ↑↓ Select  / Search  Esc Back",
            if app.playback.active { "▶" } else { "Ⅱ" },
            app.playback.position,
            app.playback.speed
        )
    } else if app.document.is_some() {
        " ↑↓ Select  / Search  o Open  Esc Back  ? Help  q Quit".to_owned()
    } else {
        " ←→ Area  ↑↓ Select  Enter README  / Search  o Open  F5 Refresh  ? Help  q Quit".to_owned()
    };
    let text = Line::from(vec![
        Span::styled(shortcuts, Style::default().fg(MUTED)),
        Span::styled("  │  ", Style::default().fg(BLUE)),
        Span::styled(app.status.clone(), Style::default().fg(Color::White)),
    ]);
    frame.render_widget(Paragraph::new(text).style(Style::default().bg(PANEL)), area);
}

fn render_prompt(frame: &mut Frame<'_>, area: Rect, app: &App, prompt: PromptKind) {
    let popup = centered_rect(72, 5, area);
    frame.render_widget(Clear, popup);
    let (title, hint) = match prompt {
        PromptKind::Open => (
            "Open engineering file",
            "absolute path or path relative to workspace",
        ),
        PromptKind::Search => ("Filter current view", "matches every visible column"),
        PromptKind::SendCan => (
            "Transmit CAN frame",
            "hex ID followed by bytes, e.g. 123 01 02 FF; suffix ID with x for extended",
        ),
        PromptKind::InjectCan => (
            "Inject received CAN frame",
            "hex ID followed by bytes, e.g. 123 01 02 FF; suffix ID with x for extended",
        ),
        PromptKind::ConfigureCan => (
            "Configure CAN adapter",
            "zero-based channel and optional decimal/0x hardware type, e.g. 0 0x51",
        ),
        PromptKind::SetCanPayload => (
            "Set scheduled payload",
            "hexadecimal bytes only, e.g. 01 02 FF; empty input produces a zero-length frame",
        ),
        PromptKind::SetCanPeriod => (
            "Set scheduled cycle",
            "period in milliseconds, or 'event' to make the message one-shot only",
        ),
        PromptKind::SaveCanTrace => (
            "Save live CAN trace",
            "target .asc path; exports every captured classic CAN/CAN FD request and response",
        ),
        PromptKind::SendLin => (
            "Transmit LIN frame",
            "hex ID (00-3F) followed by up to eight bytes, e.g. 22 01 02 FF",
        ),
        PromptKind::InjectLin => (
            "Inject received LIN frame",
            "hex ID (00-3F) followed by up to eight bytes, e.g. 22 01 02 FF",
        ),
        PromptKind::ConfigureLin => (
            "Configure LIN adapter",
            "zero-based channel and optional decimal/0x hardware type, e.g. 0 3",
        ),
        PromptKind::SetLinPayload => (
            "Set scheduled LIN payload",
            "exactly the LDF frame length in hexadecimal bytes",
        ),
        PromptKind::SaveLinTrace => (
            "Save live LIN trace",
            "target .ltrc path; full frames and subscriber header requests are preserved",
        ),
        PromptKind::SetA2lValue => (
            "Set A2L physical value",
            "finite numeric value within the object's A2L limits",
        ),
        PromptKind::SaveA2lMdf => (
            "Save A2L DAQ recording",
            "target .mdf path; exports retained physical samples as MDF 3.30 channels",
        ),
        PromptKind::ProtocolCommand => (
            "Encode or decode protocol data",
            "use a template shown at left, or enter raw hexadecimal bytes",
        ),
        PromptKind::ConfigureProtocolTransport => (
            "Configure live protocol transport",
            "CAN: command-ID response-ID classic|fd; DoIP: remote[:port] local-IP source target [P2-ms]",
        ),
        PromptKind::LiveProtocolCommand => (
            "Execute live protocol request",
            "use the selected protocol template or enter raw request bytes; requires CAN connection or DoIP route",
        ),
        PromptKind::OdxServicePayload => (
            "Encode selected ODX service",
            "optional raw hexadecimal bytes appended after the ODX SID/subfunction/identifier prefix",
        ),
        PromptKind::LiveOdxServicePayload => (
            "Execute selected ODX service",
            "optional raw request-parameter suffix; uses the ODX workbench's CAN or DoIP transport",
        ),
        PromptKind::ConfigureSymbolMultiplier => (
            "Set symbol address multiplier",
            "positive decimal or 0x value; candidates are recomputed immediately",
        ),
        PromptKind::SaveSymbolUpdates => (
            "Write synchronized A2L copy",
            "target .a2l path; selected updates are applied to a clone before the file is written",
        ),
    };
    let block = panel(title).border_style(Style::default().fg(CYAN));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("> ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
                Span::raw(app.input.clone()),
                Span::styled("█", Style::default().fg(CYAN)),
            ]),
            Line::from(Span::styled(hint, Style::default().fg(MUTED))),
        ]),
        inner,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(72, 32, area);
    frame.render_widget(Clear, popup);
    let text = [
        "Navigation",
        "  ←/→ or h/l      switch capability area",
        "  ↑/↓ or j/k      move selection",
        "  Home/End        first/last row",
        "  PageUp/PageDown move ten rows",
        "  Space           play/pause ASC, BLF or LTRC",
        "  [ / ]           halve/double playback speed",
        "  r               reset trace playback",
        "  b / n           open CAN / LIN workbench",
        "  g               open live/offline UDS/DoIP/CCP/XCP protocol lab",
        "  e / c / r       protocol codec / transport setup / live request",
        "  d               open retained ODX database-driven diagnostic lab",
        "  Tab/e/r/t       ODX variant / encode / live request / transport",
        "  a               open retained A2L measurement/calibration/DAQ lab",
        "  f               open retained PRM flash procedure preflight lab",
        "  y               open ELF/MAP to A2L symbol synchronization lab",
        "  Tab/Enter/e     A2L view / DAQ arm / edit physical value",
        "  w               save A2L DAQ physical samples as MDF 3.30",
        "  a               cycle compiled Virtual/Vector/Kvaser/PEAK adapter",
        "  e               enumerate CAN channels (when backend supports it)",
        "  , / .           select previous/next bus channel",
        "  v               edit channel and vendor hardware-type number",
        "  c               connect/disconnect selected hardware channel",
        "  Enter / t       enable schedule row / trigger once",
        "  s / i           transmit / inject received CAN frame",
        "  w               save current CAN/LIN live trace as ASC/LTRC",
        "  p / m           edit scheduled payload / cycle time",
        "  [ / ]           switch active LIN schedule table",
        "",
        "Actions",
        "  Enter           open selected crate README",
        "  o               open A2L, DBC, ASC or any file",
        "  /               filter the current table",
        "  F5              refresh Cargo workspace metadata",
        "  Esc             clear filter or close document",
        "  q / Ctrl-C      quit",
        "",
        "Press any key to close help.",
    ]
    .join("\n");
    frame.render_widget(
        Paragraph::new(text)
            .block(panel("Keyboard reference").border_style(Style::default().fg(CYAN)))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn panel<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BLUE))
        .title(title)
        .style(Style::default().bg(PANEL).fg(Color::White))
}

fn table_widths(count: usize) -> Vec<Constraint> {
    if count == 0 {
        return vec![Constraint::Min(1)];
    }
    (0..count)
        .map(|index| {
            if index + 1 == count {
                Constraint::Fill(2)
            } else {
                Constraint::Fill(1)
            }
        })
        .collect()
}

fn hex_data(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use autors_a2l::Project;
    use autors_ldf::model::Ldf;
    use autors_prm::prm::{CmdSet, Mode, PrmCommand, PrmFile, PrmValue};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::catalog::{Capability, CapabilityCatalog, CapabilityGroup};

    use super::*;

    #[test]
    fn catalog_renders_brand_crate_and_details() {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: vec![Capability {
                name: "autors-dbc".to_owned(),
                description: "Editable CAN databases".to_owned(),
                version: "0.1.0".to_owned(),
                group: CapabilityGroup::Network,
                features: Vec::new(),
                dependencies: Vec::new(),
                manifest_path: PathBuf::from("dbc/Cargo.toml"),
            }],
        });
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("AUTORS"));
        assert!(rendered.contains("autors-dbc"));
        assert!(rendered.contains("Editable CAN databases"));
    }

    #[test]
    fn virtual_can_workbench_renders_connection_scheduler_and_trace_panels() {
        let backend = TestBackend::new(150, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.bus.connect().unwrap();
        app.bus.send_text("123 01 02").unwrap();
        app.show_bus = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("CAN / Remaining Bus"));
        assert!(rendered.contains("Virtual/CAN1"));
        assert!(rendered.contains("in-process"));
        assert!(rendered.contains("CONNECTED"));
        assert!(rendered.contains("DBC scheduler"));
        assert!(rendered.contains("Live trace"));
    }

    #[test]
    fn virtual_lin_workbench_renders_ldf_scheduler_and_symbolic_trace() {
        let database = Ldf::parse_str(
            r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;
Channel_name = "BodyLIN";
Nodes { Master: Master, 5 ms, 0.1 ms; Slaves: Slave; }
Signals { CommandValue: 8, 7, Master, Slave; }
Frames { Command: 3, Master, 1 { CommandValue, 0; } }
Node_attributes {
    Slave {
        LIN_protocol = "2.2";
        configured_NAD = 1;
        product_id = 1, 2, 3;
    }
}
Schedule_tables { Main { Command delay 10 ms; } }
"#,
        )
        .unwrap();
        let backend = TestBackend::new(150, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.lin_bus.attach_database(&database).unwrap();
        app.lin_bus.connect().unwrap();
        app.lin_bus.send_text("03 2A").unwrap();
        app.show_lin_bus = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("LIN / LDF Scheduler"));
        assert!(rendered.contains("Virtual/LIN1"));
        assert!(rendered.contains("BodyLIN"));
        assert!(rendered.contains("LDF frames"));
        assert!(rendered.contains("Command"));
        assert!(rendered.contains("Live LIN trace"));
    }

    #[test]
    fn protocol_lab_renders_templates_wire_bytes_and_decoded_fields() {
        let backend = TestBackend::new(150, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.protocol_lab.submit("read F190").unwrap();
        app.show_protocol_lab = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Diagnostic & Calibration Protocol Lab"));
        assert!(rendered.contains("UDS / KWP"));
        assert!(rendered.contains("ReadDataByIdentifier"));
        assert!(rendered.contains("22 F1 90"));
        assert!(rendered.contains("Decoded fields"));
    }

    #[test]
    fn odx_lab_renders_database_driven_service_surface() {
        let backend = TestBackend::new(150, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.show_odx_lab = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("ODX Database-Driven Diagnostics"));
        assert!(rendered.contains("ECU variants"));
        assert!(rendered.contains("ODX services"));
        assert!(rendered.contains("CAN/DoIP"));
    }

    #[test]
    fn a2l_lab_renders_objects_virtual_values_and_daq_state() {
        let project = Project::parse_str(
            r#"
/begin PROJECT Demo "demo"
 /begin MODULE ECU "ecu"
  /begin RECORD_LAYOUT RL FNC_VALUES 1 UWORD ROW_DIR DIRECT /end RECORD_LAYOUT
  /begin MEASUREMENT Speed "speed" UWORD NO_COMPU_METHOD 1 0 0 8000 ECU_ADDRESS 0x1000 READ_WRITE /end MEASUREMENT
  /begin CHARACTERISTIC Gain "gain" VALUE 0x2000 RL 0 NO_COMPU_METHOD 0 100 /end CHARACTERISTIC
 /end MODULE
/end PROJECT
"#,
        )
        .unwrap();
        let backend = TestBackend::new(160, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.a2l_lab.attach_project(&project).unwrap();
        app.a2l_lab.set_running(true).unwrap();
        app.a2l_lab.advance(std::time::Duration::from_millis(100));
        app.show_a2l_lab = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("A2L Measurement / Calibration / DAQ"));
        assert!(rendered.contains("ECU::Speed"));
        assert!(rendered.contains("RUNNING"));
        assert!(rendered.contains("Samples"));
        assert!(rendered.contains("Recent physical samples"));
    }

    #[test]
    fn prm_lab_renders_interpreter_steps_and_suppressed_io() {
        let mut project = PrmFile {
            mode: Mode::Uds,
            ..PrmFile::default()
        };
        project.cmdsets.insert(
            "FLASH".to_owned(),
            CmdSet {
                name: "FLASH".to_owned(),
                commands: vec![PrmCommand {
                    name: "UDS_DIAGNOSTIC_SESSION_CONTROL".to_owned(),
                    args: vec![PrmValue::UInt(2)],
                    ..PrmCommand::default()
                }],
            },
        );
        let backend = TestBackend::new(160, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.prm_lab.attach_project(&project);
        app.prm_lab.run_dry_run().unwrap();
        app.show_prm_lab = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("PRM Flash Procedure Preflight"));
        assert!(rendered.contains("UDS_DIAGNOSTIC_SESSION_CONTROL"));
        assert!(rendered.contains("I/O skipped"));
        assert!(rendered.contains("Interpreter execution report"));
    }

    #[test]
    fn symbol_lab_renders_a2l_address_comparison_and_write_policy() {
        let project = Project::parse_str(
            r#"
/begin PROJECT Demo "demo"
 /begin MODULE ECU "ecu"
  /begin MEASUREMENT Speed "speed" UWORD NO_COMPU_METHOD 1 0 0 8000 ECU_ADDRESS 0x1000 /end MEASUREMENT
 /end MODULE
/end PROJECT
"#,
        )
        .unwrap();
        let directory =
            std::env::temp_dir().join(format!("autors-cli-symbol-ui-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let map = directory.join("firmware.map");
        std::fs::write(&map, "00002000 Speed\n").unwrap();
        let backend = TestBackend::new(170, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(CapabilityCatalog {
            workspace_root: PathBuf::from("workspace"),
            capabilities: Vec::new(),
        });
        app.symbol_lab.attach_source(&map, &project).unwrap();
        app.show_symbol_lab = true;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("A2L Symbol Synchronization"));
        assert!(rendered.contains("ECU::Speed"));
        assert!(rendered.contains("0x00001000"));
        assert!(rendered.contains("0x00002000"));
        assert!(rendered.contains("address update"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut output = String::new();
        for y in buffer.area.top()..buffer.area.bottom() {
            for x in buffer.area.left()..buffer.area.right() {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }
}
