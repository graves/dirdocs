use chrono::{DateTime, Utc};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use clap::Parser;
use humansize::{DECIMAL, format_size};
use lscolors::LsColors;
use nu_ansi_term::{Color, Style};
use nu_table::{NuTable, TableTheme, TextStyle};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use tabled::grid::records::vec_records::Text;
use terminal_size::{Width as TermWidth, terminal_size};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "dls — Nushell-style `ls` + description from .dirdocs.nu"
)]
struct Args {
    #[clap(default_value = ".")]
    directory: String,
    #[clap(long, short = 'a')]
    all: bool,
    #[clap(long, short = 'R')]
    recursive: bool,
    /// Show extra fun columns: personality & joy rating
    #[clap(long)]
    fun: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

#[derive(Debug, Deserialize)]
struct DirEntry {
    entries: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    path: String,
    #[serde(default)]
    doc: Doc,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Default)]
struct Doc {
    #[serde(default)]
    fileDescription: String,
    #[serde(default, alias = "howMuchJoyDoesThisFileBringYou")]
    joyThisFileBrings: serde_json::Value,
    #[serde(default, alias = "emojiThatExpressesThisFilesPersonality")]
    personalityEmoji: String,
}

#[derive(Debug, Deserialize)]
struct DirdocsRoot {
    entries: Vec<Node>,
}

#[derive(Debug, Default, Clone)]
struct FileDocInfo {
    description: String,
    personality: String,
    joy: String, // stringified value
}

#[derive(Debug)]
struct RowRaw {
    path: PathBuf,
    name: String,
    ty: String,
    size_h: String,
    modified_h: String,
    description: String,
    personality: String,
    joy: String,
}

#[derive(Clone)]
struct Theme {
    header: Style,
    dir: Style,
    filesize: Style,
    date: Style,
    index: Style,
    enabled: bool,
}

impl Theme {
    fn default_enabled(enabled: bool) -> Self {
        Self {
            header: Style::new().fg(Color::Green).bold(),
            dir: Style::new().fg(Color::Cyan),
            filesize: Style::new().fg(Color::Cyan),
            date: Style::new().fg(Color::Purple),
            index: Style::new(),
            enabled,
        }
    }
}

fn try_load_nu_theme() -> Option<Theme> {
    let out = Command::new("nu")
        .args(["-c", "$env.config.color_config | to json"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let json = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;

    let resolve = |key: &str| -> Option<Style> { value_to_style(json.get(key)?) };

    let header = resolve("header").or_else(|| resolve("shape_table"));
    let dir = resolve("shape_directory")
        .or_else(|| resolve("shape_filepath"))
        .or_else(|| resolve("string"));
    let filesize = resolve("filesize").or_else(|| resolve("int"));
    let date = resolve("date").or_else(|| resolve("shape_datetime"));
    let index = resolve("row_index").or_else(|| header.clone());

    Some(Theme {
        header: header.unwrap_or_else(|| Style::new().fg(Color::Green).bold()),
        dir: dir.unwrap_or_else(|| Style::new().fg(Color::Cyan)),
        filesize: filesize.unwrap_or_else(|| Style::new().fg(Color::Cyan)),
        date: date.unwrap_or_else(|| Style::new().fg(Color::Purple)),
        index: index.unwrap_or_else(Style::new),
        enabled: true,
    })
}

fn value_to_style(v: &serde_json::Value) -> Option<Style> {
    match v {
        serde_json::Value::String(s) => parse_style_string(s),
        serde_json::Value::Object(map) => {
            let mut st = Style::new();
            if let Some(fg) = map.get("fg").and_then(|x| x.as_str()) {
                if let Some(c) = parse_color(fg) {
                    st = st.fg(c);
                }
            }
            if let Some(bg) = map.get("bg").and_then(|x| x.as_str()) {
                if let Some(c) = parse_color(bg) {
                    st = st.on(c);
                }
            }
            if let Some(attr) = map.get("attr").and_then(|x| x.as_str()) {
                let a = attr.to_lowercase();
                if a.contains('b') {
                    st = st.bold();
                }
                if a.contains('u') {
                    st = st.underline();
                }
                if a.contains('r') {
                    st = st.reverse();
                }
            }
            Some(st)
        }
        _ => None,
    }
}

fn parse_style_string(s: &str) -> Option<Style> {
    let s = s.trim().to_lowercase();
    if let Some(rgb) = parse_hex_rgb(&s) {
        return Some(Style::new().fg(rgb));
    }
    let mut st = Style::new();
    let parts: Vec<&str> = s.split('_').collect();
    for p in parts {
        match p {
            "bold" => st = st.bold(),
            "underline" | "u" => st = st.underline(),
            "reverse" | "r" => st = st.reverse(),
            other => {
                if let Some(c) = parse_color(other) {
                    st = st.fg(c);
                }
            }
        }
    }
    Some(st)
}

fn parse_hex_rgb(s: &str) -> Option<Color> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    None
}

fn parse_color(name: &str) -> Option<Color> {
    use Color::*;
    Some(match name {
        "black" => Black,
        "red" => Red,
        "green" => Green,
        "yellow" => Yellow,
        "blue" => Blue,
        "purple" | "magenta" => Purple,
        "cyan" => Cyan,
        "white" => White,

        "dark_gray" | "grey" | "gray" => Color::Rgb(128, 128, 128),
        "light_black" => Color::Rgb(96, 96, 96),
        "light_white" => Color::Rgb(240, 240, 240),

        "light_red" => LightRed,
        "light_green" => LightGreen,
        "light_yellow" => LightYellow,
        "light_blue" => LightBlue,
        "light_purple" | "light_magenta" => LightPurple,
        "light_cyan" => LightCyan,

        _ => return None,
    })
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let start = PathBuf::from(&args.directory)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&args.directory));

    let project_root = find_project_root(&start);
    let desc_map = project_root
        .as_ref()
        .and_then(|r| load_descriptions(r).ok())
        .unwrap_or_default();

    if args.recursive {
        for entry in WalkDir::new(&start).min_depth(0).max_open(256) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().is_dir() {
                let dir_path = entry.path();
                println!("{}", dir_path.display());
                let rows =
                    collect_rows_for_dir(dir_path, project_root.as_deref(), &desc_map, args.all)?;
                print_nu_table(&rows, args.fun);
                println!();
            }
        }
    } else {
        let rows = collect_rows_for_dir(&start, project_root.as_deref(), &desc_map, args.all)?;
        print_nu_table(&rows, args.fun);
    }

    Ok(())
}

