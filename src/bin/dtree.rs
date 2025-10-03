use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use ignore::WalkBuilder;
use lscolors::LsColors;
use nu_ansi_term::{Color, Style};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

/// Represents a directory entry in the file system.
/// Contains information about a directory and its contents.
#[derive(Debug, Deserialize)]
struct DirEntry {
    /// path is a string representing the relative path within the directory.
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nuon schema")]
    path: String,
    /// Recursive list of directory entries.
    entries: Vec<Node>,
}

/// Represents a file in the cache with its path and description.
#[derive(Debug, Deserialize)]
struct FileEntry {
    /// Path to the file, e.g. "/home/user/...",
    path: String,
    /// Textual description of the file. Default is `Doc::empty()`.
    #[serde(default)]
    doc: Doc,
}

/// Represents a structured document with metadata about a file, including its description.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Default)]
struct Doc {
    /// The file's description, defaulting to empty string.
    #[serde(default)]
    fileDescription: String,
    /// The file's joyfulness, defaulting to empty JSON value.
    #[serde(default, alias = "howMuchJoyDoesThisFileBringYou")]
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nuon schema")]
    joyThisFileBrings: serde_json::Value,
    /// The file's personality emoji, defaulting to empty string.
    #[serde(default, alias = "emojiThatExpressesThisFilesPersonality")]
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nuon schema")]
    personalityEmoji: String,
}

/// A root directory structure for dirdocs documentation.
/// Contains a placeholder `root` and managed nodes under it.
#[derive(Debug, Deserialize)]
struct DirdocsRoot {
    /// `root` is a placeholder kept to match .dirdocs.nuon schema.
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nuon schema")]
    root: String,
    /// The collection of nodes under the root, managed by directory tree logic.
    entries: Vec<Node>,
}

/// A container for human-readable descriptions of files and directories.
#[derive(Debug, Default, Clone)]
struct FileDocInfo {
    /// This field stores a human-readable description of the file or directory.
    description: String,
}

/// Arguments for the `dtree` command.
#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "dtree — tree-style view + descriptions from .dirdocs.nuon"
)]
/// Args holds the command line arguments for the tree utility. It contains options to control directory traversal and output formatting.
struct Args {
    /// Start directory (default: .).
    #[clap(default_value = ".")]
    directory: String,

    /// Show hidden files (dotfiles).
    #[clap(long, short = 'a')]
    all: bool,

    /// Comma-separated directory names to ignore (repeat to add more).
    #[clap(short = 'i', long = "ignore", value_delimiter = ',')]
    ignore: Vec<String>,

    /// Classic tree connectors (├── └── │   ).
    #[clap(long)]
    boring: bool,
}

/// `Theme` represents a directory navigation theme, storing visual styles and enabled status.
#[derive(Clone)]
struct Theme {
    /// A `Style` for the header, kept to match .dirdocs.nuon schema.
    #[expect(dead_code, reason = "Field kept to match .dirdocs.nuon schema")]
    header: Style,
    /// A `Style` for directory names, kept to match .dirdocs.nuon schema.
    dir: Style,
    /// A `Style` for file names, kept to match .dirdocs.nuon schema.
    file: Style,
    /// Whether the theme is enabled.
    enabled: bool,
}

impl Theme {
    /// Handle enabling or disabling the default theme settings. This function sets up the styling for
    /// headers, directories, and files based on the `enabled` boolean argument. If enabled,
    /// it applies green color to headers and bold formatting; directories are shown in cyan.
    ///
    /// Parameters:
    /// - `enabled`: A boolean indicating whether to enable the default theme styling.
    ///
    /// Returns:
    /// - The updated `Self` instance with styled elements configured according to the
    ///   provided `enabled` value.
    fn default_enabled(enabled: bool) -> Self {
        Self {
            header: Style::new().fg(Color::Green).bold(),
            dir: Style::new().fg(Color::Cyan),
            file: Style::new(), // default/no color for files if LS_COLORS is absent
            enabled,
        }
    }
}

/// Handle loading a `nu` theme configuration and return it as an optional [`Theme`].
///
/// This function uses the `nu` command-line tool to fetch a theme configuration,
/// parses it with Serde JSON, and constructs a Theme object using helper functions.
/// Colors are assigned based on the returned map values. If errors occur during
/// parsing or command execution, returns `None`.
///
/// Parameters:
/// - None; the function is called without explicit parameters.
///
/// Returns:
/// - An `Option<Theme>`: Some theme if successful, None on failure.
///
/// Errors:
/// - I/O errors when running the `nu` command or parsing JSON.
/// - Errors from Serde JSON deserialization.
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

