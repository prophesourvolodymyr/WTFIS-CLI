use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color as TuiColor, Modifier, Style},
    text::{Line, Span},
    widgets::{Borders, List, ListItem, Paragraph},
};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

const MAX_RECENTS: usize = 5;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    roots: Option<Vec<PathBuf>>,
    scan_hidden: Option<bool>,
    exact_depth: Option<usize>,
    recent: Option<Vec<PathBuf>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!(
            "wtfis - find projects fast\n\nUsage:\n  wtfis [QUERY]\n  cdd [QUERY]\n  wtfis --set"
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--set") {
        return settings();
    }
    let query = args.join(" ");
    let mut config = load_config();
    let roots = config.roots.clone().unwrap_or_else(default_roots);
    let recent = config.recent.clone().unwrap_or_default();
    let projects = if query.is_empty() {
        None
    } else {
        Some(scan(
            &roots,
            config.scan_hidden.unwrap_or(false),
            config.exact_depth,
        ))
    };
    if let Some(projects) = &projects {
        let exact: Vec<_> = projects
            .iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(&query))
            })
            .collect();
        if exact.len() == 1 {
            let path = exact[0].clone();
            remember(&mut config, path.clone())?;
            emit_path(&path)?;
            return Ok(());
        }
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        if query.is_empty() {
            return Ok(());
        }
        return print_best(projects.as_deref().unwrap_or_default(), &query);
    }
    let selected = picker(
        &roots,
        config.scan_hidden.unwrap_or(false),
        config.exact_depth,
        &recent,
        &query,
        projects,
    )?;
    if let Some(path) = selected {
        remember(&mut config, path.clone())?;
        emit_path(&path)?;
    }
    Ok(())
}

fn default_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    // V1 searches the complete home directory by default. Users can narrow
    // this with `wtfis --set`; this avoids silently missing projects in an
    // uncommon folder while keeping the scan away from system directories.
    vec![home]
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("wtfis/config.toml"))
}
fn load_config() -> Config {
    config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn scan(roots: &[PathBuf], scan_hidden: bool, exact_depth: Option<usize>) -> Vec<PathBuf> {
    if let Some(depth) = exact_depth {
        return scan_exact_depth(roots, scan_hidden, depth);
    }
    let mut paths = Vec::new();
    for root in roots {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .max_depth(exact_depth.unwrap_or(usize::MAX))
            .into_iter();
        for entry in walker
            .filter_entry(|entry| !ignored_directory(entry.path(), scan_hidden))
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_dir()
                || entry.depth() == 0
                || exact_depth.is_some_and(|depth| entry.depth() != depth)
            {
                continue;
            }
            let path = entry.path();
            if ignored_directory(path, scan_hidden) {
                continue;
            }
            paths.push(path.to_path_buf());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn scan_exact_depth(roots: &[PathBuf], scan_hidden: bool, depth: usize) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in roots {
        let first_level = directory_children(root, scan_hidden);
        if depth == 1 {
            paths.extend(first_level);
            continue;
        }
        for group in first_level {
            if depth == 2 {
                paths.extend(directory_children(&group, scan_hidden));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn directory_children(path: &Path, scan_hidden: bool) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let child = entry.path();
            if entry.file_type().ok()?.is_dir() && !ignored_directory(&child, scan_hidden) {
                Some(child)
            } else {
                None
            }
        })
        .collect()
}

fn ignored_directory(path: &Path, scan_hidden: bool) -> bool {
    if !scan_hidden
        && path
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
    {
        return true;
    }
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_string_lossy().as_ref(),
            "node_modules" | "target" | "build" | "dist" | "vendor" | ".git"
        )
    })
}

fn rank(paths: &[PathBuf], query: &str) -> Vec<(PathBuf, i64)> {
    let query = query.to_lowercase();
    let mut results: Vec<_> = paths
        .iter()
        .filter_map(|path| fuzzy_score(path, &query).map(|score| (path.clone(), score)))
        .collect();
    results.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    results
}

fn fuzzy_score(path: &Path, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let text = path.to_string_lossy().to_lowercase();
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name == query {
        return Some(10_000);
    }
    if name.starts_with(query) {
        return Some(8_000 - name.len() as i64);
    }
    if name.contains(query) {
        return Some(6_000 - name.len() as i64);
    }
    let mut score = 0;
    let mut cursor = 0;
    let chars: Vec<_> = text.chars().collect();
    for wanted in query.chars() {
        let Some(pos) = chars[cursor..].iter().position(|c| *c == wanted) else {
            return None;
        };
        let actual = cursor + pos;
        score += if actual == 0
            || chars[actual - 1].is_whitespace()
            || chars[actual - 1] == '/'
            || chars[actual - 1] == '-'
            || chars[actual - 1] == '_'
        {
            20
        } else {
            5
        };
        cursor = actual + 1;
    }
    Some(score - (text.len() as i64 / 10))
}

