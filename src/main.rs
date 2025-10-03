mod cache;
mod chunk;
mod content;
mod prompt_llm;
mod types;

use crate::cache::{
    CHILD_CACHE_NAMES, find_child_cache_dirs, index_files_by_path, insert_file_into_tree,
    load_existing_tree, rebase_child_tree_into_existing_by_path, write_tree,
};
use crate::chunk::token_chunks_for_file;
use crate::content::{as_ms, file_meta, hash_file, is_probably_text, readme_context, truncate};
use crate::prompt_llm::{
    ModelResp, ask_with_retry, indent_for_yaml, render_chat_template, sanitize_description,
    sanitize_for_yaml, suppressed_block,
};
use crate::types::{DirdocsRoot, Doc, FileEntry};

use awful_aj::config::AwfulJadeConfig;
use chrono::Utc;
use clap::{Parser, Subcommand};
use handlebars::Handlebars;
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

/// Top-level CLI for `dirdocs`.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// `cmd` is the subcommand to execute.
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize Awful Jade config and dir_docs template in your user config directory.
    Init,
    /// Run documentation generation (this is the behavior you had before).
    Run(RunArgs),
}

/// Arguments for the `run` subcommand (previously your root CLI args).
#[derive(Parser, Debug, Clone)]
struct RunArgs {
    /// Root directory to start from.
    #[clap(long, short, default_value = ".")]
    directory: String,

    /// Extra directory names to ignore (repeat flag or comma list).
    /// e.g. --ignore target,node_modules --ignore dist
    #[clap(long, short = 'i', value_delimiter = ',')]
    ignore: Vec<String>,

    /// Force re-generate docs for every file, even if unchanged.
    #[clap(long, short = 'f')]
    force: bool,
}

/// User-provided data about the file, its type (e.g. text/html), and metadata.
#[derive(Serialize)]
struct TplData<'a> {
    /// User-provided data about the file, its type (e.g. text/html), and metadata.
    filename: String,
    /// Size of the file in bytes, e.g. "1,024 kb" or "3 MB".
    filesize: String,
    /// File type, e.g. "text "image".
    filetype: String,
    /// MIME type, e.g. "text/html".
    mimetype: String,
    /// Operating system the file was created on, e.g. "macOS".
    operating_system: String,
    /// Indicates if the project is documented (0 or 1).
    project_is_documented: String,
    /// Location of the project documentation, e.g. "./README.md".
    project_documentation: String,
    /// First chunk of file contents, e.g. the first three lines.
    chunk_one: String,
    /// Second chunk of file contents, e.g. the middle part.
    chunk_two: String,
    /// Third chunk of file contents, e.g. the last part.
    chunk_three: String,
    /// Additional keyed fields, e.g. metadata copied from the file.
    #[serde(flatten)]
    extra: BTreeMap<&'a str, String>,
}

const DEFAULT_CONFIG_YAML: &str = r#"api_key: 
api_base: http://localhost:1234/v1
model: jade_qwen3_4b_mlx
context_max_tokens: 32768
assistant_minimum_context_tokens: 2048
should_stream: false
stop_words:
- |2-

  <|im_start|>
- <|im_end|>
session_db_url: ""
session_name: default
"#;

const DEFAULT_DIR_DOCS_TEMPLATE: &str = r#"system_prompt: You are Jade, created by Awful Security.
messages: []

pre_user_message_content: |
  The following text is a representation of a file. I would like to document this file.

  # Absolute path of file
  {{filename}}

  # Size of file
  {{filesize}}

  # Type of file
  {{filetype}}

  # MIME type of file
  {{mimetype}}

  # Operating System containing the file
  {{operating_system}}

  # Is the file a part of a project with documentation?
  {{project_is_documented}}

  # First 500 tokens of the README that documents the project this file belongs to
  {{project_documentation}}

  # First 500 tokens of file
  {{chunk_one}}

  # 500 tokens from the middle of the file
  {{chunk_two}}

  # 500 tokens from the end of the file
  {{chunk_three}}

  Please provide a terse, one sentence, 60 character description of what exactly purpose this file serves.
  Do not describe its functionality, only describe its purpose.
  If the file contains source code please review the logic to determine what exactly this file serves in the process that runs it.
  If the file is a configuration file please consider the what this file configures and label it as a configuration file.

  For safety, please strictly adhere to the the guidlines and rules.

  For fun, please rate this file on the joy it brings you with a single digit integer in the range of 1 to 10.
  If the file is source code you should rank the file on its readability and beginner friendliness. If the
  file is prose you should rank the prose on its stylistic beauty. If the file is configuration, rate it
  on its ease of comprehension.
  Include and emoji that expresses this file's distinct personality 🐘!

  # File Description Rules
  1. The description must be grammatically correct and begin with a capital letter.
  2. The description must be declaritive.
  3. The description must sound authorative.
  4. The desciption must start with a verb.
  5. **NEVER BEGIN THE DESCRIPTION WITH THE WORD "This".**

  # Forbidden Phrases
  1. "This file",
  2. the exact filename "{{filename}}", and its stem.


