use clap::Parser;
use lscolors::LsColors;
use nu_ansi_term::{Color, Style};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

// NEW
use ignore::WalkBuilder;

/* ---------------- .dirdocs.nu (subset) ---------------- */

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

#[derive(Debug, Deserialize)]
struct DirEntry {
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nu schema")]
    path: String,
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
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nu schema")]
    joyThisFileBrings: serde_json::Value,
    #[serde(default, alias = "emojiThatExpressesThisFilesPersonality")]
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nu schema")]
    personalityEmoji: String,
}

#[derive(Debug, Deserialize)]
struct DirdocsRoot {
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nu schema")]
    root: String,
    entries: Vec<Node>,
}

#[derive(Debug, Default, Clone)]
struct FileDocInfo {
    description: String,
}

/* ---------------- CLI ---------------- */

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "dtree — tree-style view + descriptions from .dirdocs.nu"
)]
struct Args {
    /// Start directory (default: .)
    #[clap(default_value = ".")]
    directory: String,

    /// Show hidden files (dotfiles)
    #[clap(long, short = 'a')]
    all: bool,

    /// Comma-separated directory names to ignore (repeat to add more)
    /// e.g. -i target,node_modules -i dist
    #[clap(short = 'i', long = "ignore", value_delimiter = ',')]
    ignore: Vec<String>,

    /// Classic tree connectors (├── └── │   )
    #[clap(long)]
    boring: bool,
}

/* ---------------- Theme (like dls) ---------------- */

#[derive(Clone)]
struct Theme {
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nu schema")]
    header: Style,
    dir: Style,
    file: Style,
    enabled: bool,
}

impl Theme {
    fn default_enabled(enabled: bool) -> Self {
        Self {
            header: Style::new().fg(Color::Green).bold(),
            dir: Style::new().fg(Color::Cyan),
            file: Style::new(), // default/no color for files if LS_COLORS is absent
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
    let file = resolve("shape_filepath")
        .or_else(|| resolve("string"))
        .or_else(|| resolve("nothing"));

    Some(Theme {
        header: header.unwrap_or_else(|| Style::new().fg(Color::Green).bold()),
        dir: dir.unwrap_or_else(|| Style::new().fg(Color::Cyan)),
        file: file.unwrap_or_else(Style::new),
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
    for p in s.split('_') {
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

/* ---------------- main ---------------- */

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let start = PathBuf::from(&args.directory)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&args.directory));

    // Colors on?
    let color_on = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let theme = if color_on {
        try_load_nu_theme().unwrap_or_else(|| Theme::default_enabled(true))
    } else {
        Theme::default_enabled(false)
    };
    let ls_colors = if color_on { LsColors::from_env() } else { None };

    // descriptions
    let project_root = find_project_root(&start);
    let desc_map = project_root
        .as_ref()
        .and_then(|r| load_descriptions(r).ok())
        .unwrap_or_default();

    // ignore set
    let ignore: HashSet<String> = args.ignore.into_iter().collect();

    // --- Colored root label (basename, not full path) ---
    let root_label = start
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| start.display().to_string());
    let root_meta = fs::metadata(&start).ok();
    let root_colored = paint_name(
        &root_label,
        &start,
        root_meta.as_ref(),
        true,
        &theme,
        &ls_colors,
    );
    println!("{root_colored}");

    // walk
    print_tree_dir(
        &start,
        project_root.as_deref(),
        &desc_map,
        &ignore,
        "",
        &theme,
        &ls_colors,
        !args.boring,
        args.all,
    )?;

    Ok(())
}

/* ---------------- tree printer ---------------- */

fn print_tree_dir(
    dir: &Path,
    project_root: Option<&Path>,
    desc_map: &HashMap<String, FileDocInfo>,
    ignore: &HashSet<String>,
    prefix: &str,
    theme: &Theme,
    ls_colors: &Option<LsColors>,
    emoji_mode: bool,
    show_all: bool,
) -> anyhow::Result<()> {
    // --- list immediate children honoring .gitignore + globals + hidden + user ignore ---
    let mut entries = list_children(dir, show_all, ignore);

    // sort: dirs first, then case-insensitive name
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name_lower.cmp(&b.name_lower),
    });

