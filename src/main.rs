use std::{
    env, fs,
    io::{self, IsTerminal},
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
    execute, terminal,
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
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

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
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
    )?;
    if let Some(path) = selected {
        remember(&mut config, path.clone())?;
        emit_path(&path)?;
    }
    Ok(())
}

fn default_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
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
            if !ignored_directory(path, scan_hidden) {
                paths.push(path.to_path_buf());
            }
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
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let height = terminal::size()
        .map(|(_, rows)| rows.clamp(7, 12))
        .unwrap_or(8);
    let mut session = UiSession::new(height)?;
    let mut query = initial.to_string();
    let mut selected = 0usize;
    let mut paths = None;
    let mut scan_receiver: Option<Receiver<Vec<PathBuf>>> = None;
    let mut scanning = false;
    let mut last_click: Option<(usize, Instant)> = None;

    let result = loop {
        if !query.is_empty() && paths.is_none() && scan_receiver.is_none() {
            scan_receiver = Some(start_scan(roots.to_vec(), scan_hidden, exact_depth));
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

        let mut results_area = Rect::default();
        session.terminal.draw(|frame| {
            results_area = render_frame(frame, &query, &results, selected, scanning);
        })?;

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
            }) => query.push(c),
            Event::Mouse(mouse)
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
            {
                let visible_count = results_area.height as usize;
                let start = result_start(results.len(), selected, visible_count);
                if results_area.contains(Position::new(mouse.column, mouse.row)) {
                    let clicked = start + mouse.row.saturating_sub(results_area.y) as usize;
                    if clicked >= results.len() {
                        continue;
                    }
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

    session.cleanup()?;
    Ok(result)
}

struct UiSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse_capture: bool,
    raw_mode: bool,
    cleaned: bool,
}

impl UiSession {
    fn new(height: u16) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut backend = CrosstermBackend::new(io::stdout());
        if let Err(error) = execute!(backend, cursor::SavePosition) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }

        let terminal = match Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), cursor::RestorePosition);
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        let mut session = Self {
            terminal,
            mouse_capture: false,
            raw_mode: true,
            cleaned: false,
        };
        if let Err(error) = execute!(session.terminal.backend_mut(), EnableMouseCapture) {
            let _ = session.cleanup();
            return Err(error);
        }
        session.mouse_capture = true;
        Ok(session)
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let mut first_error = None;
        if self.mouse_capture {
            if let Err(error) = execute!(self.terminal.backend_mut(), DisableMouseCapture) {
                first_error = Some(error);
            }
            self.mouse_capture = false;
        }
        if let Err(error) = self.terminal.clear() {
            first_error.get_or_insert(error);
        }
        if let Err(error) = execute!(self.terminal.backend_mut(), cursor::RestorePosition) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = self.terminal.show_cursor() {
            first_error.get_or_insert(error);
        }
        if self.raw_mode {
            if let Err(error) = terminal::disable_raw_mode() {
                first_error.get_or_insert(error);
            }
            self.raw_mode = false;
        }
        self.cleaned = true;
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
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

fn result_start(total: usize, selected: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        0
    } else {
        selected.saturating_sub(visible - 1).min(total - visible)
    }
}

fn render_frame(
    frame: &mut ratatui::Frame<'_>,
    query: &str,
    results: &[(PathBuf, i64)],
    selected: usize,
    scanning: bool,
) -> Rect {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" WTFIS ")
        .title_bottom(" local project finder ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let label = if query.is_empty() {
        "Recent projects ["
    } else {
        "Search projects ["
    };
    let input = Line::from(vec![
        Span::styled(label, Style::default().fg(Color::White)),
        Span::styled(query, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("]", Style::default().fg(Color::Cyan)),
    ]);
    let cursor_line = Line::from(vec![Span::raw(label), Span::raw(query)]);
    let cursor_x = sections[0]
        .x
        .saturating_add(cursor_line.width().min(u16::MAX as usize) as u16)
        .min(sections[0].right().saturating_sub(1));
    frame.render_widget(Paragraph::new(input), sections[0]);
    frame.set_cursor_position(Position::new(cursor_x, sections[0].y));

    frame.render_widget(
        Paragraph::new("Up/Down navigate  Enter open  Esc cancel  Click select  Double-click open")
            .style(Style::default().fg(Color::DarkGray)),
        sections[1],
    );

    let visible = sections[2].height as usize;
    let start = result_start(results.len(), selected, visible);
    let end = (start + visible).min(results.len());
    if start == end {
        let message = if scanning {
            "Scanning folders..."
        } else if query.is_empty() {
            "Type to search folders"
        } else {
            "No matching folders"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(Color::DarkGray)),
            sections[2],
        );
    } else {
        let items: Vec<ListItem> = results[start..end]
            .iter()
            .enumerate()
            .map(|(index, (path, _))| {
                let actual = start + index;
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let style = if actual == selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(if actual == selected { "> " } else { "  " }, style),
                    Span::styled(name, style),
                    Span::styled("  ", style),
                    Span::styled(path.display().to_string(), style),
                ]))
            })
            .collect();
        frame.render_widget(List::new(items), sections[2]);
    }

    let footer = if results.is_empty() {
        "No selection".to_string()
    } else {
        format!("{}-{} of {}", start + 1, end, results.len())
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        sections[3],
    );
    sections[2]
}

fn remember(config: &mut Config, path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let recent = config.recent.get_or_insert_with(Vec::new);
    recent.retain(|item| item != &path);
    recent.insert(0, path);
    recent.truncate(MAX_RECENTS);
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

    #[test]
    fn result_start_keeps_selected_row_visible() {
        assert_eq!(result_start(10, 0, 3), 0);
        assert_eq!(result_start(10, 4, 3), 2);
        assert_eq!(result_start(10, 9, 3), 7);
    }
}
