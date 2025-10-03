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
    about = "dls — Nushell-style `ls` + description from .dirdocs.nuon"
)]

/// Command-line arguments for the dls tool. Contains options to specify a directory, recurse into subdirectories, and show additional file information.
struct Args {
    /// Directory to search (default is current directory).
    #[clap(default_value = ".")]
    directory: String,
    /// If set, show all files (not just regular ones).
    #[clap(long, short = 'a')]
    all: bool,
    /// If set, include subdirectories and contents of directories.
    #[clap(long, short = 'R')]
    recursive: bool,
    /// Show additional information about the files (personality and joy rating).
    #[clap(long)]
    fun: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

/// A directory entry containing a list of nodes. Used to represent files and subdirectories in the file system.
#[derive(Debug, Deserialize)]
struct DirEntry {
    /// A vector of `Node` instances that contain the actual content.
    entries: Vec<Node>,
}

/// A data structure representing a file entry with its path and documentation.
#[derive(Debug, Deserialize)]
struct FileEntry {
    /// The path of the file entry, uniquely identified in its tree.
    path: String,
    /// The documentation associated with this file entry, initialized to an empty doc.
    #[serde(default)]
    doc: Doc,
}

/// A struct representing a document's metadata, including description.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Default)]
struct Doc {
    /// The file's description.",
    #[serde(default)]
    fileDescription: String,
    /// Represents how much joy the file brings, with a default value. This field can be aliased as `howMuchJoyDoesThisFileBringYou`.
    #[serde(default, alias = "howMuchJoyDoesThisFileBringYou")]
    joyThisFileBrings: serde_json::Value,
    /// The personality emoji of the file, with a default value. This field can be aliased as `emojiThatExpressesThisFilesPersonality`.
    #[serde(default, alias = "emojiThatExpressesThisFilesPersonality")]
    personalityEmoji: String,
}

/// DirdocsRoot holds all the description docs in a directory.
#[derive(Debug, Deserialize)]
struct DirdocsRoot {
    /// Vec of child docs (each is a Node).
    entries: Vec<Node>,
}

/// Represents metadata about a file for documentation purposes.
#[derive(Debug, Default, Clone)]
struct FileDocInfo {
    /// The brief description of the file, typically from its README.
    description: String,
    /// The personality of the file, such as "Acutely Perceptive".
    personality: String,
    /// The joy associated with the file, such as "Enthusiastically".
    joy: String,
}

/// Represents raw data for a file or directory entry.
#[derive(Debug)]
struct RowRaw {
    /// The path to the file or directory.
    path: PathBuf,
    /// Human-readable name of the item (e.g. files, dirs).
    name: String,
    /// Identifier for the type (file or directory).
    ty: String,
    /// Size in bytes, as a string.
    size_h: String,
    /// Last modified time in human-readable format.
    modified_h: String,
    /// Detailed description of the item.
    description: String,
    /// Personality trait assigned to this item;
    personality: String,
    /// A measure of joy associated with this item;
    joy: String,
}

/// A theme for the "tree" view. This data structure encapsulates all styles and configuration options required to render a tree in the terminal.
#[derive(Clone)]
struct Theme {
    /// Content of the file or directory name, with a "/" prefix added.
    header: Style,
    /// Style for the directory (used as a default if no matching file found).
    dir: Style,
    /// Style for the file name.
    filesize: Style,
    /// Style for the date of the file or directory.
    date: Style,
    /// Style for the index of file or directory.
    index: Style,
    /// Whether to enable the theme; disabled by default.
    enabled: bool,
}

impl Theme {
    /// Handles enabling or disabling the default theme in a directory listing system.
    ///
    /// This function constructs an instance of `Theme` with styling applied to various
    /// elements, and sets the `enabled` flag based on input.
    ///
    /// Parameters:
    /// - `enabled`: A boolean indicating whether to enable the default theme.
    ///
    /// Returns:
    /// - `Self`: An instance of `Theme` with styling and state configured.
    ///
    /// Notes:
    /// - The styles for headers, directories, file sizes, dates, and indexes are
    ///   predefined using color codes from the `Color::` enum.
    /// - This function is intended to be used internally by other parts of the system
    ///   and should not be called directly.
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
/// Load and parse a `nu` theme configuration file.
///
/// This function executes a shell command to get color settings from an environment variable,
/// parses the resulting JSON, and constructs a `Theme` struct based on resolved values.
///
/// Parameters:
/// - None
///
/// Returns:
/// - `Some(Theme)` if the theme is successfully loaded and parsed.
/// - `None` on failure (e.g., if command fails or JSON parse error).
///
/// Errors:
/// - I/O errors from executing the `nu` command.
/// - JSON parsing errors.
///
/// Notes:
/// - Uses `serde_json` to parse the output of a shell command.
/// - Defaults to using environment variables and fallback strategies for missing keys.
///
/// References:
/// - `Command::new`
/// - `serde_json::from_slice
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

/// Handle conversion of JSON value to a `Style` object.
///
/// Parses a serde_json::Value into an optional Style object, using
/// custom formatting rules. Supports string and object formats.
///
/// Parameters:
/// - `v`: A reference to a serde_json::Value
///
/// Returns:
/// - An Option<Style> containing parsed style or None if parsing fails.
///
/// Errors:
/// - Returns None in case of invalid input format
///
/// Notes:
/// - The object format expects specific keys: "fg", "bg" for colors,
///   and "attr" for text attributes.
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

/// Parses a style string and returns an optional `Style` object.
///
/// This function takes a string input, trims it, and converts to lowercase
/// before attempting to parse either an RGB hex color or a style configuration
/// consisting of key-value pairs separated by underscores. It supports common
/// syntax such as "bold", "underline", "reverse", and color names or hex codes.
///
/// Parameters:
/// - `s`: A string slice representing the style configuration to parse.
///
/// Returns:
/// - `Option<Style>`: An optional `Style` object if parsing succeeds, or `None`
///   if the input is invalid or unsupported.
///
/// Errors:
/// - This function does not directly return errors but may panic if it encounters
///   invalid input or unsupported syntax. Use `?` operator in caller to handle panics.
///
/// Notes:
/// - The input string is case-insensitive due to `to_lowercase`.
/// - Supported style keywords include: bold, underline (u), reverse (r).
/// - Supported color values can be named or specified in hex format.
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
/// Parses a hexadecimal RGB color string and returns an Option<Color>.
///
/// This function checks if the input string starts with a `#` and contains exactly 6 hexadecimal characters.
/// If valid, it parses the red (`r`), green (`g`), and blue (`b`) values from the string using base-16 conversion.
/// Returns Some(Color::Rgb(r, g, b)) on success or None otherwise.
///
/// Parameters:
/// - `s`: A string slice representing the hexadecimal RGB color (e.g., "#RRGGBB").
///
/// Returns:
/// - Some(Color::Rgb(r, g, b)) if the input is a valid hexadecimal RGB string.
/// - None otherwise.
///
/// Errors:
/// - Returns Option::None if the input is not a valid hexadecimal RGB string.
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
/// Parse a color name and return the corresponding `Color` value.
/// This function supports named colors, RGB values, and special keywords like "dark_gray" or "light_red".
/// On failure to match a known color, it returns `None`.
///
/// Parameters:
/// - `name`: The name of the color to parse (e.g., "red", "cyan").
///
/// Returns:
/// - `Some(Color)` if the name matches a known color.
/// - `None` otherwise.
///
/// Notes:
/// - Supports named colors, RGB values, and special keywords.
/// - Uses `use Color::*` to simplify the match pattern.
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

/// Handle the main entry point for the `dls` command-line tool.
/// This function parses command-line arguments, locates a project root directory, and collects descriptions
/// from files in that directory. It supports recursive traversal of directories and prints a formatted table
/// of found descriptions based on the provided arguments. On success, it returns `Ok(())`.
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

/// Collects file and directory rows from a given directory, including metadata like size, modification time, and optional description.
///
/// Parameters:
/// - `dir`: The directory to scan.
/// - `project_root`: Optional root path for relative file paths (used in `rel_str`).
/// - `desc_map`: A map of file names to their description and metadata (from previous runs).
/// - `show_all`: Whether to include hidden files.
///
/// Returns:
/// A Vec of `RowRaw` objects containing file/dir info, or an error.
///
/// Errors:
/// - I/O errors during directory scanning or metadata retrieval.
/// - Errors from `rel_str` or `format_size`.
/// - Deserialization errors if no previous run data exists.
///
/// Notes:
/// - Hidden files are skipped unless `show_all` is true.
/// - The returned rows are sorted with files first, then dirs by name.
/// - `size_h` is formatted using `format_size`.
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

/// Handle and format a table of data for the `nu` command-line tool.
/// This function constructs and renders a formatted table with headers, rows of data,
/// custom styling via themes or colors (if enabled), and supports optional emoji-based
/// presentation for personality/joy attributes. It is used internally by `nu` to display
/// structured data in a terminal-friendly format.
///
///
/// Parameters:
/// - `rows`: A slice of raw row data to be displayed in the table.
/// - `fun`: A boolean flag indicating whether emoji-based personality/joy data should be included.
///
///
/// Returns:
/// - `()`: This function does not return a value; it prints the formatted table directly to stdout.
///
///
/// Errors:
/// - `()` (no return value), but errors may occur during table construction, styling, or rendering.
///   See the function body for details on potential error sources (e.g., terminal size, color settings).
///
/// Notes:
/// - The function builds a table with optional headers and rows, using either theme-based or color-based
///   styling for visual presentation.
/// - The `fun` parameter controls whether emoji representations of personality and joy are added to the table.
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

/// Checks if a file or directory is hidden by examining its name.
/// A path is considered hidden if it starts with a dot (`.`).
///
/// Parameters:
/// - `name`: A reference to an OS string representing the path.
///
/// Returns:
/// - `true` if the name starts with a dot, indicating a hidden file or directory.
/// - `false` otherwise.
fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Find the root of a project by searching for `.dirdocs.nuon` files.
///
/// This function starts at the given `start` path and recursively checks
/// parent directories for a file named `.dirdocs.nuon`. If found, it returns
/// the path to that directory. If no such file is found within a reasonable
/// range, it returns `None`.
///
/// Parameters:
/// - `start`: A reference to a path from which the search begins.
///
/// Returns:
/// - An `Option<PathBuf>` containing the path to the project root if
///   a `.dirdocs.nuon` file is found; otherwise `None`.
///
/// Errors:
/// - This function does not return explicit errors, but may panic
///   if `cur.parent()` returns `None` (e.g., when `cur` is `/`).
///
/// Notes:
/// - The search continues upwards through parent directories until
///   either a `.dirdocs.nuon` file is found or the root of the filesystem
///   is reached.
/// ```
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

/// Load descriptions from a directory structure and return them as `FileDocInfo` in a `HashMap`.
///
/// This function reads the `.dirdocs.nuon` file to parse a directory structure and extracts
/// descriptions, personality emojis, and joy fields from each file. It builds a mapping of file paths
/// to `FileDocInfo` objects, which contain the extracted metadata. If any required fields are empty,
/// the file is skipped.
///
/// # Parameters:
/// - `root`: The path to the directory where `.dirdocs.nuon` is located.
///
/// # Returns:
/// - A `HashMap<String, FileDocInfo>` containing the parsed descriptions.
///
/// # Errors:
/// - I/O errors when reading files or parsing JSON, and
///   any errors from `serde_json::from_str`.
///
/// # Notes:
/// - The `.dirdocs.nuon` file must be in the form of a JSON object with an `entries` field.
/// - Empty fields are ignored to ensure valid output.
fn load_descriptions(root: &Path) -> anyhow::Result<HashMap<String, FileDocInfo>> {
    let mut map: HashMap<String, FileDocInfo> = HashMap::new();
    let s = fs::read_to_string(root.join(".dirdocs.nuon"))?;
    let parsed: DirdocsRoot = serde_json::from_str(&s)?;

    /// Handle a JSON value and convert it into a compact string representation.
    ///
    /// Converts any `serde_json::Value` to a string, handling nulls by returning an empty
    /// string, strings by cloning their contents, and other types (numbers, booleans,
    /// arrays, objects) by calling `to_string()` on them.
    fn v_to_joy(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::Null => String::new(),
            serde_json::Value::String(s) => s.clone(),
            // numbers, bools, arrays, objects – compact string
            other => other.to_string(),
        }
    }