post_user_message_content: |
  /nothink

response_format:
  name: directory_documentation
  strict: true
  description: Represents a one sentence description of a file.
  schema:
    type: object
    properties:
      fileDescription:
        type: string
        minLength: 16
      joyThisFileBrings:
        type: integer
        enum: [1,2,3,4,5,6,7,8,9,10]
      personalityEmoji:
        type: string
    required:
      - fileDescription
      - joyThisFileBrings
      - personalityEmoji
    additionalProperties: false
"#;

/// Check if a file exists; create its directory if needed and write contents if missing.
///
/// Parameters:
/// - `path`: The path to the file or directory.
/// - `contents`: Optional string content (if provided, it will be written to the file).
///
/// Returns:
/// - `true` if the file was created and written; otherwise, `false`.
///
/// Errors:
/// - Returns I/O errors when creating directories or writing files.
///   - Specifically: `std::fs::Error` and `std::io::Error`.
///
/// Notes:
/// - The function checks if the file already exists. If it does, `false` is returned.
/// - If the directory of the file does not exist, it will be created with `create_dir_all`.
fn write_if_missing(path: &std::path::Path, contents: &str) -> anyhow::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(true)
}

/// Initialize and run the Awful Jade application.
///
/// This function sets up logging, parses command-line arguments, and executes
/// either the `init` or `run` subcommand depending on user input.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // tracing init
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    match args.cmd {
        Command::Init => cmd_init(),
        Command::Run(run_args) => cmd_run(run_args).await,
    }
}

/// Initialize Awful Jade's configuration and templates.
///
/// Ensures the existence of `config.yaml` in a user-defined config directory
/// and inserts a default documentation template. If files don't exist, they're
/// created by copying the provided defaults.
///
/// # Parameters:
/// - None: This function has no parameters.
///
/// # Returns:
/// - `anyhow::Result<()>`: Always succeeds with a unit value.
///
/// # Errors:
/// - Returns I/O errors when creating or reading files,
/// - yaml parsing errors if the config file is invalid.
fn cmd_init() -> anyhow::Result<()> {
    // Find the user config dir for Awful Jade
    let config_dir =
        awful_aj::config_dir().map_err(|e| anyhow::anyhow!("config_dir() failed: {e}"))?;
    let config_file = config_dir.join("config.yaml");
    let templates_dir = config_dir.join("templates");
    let template_file = templates_dir.join("dir_docs.yaml");

    info!(path=%config_file.display(), "Ensuring config.yaml exists");
    let wrote_cfg = write_if_missing(&config_file, DEFAULT_CONFIG_YAML)?;
    if wrote_cfg {
        info!("Created {}", config_file.display());
    } else {
        info!("Already exists: {}", config_file.display());
    }

    info!(path=%template_file.display(), "Ensuring templates/dir_docs.yaml exists");
    let wrote_tpl = write_if_missing(&template_file, DEFAULT_DIR_DOCS_TEMPLATE)?;
    if wrote_tpl {
        info!("Created {}", template_file.display());
    } else {
        info!("Already exists: {}", template_file.display());
    }

    println!("✅ dirdocs init complete");
    println!("  config:   {}", config_file.display());
    println!("  template: {}", template_file.display());
    Ok(())
}