fn picker(
    roots: &[PathBuf],
    scan_hidden: bool,
    exact_depth: Option<usize>,
    recent: &[PathBuf],
    initial: &str,
    prepared: Option<Vec<PathBuf>>,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    terminal::enable_raw_mode()?;
    let height = terminal::size()
        .map(|(_, rows)| rows.clamp(12, 20))
        .unwrap_or(16);
    let backend = CrosstermBackend::new(io::stdout());
    let mut ui_terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )?;
    execute!(ui_terminal.backend_mut(), EnableMouseCapture)?;
    let mut query = initial.to_string();
    let mut selected = 0usize;
    let mut paths = prepared;
    let mut scan_receiver: Option<Receiver<Vec<PathBuf>>> = None;
    let mut scanning = false;
    let mut last_click: Option<(usize, Instant)> = None;
    let result = loop {
        if !query.is_empty() && paths.is_none() && scan_receiver.is_none() {
            let roots = roots.to_vec();
            scan_receiver = Some(start_scan(roots, scan_hidden, exact_depth));
            scanning = true;
        }
        if let Some(receiver) = &scan_receiver {
            if let Ok(found_paths) = receiver.try_recv() {
                paths = Some(found_paths);
                scan_receiver = None;
                scanning = false;
            }
        }
        let recent_results: Vec<_> = recent
            .iter()
            .take(MAX_RECENTS)
            .cloned()
            .map(|path| (path, 0))
            .collect();
        let results = if query.is_empty() {
            recent_results
        } else {
            rank(paths.as_deref().unwrap_or_default(), &query)
        };
        selected = selected.min(results.len().saturating_sub(1));
        ui_terminal.draw(|frame| render_frame(frame, &query, &results, selected, scanning))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => break None,
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => break results.get(selected).map(|item| item.0.clone()),
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            }) => selected = selected.saturating_sub(1),
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            }) => selected = (selected + 1).min(results.len().saturating_sub(1)),
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            }) => {
                query.pop();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            }) if modifiers.contains(KeyModifiers::CONTROL) => break None,
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                query.push(c);
            }
            Event::Mouse(mouse)
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
            {
                let viewport = ui_terminal.get_frame().area();
                let visible_count = viewport.height.saturating_sub(8).max(1) as usize;
                let start = if results.len() <= visible_count {
                    0
                } else {
                    selected
                        .saturating_sub(visible_count - 1)
                        .min(results.len() - visible_count)
                };
                let first_result_row = viewport.y as usize + 4;
                let clicked_row = mouse.row as usize;
                if clicked_row >= first_result_row
                    && clicked_row < first_result_row + results.len().min(visible_count)
                {
                    let clicked = start + clicked_row - first_result_row;
                    if last_click.is_some_and(|(previous, time)| {
                        previous == clicked && time.elapsed() < Duration::from_millis(500)
                    }) {
                        break results.get(clicked).map(|item| item.0.clone());
                    }
                    selected = clicked;
                    last_click = Some((clicked, Instant::now()));
                }
            }
            _ => {}
        }
    };
    terminal::disable_raw_mode()?;
    execute!(ui_terminal.backend_mut(), DisableMouseCapture)?;
    ui_terminal.clear()?;
    ui_terminal.show_cursor()?;
    Ok(result)
}

fn start_scan(
    roots: Vec<PathBuf>,
    scan_hidden: bool,
    exact_depth: Option<usize>,
) -> Receiver<Vec<PathBuf>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(scan(&roots, scan_hidden, exact_depth));
    });
    receiver
}