    /// Handle visiting nodes to populate file documentation info.
    ///
    /// This function recursively visits directory and file nodes, extracting
    /// descriptions, personality emojis, and joy metadata from each file.
    /// It builds a mapping between file paths and their documentation info,
    /// skipping any files with empty description, personality emoji, or joy data.
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

/// Handle relative path string comparison between `p` and `base`.
///
/// Computes the relative path from `base` to `p`, using the
/// `pathdiff::diff_paths` utility. If no relative path exists,
/// returns a copy of `p`. The result is converted to a lossy string.
///
/// Parameters:
/// - `p`: Path to resolve relative from `base`.
/// - `base`: Base path used for computing the relative path.
///
/// Returns:
/// A lossy UTF-8 string representing the relative path, or a copy of `p`
/// if no relative path exists.
///
/// Errors:
/// - No direct errors are returned; all failures propagate through `unwrap_or_else`.
///
/// Notes:
/// The result contains only ASCII if `p` or `base` contain non-ASCII UTF-8.
fn rel_str(p: &Path, base: &Path) -> String {
    pathdiff::diff_paths(p, base)
        .unwrap_or_else(|| p.to_path_buf())
        .to_string_lossy()
        .into()
}

/// Convert a string to its emoji presentation form.
///
/// This function checks if the input string is empty or contains the emoji modifier code point (`\u{FE0F}`). If so, it returns the string unchanged.
/// Otherwise, if the string contains exactly one codepoint (e.g., a single Unicode character), it appends the emoji modifier to force emoji presentation.
///
/// Parameters:
/// - `s`: The input string to be converted.
///
/// Returns:
/// A new `String` with emoji modifier applied if necessary.
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
