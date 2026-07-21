use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

const MAX_RESULTS: usize = 8;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    roots: Option<Vec<PathBuf>>,
    scan_hidden: Option<bool>,
    exact_depth: Option<usize>,
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
    let config = load_config();
    let roots = config.roots.unwrap_or_else(default_roots);
    let projects = scan(
        &roots,
        config.scan_hidden.unwrap_or(false),
        config.exact_depth,
    );
    if projects.is_empty() {
        eprintln!("wtfis: no directories found in configured roots");
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return print_best(&projects, &query);
    }
    let selected = if query.is_empty() {
        picker(&projects, "")?
    } else {
        let matches = rank(&projects, &query);
        if matches.first().is_some_and(|(_, score)| *score == 0) {
            picker(&projects, &query)?
        } else {
            picker(&projects, &query)?
        }
    };
    if let Some(path) = selected {
        println!("{}", path.display());
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
    results.truncate(MAX_RESULTS);
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

fn picker(paths: &[PathBuf], initial: &str) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    terminal::enable_raw_mode()?;
    let mut out = io::stderr();
    let mut query = initial.to_string();
    let mut selected = 0usize;
    let mut rendered_lines = 0usize;
    let result = loop {
        let results = rank(paths, &query);
        selected = selected.min(results.len().saturating_sub(1));
        rendered_lines = render(&mut out, &query, &results, selected, rendered_lines)?;
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
            _ => {}
        }
    };
    terminal::disable_raw_mode()?;
    clear_inline(&mut out)?;
    Ok(result)
}

fn render(
    out: &mut impl Write,
    query: &str,
    results: &[(PathBuf, i64)],
    selected: usize,
    rendered_lines: usize,
) -> io::Result<usize> {
    if rendered_lines > 0 {
        queue!(
            out,
            cursor::MoveUp(rendered_lines as u16),
            cursor::MoveToColumn(0)
        )?;
    }
    queue!(
        out,
        cursor::MoveToColumn(0),
        Clear(ClearType::FromCursorDown),
        SetForegroundColor(Color::Cyan),
        Print("  wtfis "),
        ResetColor,
        Print("Search projects: "),
        SetAttribute(Attribute::Bold),
        Print(query),
        SetAttribute(Attribute::Reset),
        Print("\n")
    )?;
    for (index, (path, _)) in results.iter().enumerate() {
        queue!(
            out,
            Print(if index == selected { "  > " } else { "    " }),
            SetAttribute(if index == selected {
                Attribute::Bold
            } else {
                Attribute::Reset
            }),
            Print(path.file_name().unwrap_or_default().to_string_lossy()),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::DarkGrey),
            Print("  "),
            Print(path.display()),
            ResetColor,
            Print("\n")
        )?;
    }
    if results.is_empty() {
        queue!(
            out,
            SetForegroundColor(Color::DarkGrey),
            Print("    No matching folders"),
            ResetColor,
            Print("\n")
        )?;
    }
    queue!(
        out,
        SetForegroundColor(Color::DarkGrey),
        Print("\n  Up/Down move  Enter open  Esc cancel"),
        ResetColor,
        cursor::MoveToColumn(0)
    )?;
    out.flush()?;
    Ok(results.len() + 2 + usize::from(results.is_empty()))
}
fn clear_inline(out: &mut impl Write) -> io::Result<()> {
    execute!(
        out,
        cursor::MoveToColumn(0),
        Clear(ClearType::FromCursorDown)
    )
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