fn remember(config: &mut Config, path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let recent = config.recent.get_or_insert_with(Vec::new);
    recent.retain(|item| item != &path);
    recent.insert(0, path);
    recent.truncate(5);
    let path = config_path().ok_or("cannot determine config directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

fn emit_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(output_path) = env::var("WTFIS_OUTPUT") {
        fs::write(output_path, format!("{}\n", path.display()))?;
    } else {
        println!("{}", path.display());
    }
    Ok(())
}

fn render_frame(
    frame: &mut ratatui::Frame<'_>,
    query: &str,
    results: &[(PathBuf, i64)],
    selected: usize,
    scanning: bool,
) {
    let block = ratatui::widgets::Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(TuiColor::Cyan))
        .title(" ◆  W T F I S ")
        .title_bottom(" where the fuck is your project? ");
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let label = if query.is_empty() {
        "Recent projects  ["
    } else {
        "Search projects  ["
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(label, Style::default().fg(TuiColor::White)),
            Span::styled(query, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("|]", Style::default().fg(TuiColor::Cyan)),
        ])),
        sections[0],
    );
    frame.render_widget(
        Paragraph::new("  ↑/↓ navigate  Enter open  Esc cancel")
            .style(Style::default().fg(TuiColor::DarkGray)),
        sections[1],
    );
    let visible_count = sections[3].height.max(1) as usize;
    let start = if results.len() <= visible_count {
        0
    } else {
        selected
            .saturating_sub(visible_count - 1)
            .min(results.len() - visible_count)
    };
    let end = (start + visible_count).min(results.len());
    let items: Vec<ListItem> = results[start..end]
        .iter()
        .enumerate()
        .map(|(index, (path, _))| {
            let actual = start + index;
            let row = format!(
                "{} ▸ {}  {}",
                if actual == selected { "›" } else { " " },
                path.file_name().unwrap_or_default().to_string_lossy(),
                path.display()
            );
            let style = if actual == selected {
                Style::default()
                    .fg(TuiColor::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TuiColor::DarkGray)
            };
            ListItem::new(Line::from(Span::styled(row, style)))
        })
        .collect();
    if items.is_empty() {
        let message = if scanning {
            "  ◌  Scanning folders..."
        } else if query.is_empty() {
            "  ◇  Type to search folders"
        } else {
            "  ×  No matching folders"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(TuiColor::DarkGray)),
            sections[3],
        );
    } else {
        frame.render_widget(List::new(items), sections[3]);
    }
    let navigation = if results.is_empty() {
        "↑/↓ navigate  Enter open  Esc cancel".to_string()
    } else {
        format!(
            "{}-{} of {}  ↑/↓ navigate  Enter open  Esc cancel",
            start + 1,
            end,
            results.len()
        )
    };
    frame.render_widget(
        Paragraph::new(navigation).style(Style::default().fg(TuiColor::DarkGray)),
        sections[4],
    );
}

fn render(
    out: &mut impl Write,
    query: &str,
    results: &[(PathBuf, i64)],
    selected: usize,
    rendered_lines: usize,
    scanning: bool,
) -> io::Result<usize> {
    let _ = rendered_lines;
    let terminal_width = match terminal::size() {
        Ok((columns, _)) if columns >= 45 => columns as usize,
        _ => 72,
    };
    let panel_width = terminal_width.min(78);
    let inner_width = panel_width.saturating_sub(6).max(32);
    let terminal_height = terminal::size()
        .map(|(_, rows)| rows as usize)
        .unwrap_or(24);
    let visible_count = terminal_height.saturating_sub(11).max(1);
    let start = if results.len() <= visible_count {
        0
    } else {
        selected
            .saturating_sub(visible_count - 1)
            .min(results.len() - visible_count)
    };
    let end = (start + visible_count).min(results.len());
    let search_label = if query.is_empty() {
        "Recent projects  ["
    } else {
        "Search projects  ["
    };
    let query_width = inner_width.saturating_sub(search_label.chars().count() + 2);
    let displayed_query = truncate_line(query, query_width);
    let search_padding = inner_width
        .saturating_sub(search_label.chars().count() + displayed_query.chars().count() + 2);
    queue!(
        out,
        cursor::RestorePosition,
        cursor::MoveToColumn(0),
        Clear(ClearType::FromCursorDown),
        Print(box_border(inner_width, '-')),
        Print("\n"),
        SetForegroundColor(Color::Cyan),
        Print(box_text("  ◆  W T F I S", inner_width)),
        ResetColor,
        Print("\n"),
        SetForegroundColor(Color::DarkGrey),
        Print(box_text(
            "     where the fuck is your project?",
            inner_width
        )),
        ResetColor,
        Print("\n"),
        SetForegroundColor(Color::Cyan),
        Print("  | "),
        ResetColor,
        Print(search_label),
        SetAttribute(Attribute::Bold),
        Print(displayed_query),
        SetForegroundColor(Color::Cyan),
        Print("|"),
        ResetColor,
        Print("]"),
        Print(&" ".repeat(search_padding)),
        SetForegroundColor(Color::Cyan),
        Print(" |\n"),
        ResetColor,
        Print(box_border(inner_width, '-')),
        Print("\n")
    )?;
    for (index, (path, _)) in results[start..end].iter().enumerate() {
        let actual_index = start + index;
        let name = fit_name(path.file_name().unwrap_or_default().to_string_lossy());
        let path_text = fit_path(path, name.chars().count());
        let row = format!("▸ {}  {}", name, path_text);
        queue!(
            out,
            SetForegroundColor(if actual_index == selected {
                Color::White
            } else {
                Color::DarkGrey
            }),
            SetAttribute(if actual_index == selected {
                Attribute::Bold
            } else {
                Attribute::Reset
            }),
            Print(box_text_with_marker(
                &row,
                inner_width,
                actual_index == selected,
            )),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n")
        )?;
    }
    if results.is_empty() {
        queue!(
            out,
            SetForegroundColor(Color::DarkGrey),
            Print(box_text(
                if scanning {
                    "  ◌  Scanning folders..."
                } else if query.is_empty() {
                    "  ◇  Type to search folders"
                } else {
                    "  ×  No matching folders"
                },
                inner_width
            )),
            ResetColor,
            Print("\n")
        )?;
    }
    let navigation = if results.is_empty() {
        "  ↑/↓ navigate  Enter open  Esc cancel".to_string()
    } else {
        format!(
            "  {}-{} of {}  ↑/↓ navigate  Enter open  Esc cancel",
            start + 1,
            end,
            results.len()
        )
    };
    queue!(
        out,
        SetForegroundColor(Color::DarkGrey),
        Print(box_text(&navigation, inner_width)),
        ResetColor,
        Print("\n"),
        SetForegroundColor(Color::Cyan),
        Print(box_border(inner_width, '-')),
        ResetColor
    )?;
    out.flush()?;
    Ok(end.saturating_sub(start) + 6 + usize::from(results.is_empty()))
}