fn collect_rows_for_dir(
    dir: &Path,
    project_root: Option<&Path>,
    desc_map: &HashMap<String, FileDocInfo>,
    show_all: bool,
) -> anyhow::Result<Vec<RowRaw>> {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("dls: cannot access {}: {}", dir.display(), e);
            return Ok(vec![]);
        }
    };

    let mut rows: Vec<RowRaw> = Vec::new();

    for dent in entries {
        let dent = match dent {
            Ok(d) => d,
            Err(_) => continue,
        };
        let name = dent.file_name();
        if !show_all && is_hidden(&name) {
            continue;
        }

        let path = dent.path();
        let meta = match dent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let ty = if meta.is_dir() { "dir" } else { "file" }.to_string();

        let size_raw = if meta.is_file() { meta.len() } else { 0 };
        let size_h = if size_raw == 0 {
            "0 B".to_string()
        } else {
            format_size(size_raw, DECIMAL)
        };

        let modified_h = meta
            .modified()
            .ok()
            .map(|t| {
                let dt: DateTime<Utc> = t.into();
                HumanTime::from(Utc::now() - dt).to_text_en(Accuracy::Rough, Tense::Past)
            })
            .unwrap_or_else(|| "—".to_string());

        let rel_key = if let Some(root) = project_root {
            rel_str(&path, root)
        } else {
            rel_str(&path, dir)
        };

        let doc = desc_map.get(&rel_key).cloned().unwrap_or_default();

        rows.push(RowRaw {
            path: path.clone(),
            name: name.to_string_lossy().to_string(),
            ty,
            size_h,
            modified_h,
            description: doc.description,
            personality: doc.personality,
            joy: doc.joy,
        });
    }

    // sort: files first, then dirs, by name
    rows.sort_by(|a, b| match (a.ty.as_str(), b.ty.as_str()) {
        ("file", "dir") => std::cmp::Ordering::Less,
        ("dir", "file") => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(rows)
}

fn print_nu_table(rows: &[RowRaw], fun: bool) {
    // Terminal width
    let mut width = terminal_size()
        .map(|(TermWidth(w), _)| w as usize)
        .unwrap_or(0);
    if width < 4 {
        eprintln!("Width must be >= 4; defaulting to 80");
        width = 80;
    }

    // Color on/off
    let color_on = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();

    // Theme (for header, index, size, date)
    let theme = if color_on {
        try_load_nu_theme().unwrap_or_else(|| Theme::default_enabled(true))
    } else {
        Theme::default_enabled(false)
    };

    // LS_COLORS for the NAME column
    let ls_colors = if color_on { LsColors::from_env() } else { None };

    // Headers (conditionally add personality & joy)
    let mut headers = vec!["#", "name", "type", "size", "modified", "description"];
    if fun {
        headers.push("personality");
        headers.push("joy");
    }
    let cols = headers.len();

    let headers_cells: Vec<Text<String>> = headers
        .iter()
        .map(|h| Text::new((*h).to_string()))
        .collect();

    // Rows
    let mut data_rows: Vec<Vec<Text<String>>> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let paint = |st: &Style, s: &str| -> String {
            if theme.enabled {
                st.paint(s).to_string()
            } else {
                s.to_string()
            }
        };

        let idx = paint(&theme.index, &i.to_string());

        // NAME: prefer LS_COLORS, fallback to theme.dir for directories
        let name = if let Some(ls) = ls_colors.as_ref() {
            if let Some(st) = ls.style_for_path(&r.path) {
                st.to_ansi_term_style().paint(&r.name).to_string()
            } else if r.ty == "dir" && theme.enabled {
                paint(&theme.dir, &r.name)
            } else {
                r.name.clone()
            }
        } else if r.ty == "dir" && theme.enabled {
            paint(&theme.dir, &r.name)
        } else {
            r.name.clone()
        };

        let size = paint(&theme.filesize, &r.size_h);
        let modified = paint(&theme.date, &r.modified_h);

        let mut row = vec![
            Text::new(idx),
            Text::new(name),
            Text::new(r.ty.clone()),
            Text::new(size),
            Text::new(modified),
            Text::new(r.description.clone()),
        ];
        if fun {
            row.push(Text::new(as_emoji_presentation(&r.personality)));
            row.push(Text::new(r.joy.clone()));
        }
        debug_assert_eq!(row.len(), cols);
        data_rows.push(row);
    }

    // Table (rows.len() + header row)
    let mut table = NuTable::new(rows.len() + 1, cols);
    table.set_row(0, headers_cells);
    for (i, row) in data_rows.into_iter().enumerate() {
        table.set_row(i + 1, row);
    }

    table.set_data_style(TextStyle::basic_left());
    table.set_header_style(TextStyle::basic_center().style(theme.header));
    table.set_theme(TableTheme::rounded());
    // We render our own "#" index; no built-in index
    table.set_structure(false, true, false);

    let output = table
        .draw(width)
        .unwrap_or_else(|| format!("Couldn't fit table into {width} columns!"));
    println!("{output}");
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".dirdocs.nu").exists() {
            return Some(cur);
        }
        let parent = cur.parent()?.to_path_buf();
        if parent == cur {
            return None;
        }
        cur = parent;
    }
}