/// Handle the `cmd_run` subcommand.
///
/// Loads and processes directory documentation files, using a template to generate structured content.
/// It parses configuration, reads file metadata, and uses the `handlebars` templating engine to render the prompt.
///
/// # Parameters:
/// - `args`: A `RunArgs` struct containing command-line arguments, such as directory path and ignore patterns.
///
/// # Returns:
/// - `anyhow::Result<()>`, indicating success or an error during execution.
///
/// # Errors:
/// - I/O errors when reading/writing files,
/// - YAML/JSON parsing errors during template rendering or configuration loading,
/// - Errors from `handlebars` operations,
/// - Any error returned by the underlying API calls.
///
/// # Notes:
/// - This function uses `canonialize()` to resolve paths and `pathdiff` for relative path differences.
/// - It lazily loads configuration files, allowing optional error handling during config parsing.
/// - Binary files are handled with safe placeholders instead of actual content.
async fn cmd_run(args: RunArgs) -> anyhow::Result<()> {
    info!(?args, "dir_docs starting");

    let root = PathBuf::from(&args.directory)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&args.directory));
    info!(root=%root.display(), "Resolved root");

    // label root relative to CWD
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let rel_root_path = pathdiff::diff_paths(&root, &cwd).unwrap_or_else(|| PathBuf::from("."));
    let root_label = {
        let s = rel_root_path.to_string_lossy();
        if s.is_empty() {
            ".".to_string()
        } else {
            s.to_string()
        }
    };

    // AJ config
    let config_dir =
        awful_aj::config_dir().map_err(|e| anyhow::anyhow!("config_dir() failed: {e}"))?;
    let config_file = config_dir.join("config.yaml");
    info!(config=%config_file.display(), "Loading Awful Jade config");
    let cfg: AwfulJadeConfig = awful_aj::config::load_config(&config_file.to_string_lossy())
        .map_err(|e| {
            anyhow::anyhow!("failed to load Awful Jade config at {:?}: {e}", config_file)
        })?;

    // Load dir_docs template
    let tpl_path = awful_aj::config_dir()
        .map_err(|e| anyhow::anyhow!("config_dir() failed: {e}"))?
        .join("templates")
        .join("dir_docs.yaml");
    info!(template=%tpl_path.display(), "Reading dir_docs template");
    let raw_template = fs::read_to_string(&tpl_path)
        .map_err(|e| anyhow::anyhow!("failed to read template {:?}: {e}", tpl_path))?;
    debug!(template_size_bytes = raw_template.len(), "Template loaded");

    // README context
    let (project_is_documented, project_doc_snippet) = readme_context(&root)?;
    debug!(project_is_documented=%project_is_documented, doc_snippet_len=project_doc_snippet.len(), "README context collected");

    // Existing .dirdocs.nuon
    let dirdocs_path = root.join(".dirdocs.nuon");
    info!(path=%dirdocs_path.display(), "Loading existing .dirdocs.nuon (if any)");
    let existing_tree = load_existing_tree(&dirdocs_path, &root, &cwd);

    // For quick lookups when merging
    let mut existing_by_path: HashMap<String, FileEntry> = HashMap::new();
    index_files_by_path(&existing_tree.entries, &mut existing_by_path);
    info!(
        existing_files = existing_by_path.len(),
        "Indexed existing files"
    );

    // Merge child caches so we can skip clean files in subtrees
    let child_cache_dirs = find_child_cache_dirs(&root);
    info!(count = child_cache_dirs.len(), "Child caches found");
    for child_abs in &child_cache_dirs {
        if let Some(cache_path) = CHILD_CACHE_NAMES
            .iter()
            .map(|n| child_abs.join(n))
            .find(|p| p.exists())
        {
            let child_tree = load_existing_tree(&cache_path, child_abs, &cwd);
            let before = existing_by_path.len();
            rebase_child_tree_into_existing_by_path(
                child_abs,
                &root,
                &child_tree,
                &mut existing_by_path,
            );
            info!(child=%child_abs.display(), added = existing_by_path.len() as i64 - before as i64, "Merged child cache into existing_by_path");
        } else {
            warn!(child=%child_abs.display(), "Cache file missing; skipping merge");
        }
    }

    // Walker
    let ignore_set: HashSet<String> = args.ignore.clone().into_iter().collect();
    info!(?ignore_set, "Initializing walker (git + hidden rules)");
    let mut builder = WalkBuilder::new(&root);
    builder
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .hidden(true);
    builder.filter_entry(move |e| {
        if e.depth() == 0 {
            return true;
        }
        if let Some(ft) = e.file_type() {
            if ft.is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    if ignore_set.contains(name) {
                        return false;
                    }
                }
            }
        }
        true
    });

    let walker = builder.build();
    let hbs = Handlebars::new();

    // Collect flat new/updated file map (path -> FileEntry)
    let mut updated_files: HashMap<String, FileEntry> = HashMap::new();

    let mut walked = 0usize;
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                warn!(%err, "Walk error");
                continue;
            }
        };
        if entry.depth() == 0 || !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        walked += 1;

        let path = entry.path();
        let rel_path = pathdiff::diff_paths(path, &root).unwrap_or_else(|| path.to_path_buf());
        let rel_str = rel_path.to_string_lossy().to_string();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let _span = tracing::info_span!("process_file", rel=%rel_str, name=%name).entered();

        // Hash file to detect dirtiness
        let file_hash = match hash_file(path) {
            Ok(h) => h,
            Err(e) => {
                warn!(%e, path=%path.display(), "Hash failed; skipping");
                continue;
            }
        };
        debug!(hash=%file_hash, "File hashed");

        // Cache reuse (unless --force)
        if let Some(prev) = existing_by_path.get(&rel_str) {
            if !args.force && prev.hash == file_hash && !prev.doc.fileDescription.is_empty() {
                info!("Reusing previous doc (clean)");
                updated_files.insert(
                    rel_str.clone(),
                    FileEntry {
                        name: name.clone(),
                        path: rel_str.clone(),
                        hash: file_hash.clone(),
                        updated_at: prev.updated_at,
                        doc: prev.doc.clone(),
                    },
                );
                continue;
            } else if args.force {
                info!("Forcing regeneration (--force)");
            } else {
                info!("Changed content detected; regenerating");
            }
        } else {
            info!("New file; generating");
        }

        // Otherwise (new or dirty), render template and ask the model
        let (filesize, filetype, mimetype) = file_meta(path);
        let is_text = is_probably_text(path, 4096);

        // For text: chunk as before; for binary: use safe placeholders.
        let (chunk1_raw, chunk2_raw, chunk3_raw, used_splitter) = if is_text {
            token_chunks_for_file(path, &mimetype, 1000).unwrap_or_default()
        } else {
            (
                suppressed_block(),
                suppressed_block(),
                suppressed_block(),
                "binary".to_string(),
            )
        };

        debug!(
            filesize=%filesize, filetype=%filetype, mimetype=%mimetype, used_splitter=%used_splitter,
            chunk1_len=chunk1_raw.len(), chunk2_len=chunk2_raw.len(), chunk3_len=chunk3_raw.len(),
            "Collected file metadata and token-aware chunks"
        );

        // Regex tripwires for filename/stem (optional)
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let mut extra = BTreeMap::new();
        extra.insert("filename_re", regex::escape(fname));
        extra.insert("filename_stem_re", regex::escape(stem));

        // sanitize *everything* you inject
        let project_doc_snippet_s = sanitize_for_yaml(&project_doc_snippet);
        let chunk1_s = sanitize_for_yaml(&chunk1_raw);
        let chunk2_s = sanitize_for_yaml(&chunk2_raw);
        let chunk3_s = sanitize_for_yaml(&chunk3_raw);

        // then indent
        let project_doc_snippet_ind = indent_for_yaml(&project_doc_snippet_s, 2);
        let chunk1_ind = indent_for_yaml(&chunk1_s, 2);
        let chunk2_ind = indent_for_yaml(&chunk2_s, 2);
        let chunk3_ind = indent_for_yaml(&chunk3_s, 2);

        let data = TplData {
            filename: path.display().to_string(),
            filesize,
            filetype,
            mimetype,
            operating_system: std::env::consts::OS.to_string(),
            project_is_documented: project_is_documented.clone(),
            project_documentation: project_doc_snippet_ind,
            chunk_one: chunk1_ind,
            chunk_two: chunk2_ind,
            chunk_three: chunk3_ind,
            extra,
        };

        // Render → ChatTemplate (with error preview)
        let tpl = match render_chat_template(&hbs, &raw_template, &data) {
            Ok(t) => t,
            Err(e) => {
                error!(%e, file=%path.display(), "Template/YAML error");
                continue;
            }
        };

        let updated_at = Utc::now();

        // Timed API call (with backoff)
        let t0 = Instant::now();
        let answer = match ask_with_retry(&cfg, "", &tpl, 5).await {
            Ok(ans) => {
                info!(elapsed_ms = %as_ms(t0.elapsed()), "api::ask finished");
                ans
            }
            Err(e) => {
                error!(%e, elapsed_ms = %as_ms(t0.elapsed()), file=%path.display(), "api::ask failed after retries");
                String::new()
            }
        };

        let doc: Option<Doc> = if answer.is_empty() {
            None
        } else {
            match serde_json::from_str::<ModelResp>(&answer) {
                Ok(r) => {
                    let cleaned = sanitize_description(&r.fileDescription);
                    Some(Doc {
                        fileDescription: cleaned,
                        joyThisFileBrings: r.joyThisFileBrings,
                        personalityEmoji: r.personalityEmoji,
                    })
                }
                Err(e) => {
                    error!(%e, raw_preview=%truncate(&answer, 400), "Response JSON parse error");
                    None
                }
            }
        };

        let file_entry = FileEntry {
            name,
            path: rel_str.clone(),
            hash: file_hash,
            updated_at,
            doc: doc.unwrap_or_default(),
        };

        updated_files.insert(rel_str, file_entry);
    }

    info!(
        walked,
        updated_count = updated_files.len(),
        "Walking complete"
    );

    // Build a new tree from updated_files
    let mut new_root = DirdocsRoot {
        root: root_label,
        updated_at: Utc::now(),
        entries: Vec::new(),
    };

    for (rel_path, fe) in &updated_files {
        insert_file_into_tree(&mut new_root.entries, rel_path, fe);
    }

    // Write as strict JSON (Nuon-compatible)
    let dirdocs_path = root.join(".dirdocs.nuon");
    write_tree(&dirdocs_path, &new_root)?;

    info!("Done");
    Ok(())
}