fn box_border(inner_width: usize, character: char) -> String {
    format!("  +{}+", character.to_string().repeat(inner_width + 2))
}

fn box_text(text: &str, inner_width: usize) -> String {
    let text = truncate_line(text, inner_width);
    format!(
        "  | {}{} |",
        text,
        " ".repeat(inner_width.saturating_sub(text.chars().count()))
    )
}

fn truncate_line(text: &str, available: usize) -> String {
    if text.chars().count() <= available {
        return text.to_string();
    }
    let prefix: String = text.chars().take(available.saturating_sub(3)).collect();
    format!("{prefix}...")
}

fn box_text_with_marker(text: &str, inner_width: usize, selected: bool) -> String {
    let marker = if selected { "› " } else { "  " };
    let content = format!("{}{}", marker, text);
    box_text(&content, inner_width)
}

fn fit_name(name: std::borrow::Cow<'_, str>) -> String {
    let width = terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(100);
    truncate_text(&name, (width / 3).max(16))
}

fn fit_path(path: &Path, name_len: usize) -> String {
    let width = terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(100);
    let available = width.saturating_sub(name_len + 8).max(12);
    let text = path.to_string_lossy();
    truncate_text(&text, available)
}

fn truncate_text(text: &str, available: usize) -> String {
    if text.chars().count() <= available {
        return text.to_string();
    }
    let suffix: String = text
        .chars()
        .rev()
        .take(available.saturating_sub(3))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("...{suffix}")
}
fn print_best(paths: &[PathBuf], query: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some((path, _)) = rank(paths, query).first() {
        println!("{}", path.display());
    }
    Ok(())
}

fn settings() -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path().ok_or("cannot determine config directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    println!(
        "wtfis settings\n\nConfig: {}\n\nEnter search roots, one per line. Empty input keeps defaults.",
        path.display()
    );
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let roots: Vec<_> = input
        .split(':')
        .filter(|item| !item.trim().is_empty())
        .map(|item| PathBuf::from(item.trim()))
        .filter(|path| path.is_dir())
        .collect();
    if roots.is_empty() {
        println!("No changes made.");
        return Ok(());
    }
    fs::write(
        path,
        toml::to_string_pretty(&Config {
            roots: Some(roots),
            scan_hidden: Some(false),
            exact_depth: None,
            recent: load_config().recent,
        })?,
    )?;
    println!("Settings saved.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks_exact_before_partial() {
        let a = PathBuf::from("/tmp/Mascotify");
        let b = PathBuf::from("/tmp/Mascotify Website");
        let result = rank(&[b, a.clone()], "mascotify");
        assert_eq!(result[0].0, a);
    }
    #[test]
    fn fuzzy_handles_typo() {
        assert!(fuzzy_score(Path::new("/tmp/Mascotify"), "mascotfy").is_some());
    }
}