/// Handle converting a JSON value to an optional `Style` object.
///
/// Parses the given JSON value and returns a `Style` if it represents a valid style definition.
/// If the input is a string, it calls `parse_style_string`.
/// If the input is an object (dictionary), it extracts foreground (`fg`), background (`bg`), and attribute (`attr`) values,
/// parses their colors or attributes using `parse_color`, then constructs a `Style` object with the result.
/// Returns `None` for invalid or unsupported input types.
///
/// Parameters:
/// - `v`: A reference to a `serde_json::Value` representing the style definition.
///
/// Returns:
/// - An `Option<Style>` which contains the parsed style if valid, or `None` otherwise.
///
/// Errors:
/// - Occurs in `parse_style_string`, `parse_color`, or when parsing JSON values.
/// - The errors are propagated directly from these functions.
///
/// Safety:
/// - This function does not perform any unsafe operations.
///
/// Notes:
/// - The input JSON must match the expected format for style definitions (e.g., keys like `"fg"`, `"bg"`, and `"attr"`).
/// - The function assumes that `parse_color` will return a valid color value for foreground and background.
/// - Attribute strings are case-insensitive (e.g., `"BOLD"` is equivalent to `"bold"`).
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

/// Parse a style string into a `Style` object.
///
/// This function takes a string representing color or formatting options and
/// returns an optional `Style` object. It first attempts to parse a hex RGB color.
/// If that fails, it splits the string on underscores and applies formatting
/// options like `bold`, `underline`, or `reverse`. It also supports shorthand
/// for formatting (e.g., "u" = underline).
///
/// Parameters:
/// - `s`: A string slice representing the style.
///
/// Returns:
/// - An optional `Style` object if parsing is successful, or `None` on error.
///
/// Errors:
/// - Returns `None` on failure to parse the string.
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

/// Parses a hex-encoded RGB color string and returns an optional `Color::Rgb`.
///
/// This function checks if the input starts with a '#' and has exactly 6 hex characters.
/// It then parses each pair of digits as an 8-bit unsigned integer (0-255)
/// and returns a `Color::Rgb` if successful.
///
/// Parameters:
/// - `s`: A string slice representing the hex-encoded RGB color (e.g., "#FF0000").
///
/// Returns:
/// - `Some(Color::Rgb(r, g, b))` if the input is valid.
/// - `None` otherwise.
///
/// Errors:
/// This function does not explicitly handle errors, but internally uses `.ok()?.` to propagate any
/// parsing failures from `u8::from_str_radix`.
///
/// Notes:
/// The function assumes a standard hex RGB format (e.g., "#RRGGBB").
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

/// Handles parsing of color names into the `Color` enum.
///
/// Converts string representations like "black", "red", or "light_blue" into the corresponding color variant.
/// Supports both named colors (e.g., `Black`, `Red`) and RGB codes for "dark_gray", "light_black", etc.
///
/// # Parameters:
/// - `name`: A string slice representing the color name to parse.
///
/// # Returns:
/// - `Some(Color)` if the `name` matches a valid color variant,
///   or `None` otherwise.
///
/// # Notes:
/// - The function supports both named colors (e.g., "black") and RGB-based variants.
///   - For example, `"dark_gray"` is parsed as `Color::Rgb(128, 128, 128)`.
///   - `"light_red"` is parsed as `Color::Rgb(255, 167, 167)`.
/// - Unrecognized names result in `None`.
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

/// Handle the `dtree` command, which displays a tree-style view of directory contents along with file descriptions loaded from `.dirdocs.nuon` files.
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

/// Prints a tree-style view of the directory structure, with colored names and optional descriptions from `.dirdocs.nuon` files.
///
/// Parameters:
/// - `dir`: The directory to start printing from.
/// - `project_root`: Optional path to the project root (for relative description lookups).
/// - `desc_map`: A map of file paths to their descriptions, loaded from `.dirdocs.nuon` files.
/// - `ignore`: A set of directory names to ignore (case-sensitive).
/// - `prefix`: The current indentation level for the tree.
/// - `theme`: Custom styling configuration for colors and symbols.
/// - `ls_colors`: Whether to use LS_COLORS environment variable for colorization (if enabled).
/// - `emoji_mode`: Whether to use emoji-based connectors instead of standard tree symbols.
/// - `show_all`: If true, show hidden files (dotfiles).
///
/// Returns:
/// - `Ok(())` on success.
///
/// Errors:
/// - I/O errors when reading/writing files or directory entries.
/// - Deserialization errors from `.dirdocs.nuon` files.
/// - Errors when resolving or applying custom themes.
///
/// Notes:
/// - The tree is printed recursively, with directory structures showing under their parent.
/// - Descriptions from `.dirdocs.nuon` are added if available, with emoji-based connector support.
/// - The `prefix` is built incrementally to reflect directory depth, with `├──`, `└──`, or emoji-based symbols.
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

/// Represents a single node in the directory tree, containing metadata and path information.
struct Child {
    /// The path of the Child node, stored with its full filesystem path.
    path: PathBuf,
    /// The name of the file or directory, without the extension.
    name: String,
    /// The name of the file or directory in lowercase for sorting purposes.
    name_lower: String,
    /// Whether the path points to a directory or file.
    is_dir: bool,
    /// Metadata about the path, if available.
    meta: Option<fs::Metadata>,
}