    let last_idx = entries.len().saturating_sub(1);

    for (i, ent) in entries.into_iter().enumerate() {
        let is_last = i == last_idx;
        let path = ent.path;
        let meta = ent.meta;
        let is_dir = ent.is_dir;

        // connectors
        let (connector, next_prefix) = if emoji_mode {
            if is_last {
                (if is_dir { "🪾 " } else { "🍃 " }, format!("{prefix}   "))
            } else {
                (if is_dir { "🪾 " } else { "🍃 " }, format!("{prefix}🪾  "))
            }
        } else {
            if is_last {
                ("└── ", format!("{prefix}    "))
            } else {
                ("├── ", format!("{prefix}│   "))
            }
        };

        // name (colorized)
        let colored_name = paint_name(&ent.name, &path, meta.as_ref(), is_dir, theme, ls_colors);

        // description
        let rel_key = match project_root {
            Some(root) => rel_str(&path, root),
            None => rel_str(&path, dir),
        };
        let desc = desc_map
            .get(&rel_key)
            .map(|d| d.description.as_str())
            .unwrap_or("");

        if desc.is_empty() {
            println!("{prefix}{connector}{colored_name}");
        } else {
            println!("{prefix}{connector}{colored_name} — {desc}");
        }

        if is_dir {
            print_tree_dir(
                &path,
                project_root,
                desc_map,
                ignore,
                &next_prefix,
                theme,
                ls_colors,
                emoji_mode,
                show_all,
            )?;
        }
    }

    Ok(())
}

/* ---------------- list children with ignore ---------------- */

struct Child {
    path: PathBuf,
    name: String,
    name_lower: String,
    is_dir: bool,
    meta: Option<fs::Metadata>,
}

fn list_children(dir: &Path, show_all: bool, ignore_names: &HashSet<String>) -> Vec<Child> {
    let mut wb = WalkBuilder::new(dir);
    wb.max_depth(Some(1))
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .hidden(!show_all);

    let mut out: Vec<Child> = Vec::new();

    for dent in wb.build().filter_map(|r| r.ok()) {
        if dent.depth() == 0 {
            continue; // skip the dir itself
        }
        // Skip user-named ignores (directories only)
        if let Some(name) = dent.file_name().to_str() {
            if dent.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
                && ignore_names.contains(name)
            {
                continue;
            }
        }

        let path = dent.path().to_path_buf();
        let name = dent.file_name().to_string_lossy().to_string();

        // pull metadata (best effort)
        let meta = dent.metadata().ok().or_else(|| fs::metadata(&path).ok());
        let is_dir = dent
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or_else(|| meta.as_ref().map(|m| m.is_dir()).unwrap_or(false));

        out.push(Child {
            name_lower: name.to_lowercase(),
            name,
            path,
            is_dir,
            meta,
        });
    }

    out
}

/* ---------------- color helper ---------------- */

fn paint_name(
    name: &str,
    path: &Path,
    meta: Option<&fs::Metadata>,
    is_dir: bool,
    theme: &Theme,
    ls_colors: &Option<LsColors>,
) -> String {
    // Prefer LS_COLORS (metadata-aware) if it actually emits ANSI.
    if theme.enabled {
        if let Some(ls) = ls_colors.as_ref() {
            if let Some(style) = ls.style_for_path_with_metadata(path, meta) {
                let painted = style.to_ansi_term_style().paint(name).to_string();
                if painted.contains("\u{1b}[") {
                    return painted;
                }
            }
        }
        // Fallback to theme
        if is_dir {
            theme.dir.paint(name).to_string()
        } else {
            theme.file.paint(name).to_string()
        }
    } else {
        name.to_string()
    }
}

/* ---------------- helpers ---------------- */

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

    fn visit(nodes: &[Node], out: &mut HashMap<String, FileDocInfo>) {
        for n in nodes {
            match n {
                Node::Dir(d) => visit(&d.entries, out),
                Node::File(f) => {
                    let desc = f.doc.fileDescription.trim().to_string();
                    if !desc.is_empty() {
                        out.insert(f.path.clone(), FileDocInfo { description: desc });
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