fn load_descriptions(root: &Path) -> anyhow::Result<HashMap<String, FileDocInfo>> {
    let mut map: HashMap<String, FileDocInfo> = HashMap::new();
    let s = fs::read_to_string(root.join(".dirdocs.nu"))?;
    let parsed: DirdocsRoot = serde_json::from_str(&s)?;

    fn v_to_joy(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::Null => String::new(),
            serde_json::Value::String(s) => s.clone(),
            // numbers, bools, arrays, objects – compact string
            other => other.to_string(),
        }
    }

    fn visit(nodes: &[Node], out: &mut HashMap<String, FileDocInfo>) {
        for n in nodes {
            match n {
                Node::Dir(d) => visit(&d.entries, out),
                Node::File(f) => {
                    let desc = f.doc.fileDescription.trim().to_string();
                    let personality = f.doc.personalityEmoji.trim().to_string();
                    let joy = v_to_joy(&f.doc.joyThisFileBrings);
                    if !(desc.is_empty() && personality.is_empty() && joy.is_empty()) {
                        out.insert(
                            f.path.clone(),
                            FileDocInfo {
                                description: desc,
                                personality,
                                joy,
                            },
                        );
                    }
                }
            }
        }
    }

    visit(&parsed.entries, &mut map);
    Ok(map)
}

fn rel_str(p: &Path, base: &Path) -> String {
    pathdiff::diff_paths(p, base)
        .unwrap_or_else(|| p.to_path_buf())
        .to_string_lossy()
        .into()
}

fn as_emoji_presentation(s: &str) -> String {
    if s.is_empty() || s.contains('\u{FE0F}') {
        return s.to_string();
    }
    // cheap check: if it’s a single codepoint, force emoji presentation
    if s.chars().count() == 1 {
        let mut out = s.to_string();
        out.push('\u{FE0F}'); // VS16
        return out;
    }
    s.to_string()
}