/// This function lists children of a directory, including files and subdirectories.
///
/// Parameters:
/// - `dir`: The path to the directory whose children are being listed.
/// - `show_all`: If true, do not skip hidden files; otherwise, hide non-user-writable entries.
/// - `ignore_names`: A set of names to skip when listing children (directories only).
///
/// Returns:
/// - `Vec<Child>`: A list of child entries representing files and directories.
///
/// Notes:
/// - This function constructs a walk of the directory tree with specified options and filters out ignored names.
/// - It handles both file metadata and directory existence checks to ensure accurate results.
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

/// `paint_name` is a function that colorizes the name of a directory or file based on metadata and theme settings.
///
/// It prioritizes LS_COLORS for ANSI escape codes, falling back to a configured `Theme` if necessary.
///
/// Parameters:
/// - `name`: The name of the directory or file to colorize.
/// - `path`: A reference to the full path of the item.
/// - `meta`: Optional metadata (e.g., file size, permissions).
/// - `is_dir`: Whether the item is a directory.
/// - `theme`: A reference to the color theme configuration.
/// - `ls_colors`: An optional reference to LS_COLORS for ANSI escape code support.
///
/// Returns:
/// - A string containing the colorized name of the item, potentially with ANSI escape codes.
///
/// Errors:
/// - No explicit errors are returned; failures are typically handled internally via optional references and `ok()`/`unwrap_or_default()`.
///
/// Notes:
/// - If `theme.enabled` is false, the name is returned as-is with no coloring.
/// - The function supports both terminal color and emoji-based connectors depending on the context.
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

/// Finds the project root by searching for a `.dirdocs.nuon` file starting from the given directory.
///
/// This function traverses up the directory hierarchy, checking for a `.dirdocs.nuon` file in each
/// parent directory. If found, it returns the path to that root directory as a `PathBuf`; otherwise,
/// it returns `None`.
///
/// Parameters:
/// - `start`: A reference to the starting directory path.
///
/// Returns:
/// - An `Option<PathBuf>` representing the project root, or `None` if no `.dirdocs.nuon` file is found.
///
/// Errors:
/// - This function does not return explicit error values, but may panic if `cur.parent()?` fails
///   (e.g., when the starting path is already at the root).
///
/// Notes:
/// - The search continues upward until either a `.dirdocs.nuon` file is found or the root of the filesystem
///   is reached.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".dirdocs.nuon").exists() {
            return Some(cur);
        }
        let parent = cur.parent()?.to_path_buf();
        if parent == cur {
            return None;
        }
        cur = parent;
    }
}

/// Load description files from a diredocs root.
///
/// Parses (`root.join(".dirdocs.nuon")`) to get a root diredocs tree,
/// and recursively visits nodes to collect file descriptions.
/// Each `Node::File`'s description is stored in a map with the full path.
/// Returns an error if reading or parsing fails.
///
/// Parameters:
/// - `root`: Path to diredocs root directory.
///
/// Returns:
/// A `Result<HashMap<String, FileDocInfo>>` mapping file paths to their description info, or an error.
///
/// Errors:
/// - I/O errors when reading files,
/// - JSON parsing errors from `serde_json`,
/// - or invalid diredocs structure.
fn load_descriptions(root: &Path) -> anyhow::Result<HashMap<String, FileDocInfo>> {
    let mut map: HashMap<String, FileDocInfo> = HashMap::new();
    let s = fs::read_to_string(root.join(".dirdocs.nuon"))?;
    let parsed: DirdocsRoot = serde_json::from_str(&s)?;

    /// Recursively visits all nodes in a directory structure, collecting documentation info.
    ///
    /// Parameters:
    /// - `nodes`: A slice of nodes to visit (typically from a directory tree).
    /// - `out`: A mutable reference to a hash map storing file documentation info.
    ///
    /// Returns:
    /// - None
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

/// Handle a path relative to an anchor point, returning it as a string.
/// This function computes the relative path between `p` and `base`, using the
/// `pathdiff::diff_paths` crate to determine it. If no relative path is found,
/// the original `p` path is returned instead.
///
/// Parameters:
/// - `p`: The absolute path to compute the relative of.
/// - `base`: The base path used for computing the relative path.
///
/// Returns:
/// A string representing the relative path from `base` to `p`.
///
/// Errors:
/// This function does not return explicit errors. It handles all failures internally
/// via the `.unwrap_or_else()` chain, which returns `p` itself if no relative path
/// can be computed.
///
/// Notes:
/// This function is a convenience wrapper around `pathdiff::diff_paths`, with
/// fallback behavior to return the original path if no relative path is found.
fn rel_str(p: &Path, base: &Path) -> String {
    pathdiff::diff_paths(p, base)
        .unwrap_or_else(|| p.to_path_buf())
        .to_string_lossy()
        .into()
}
