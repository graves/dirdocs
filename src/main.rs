use awful_aj::config::AwfulJadeConfig;
use awful_aj::{api, template::ChatTemplate};
use blake3::Hasher;
use chrono::{DateTime, Utc};
use clap::Parser;
use handlebars::Handlebars;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_yaml as yaml;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;
use tokio::time::{Duration, sleep};

use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use text_splitter::{ChunkConfig, CodeSplitter, MarkdownSplitter, TextSplitter};
use tiktoken_rs::cl100k_base;

use tree_sitter::Language;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Root directory to start from
    #[clap(long, short, default_value = ".")]
    directory: String,

    /// Extra directory names to ignore (repeat flag or comma list)
    /// e.g. --ignore target,node_modules --ignore dist
    #[clap(long, short = 'i', value_delimiter = ',')]
    ignore: Vec<String>,

    /// Force re-generate docs for every file, even if unchanged
    #[clap(long, short = 'f')]
    force: bool,
}

#[derive(Serialize)]
struct TplData<'a> {
    filename: String,
    filesize: String,
    filetype: String,
    mimetype: String,
    operating_system: String,
    project_is_documented: String,
    project_documentation: String,
    chunk_one: String,
    chunk_two: String,
    chunk_three: String,
    #[serde(flatten)]
    extra: BTreeMap<&'a str, String>,
}

/* ---------------- Directory doc file schema (Nuon/JSON) ---------------- */

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DirEntry {
    name: String,
    path: String,
    updated_at: DateTime<Utc>,
    entries: Vec<Node>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FileEntry {
    name: String,
    path: String,
    hash: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    doc: Doc, // the model response
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct Doc {
    fileDescription: String,

    // Prefer short key when writing; accept long legacy key when reading.
    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    joyThisFileBrings: serde_json::Value,

    // Prefer short key when writing; accept long legacy key when reading.
    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    personalityEmoji: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DirdocsRoot {
    // version removed per request
    root: String,
    updated_at: DateTime<Utc>,
    entries: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct ModelResp {
    fileDescription: String,

    // Accept both new and old names from the model
    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    joyThisFileBrings: serde_json::Value,

    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    personalityEmoji: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- tracing init ---
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    info!(?args, "dirdocs starting");

    let root = PathBuf::from(&args.directory)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&args.directory));
    info!(root=%root.display(), "Resolved root");

    // Compute label for root as relative to current working directory
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

    // Load dirdocs template
    let tpl_path = awful_aj::config_dir()
        .map_err(|e| anyhow::anyhow!("config_dir() failed: {e}"))?
        .join("templates")
        .join("dirdocs.yaml");
    info!(template=%tpl_path.display(), "Reading dirdocs template");
    let raw_template = fs::read_to_string(&tpl_path)
        .map_err(|e| anyhow::anyhow!("failed to read template {:?}: {e}", tpl_path))?;
    debug!(template_size_bytes = raw_template.len(), "Template loaded");

    // README context
    let (project_is_documented, project_doc_snippet) = readme_context(&root)?;
    debug!(
        project_is_documented=%project_is_documented,
        doc_snippet_len=project_doc_snippet.len(),
        "README context collected"
    );

    // Existing .dirdocs.nu (JSON/NUON)
    let dirdocs_path = root.join(".dirdocs.nu");
    info!(path=%dirdocs_path.display(), "Loading existing .dirdocs.nu (if any)");
    let existing_tree = load_existing_tree(&dirdocs_path, &root, &cwd);

    // For quick lookups when merging
    let mut existing_by_path: HashMap<String, FileEntry> = HashMap::new();
    index_files_by_path(&existing_tree.entries, &mut existing_by_path);
    info!(
        existing_files = existing_by_path.len(),
        "Indexed existing files"
    );

    // ---- NEW: Merge child caches (rebased) so we can skip clean files in subtrees
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
            info!(
                child=%child_abs.display(),
                added = existing_by_path.len() as i64 - before as i64,
                "Merged child cache into existing_by_path"
            );
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
                        updated_at: prev.updated_at, // keep prior timestamp
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
            filesize=%filesize,
            filetype=%filetype,
            mimetype=%mimetype,
            used_splitter=%used_splitter,
            chunk1_len=chunk1_raw.len(),
            chunk2_len=chunk2_raw.len(),
            chunk3_len=chunk3_raw.len(),
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
            project_documentation: project_doc_snippet_ind, // <— indented
            chunk_one: chunk1_ind,                          // <— indented
            chunk_two: chunk2_ind,                          // <— indented
            chunk_three: chunk3_ind,                        // <— indented
            extra,
        };

        let rendered_yaml = match hbs.render_template(&raw_template, &data) {
            Ok(s) => {
                debug!(yaml_size=s.len(), yaml_preview=%truncate(&s, 400), "Template rendered");
                s
            }
            Err(e) => {
                error!(%e, file=%path.display(), "Template render error");
                continue;
            }
        };

        let tpl: ChatTemplate = match yaml::from_str(&rendered_yaml) {
            Ok(t) => {
                debug!(tpl_preview=?TruncDebug(&t, 5), "Parsed ChatTemplate");
                t
            }
            Err(e) => {
                error!(%e, file=%path.display(), "YAML -> ChatTemplate error");
                continue;
            }
        };

        let updated_at = Utc::now();

        // ---- Timed API call (with backoff) ----
        let t0 = Instant::now();
        let answer = match ask_with_retry(&cfg, "".to_string(), &tpl, 5).await {
            Ok(ans) => {
                let elapsed = t0.elapsed();
                info!(elapsed_ms = %as_ms(elapsed), "api::ask finished");
                ans
            }
            Err(e) => {
                let elapsed = t0.elapsed();
                error!(%e, elapsed_ms = %as_ms(elapsed), file=%path.display(), "api::ask failed after retries");
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
        root: root_label, // <- relative (e.g., ".")
        updated_at: Utc::now(),
        entries: Vec::new(),
    };

    for (rel_path, fe) in &updated_files {
        insert_file_into_tree(&mut new_root.entries, rel_path, fe);
    }

    // Write as strict JSON (Nuon-compatible)
    match fs::write(
        &dirdocs_path,
        serde_json::to_string_pretty(&new_root)? + "\n",
    ) {
        Ok(()) => info!(path=%dirdocs_path.display(), "Wrote .dirdocs.nu"),
        Err(e) => error!(%e, path=%dirdocs_path.display(), "Failed to write .dirdocs.nu"),
    }

    info!("Done");
    Ok(())
}

#[derive(Debug, Clone)]
enum SplitterKind {
    Code(Language),
    Markdown,
    Text,
}

/// Returns (start, middle, end, used_splitter_name)
fn token_chunks_for_file(
    path: &Path,
    mimetype: &str,
    max_tokens: usize,
) -> Option<(String, String, String, String)> {
    let text = read_text_lossy_limited(path, 2_000_000);
    if text.trim().is_empty() {
        return Some((String::new(), String::new(), String::new(), "empty".into()));
    }

    // Tokenizer for OpenAI-style models (tiktoken-rs 0.7)
    let bpe = cl100k_base().ok()?;
    // Pass a reference so it satisfies ChunkSizer
    let cfg = ChunkConfig::new(max_tokens).with_sizer(&bpe);

    let kind = guess_splitter(mimetype, path);

    // Unify each arm to the same concrete type
    let (chunks, used): (Vec<&str>, String) = match kind {
        SplitterKind::Code(lang) => {
            let splitter = CodeSplitter::new(lang, cfg).expect("valid tree-sitter language");
            (splitter.chunks(&text).collect(), "code".to_string())
        }
        SplitterKind::Markdown => {
            let splitter = MarkdownSplitter::new(cfg);
            (splitter.chunks(&text).collect(), "markdown".to_string())
        }
        SplitterKind::Text => {
            let splitter = TextSplitter::new(cfg);
            (splitter.chunks(&text).collect(), "text".to_string())
        }
    };

    if chunks.is_empty() {
        return Some((String::new(), String::new(), String::new(), used));
    }

    // Choose first / middle / last and own them
    let first = chunks.first().copied().unwrap_or_default().to_owned();
    let mid = chunks
        .get(chunks.len() / 2)
        .copied()
        .unwrap_or_else(|| chunks[0])
        .to_owned();
    let last = chunks
        .last()
        .copied()
        .unwrap_or_else(|| chunks[0])
        .to_owned();

    Some((first, mid, last, used))
}

fn guess_splitter(mime: &str, path: &Path) -> SplitterKind {
    // Prefer Markdown if clearly markdown
    if mime.to_lowercase().contains("markdown")
        || matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
                .as_deref(),
            Some("md" | "markdown" | "mdx")
        )
    {
        return SplitterKind::Markdown;
    }

    // Try to resolve a Tree-sitter code language; fall back to Text
    if let Some(lang) = guess_tree_sitter_language(mime, path) {
        return SplitterKind::Code(lang);
    }

    SplitterKind::Text
}

/// Map MIME/extension → Tree-sitter Language if the corresponding feature (and crate) is enabled.
fn guess_tree_sitter_language(mime: &str, path: &Path) -> Option<Language> {
    let m = mime.to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    macro_rules! ext_is {
        ($($lit:literal),* $(,)?) => {{
            if let Some(ref e) = ext {
                matches!(e.as_str(), $( $lit )|*)
            } else {
                false
            }
        }};
    }

    // Bash / Shell
    #[cfg(feature = "lang-bash")]
    if m.contains("shell") || m.contains("bash") || ext_is!("sh", "bash", "zsh") {
        return Some(tree_sitter_bash::LANGUAGE.into());
    }

    // C
    #[cfg(feature = "lang-c")]
    if m.contains("text/x-c") || m.contains("c;") || ext_is!("c", "h") {
        return Some(tree_sitter_c::LANGUAGE.into());
    }

    // C++
    #[cfg(feature = "lang-cpp")]
    if m.contains("c++") || m.contains("x-c++") || ext_is!("cpp", "cxx", "cc", "hpp", "hxx", "hh") {
        return Some(tree_sitter_cpp::LANGUAGE.into());
    }

    // C#
    #[cfg(feature = "lang-c-sharp")]
    if m.contains("csharp") || ext_is!("cs") {
        return Some(tree_sitter_c_sharp::LANGUAGE.into());
    }

    // CSS
    #[cfg(feature = "lang-css")]
    if m.contains("css") || ext_is!("css") {
        return Some(tree_sitter_css::LANGUAGE.into());
    }

    // ERB / EJS (embedded templates)
    #[cfg(feature = "lang-embedded-template")]
    if ext_is!("erb", "ejs") || m.contains("erb") || m.contains("ejs") {
        return Some(tree_sitter_embedded_template::LANGUAGE.into());
    }

    // Go
    #[cfg(feature = "lang-go")]
    if m.contains("golang") || m.contains("go") || ext_is!("go") {
        return Some(tree_sitter_go::LANGUAGE.into());
    }

    // Haskell
    #[cfg(feature = "lang-haskell")]
    if m.contains("haskell") || ext_is!("hs") {
        return Some(tree_sitter_haskell::LANGUAGE.into());
    }

    // HTML
    #[cfg(feature = "lang-html")]
    if m.contains("html") || ext_is!("html", "htm") {
        return Some(tree_sitter_html::LANGUAGE.into());
    }

    // Java
    #[cfg(feature = "lang-java")]
    if m.contains("java") || ext_is!("java") {
        return Some(tree_sitter_java::LANGUAGE.into());
    }

    // JavaScript
    #[cfg(feature = "lang-javascript")]
    if m.contains("javascript") || m.contains("ecmascript") || ext_is!("js", "mjs", "cjs") {
        return Some(tree_sitter_javascript::LANGUAGE.into());
    }

    // JSDoc
    #[cfg(feature = "lang-jsdoc")]
    if m.contains("jsdoc") || ext_is!("jsdoc") {
        return Some(tree_sitter_jsdoc::LANGUAGE.into());
    }

    // JSON
    #[cfg(feature = "lang-json")]
    if m.contains("json") || ext_is!("json") {
        return Some(tree_sitter_json::LANGUAGE.into());
    }

    // Julia
    #[cfg(feature = "lang-julia")]
    if m.contains("julia") || ext_is!("jl") {
        return Some(tree_sitter_julia::LANGUAGE.into());
    }

    // OCaml
    #[cfg(feature = "lang-ocaml")]
    if m.contains("ocaml") || ext_is!("ml", "mli") {
        // NOTE: tree-sitter-ocaml exports multiple languages.
        return Some(tree_sitter_ocaml::LANGUAGE_OCAML.into());
    }

    // PHP
    #[cfg(feature = "lang-php")]
    if m.contains("php") || ext_is!("php", "phtml") {
        return Some(tree_sitter_php::LANGUAGE_PHP.into());
    }

    // Python
    #[cfg(feature = "lang-python")]
    if m.contains("python") || ext_is!("py") {
        return Some(tree_sitter_python::LANGUAGE.into());
    }

    // Regex
    #[cfg(feature = "lang-regex")]
    if m.contains("regex") || ext_is!("re", "regex") {
        return Some(tree_sitter_regex::LANGUAGE.into());
    }

    // Ruby
    #[cfg(feature = "lang-ruby")]
    if m.contains("ruby") || ext_is!("rb", "rake", "gemspec") {
        return Some(tree_sitter_ruby::LANGUAGE.into());
    }

    // Rust
    #[cfg(feature = "lang-rust")]
    if m.contains("rust") || ext_is!("rs") {
        return Some(tree_sitter_rust::LANGUAGE.into());
    }

    // Scala
    #[cfg(feature = "lang-scala")]
    if m.contains("scala") || ext_is!("scala", "sc") {
        return Some(tree_sitter_scala::LANGUAGE.into());
    }

    // TypeScript (+ TSX)
    #[cfg(feature = "lang-typescript")]
    if m.contains("typescript") || ext_is!("ts", "tsx") {
        return Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into());
    }

    // Verilog
    #[cfg(feature = "lang-verilog")]
    if m.contains("verilog") || ext_is!("v", "vh", "sv", "svh") {
        return Some(tree_sitter_verilog::LANGUAGE.into());
    }

    None
}

fn readme_context(root: &Path) -> anyhow::Result<(String, String)> {
    let readme = [
        "README.md",
        "README.txt",
        "README",
        "Readme.md",
        "readme.md",
    ]
    .iter()
    .map(|n| root.join(n))
    .find(|p| p.exists() && p.is_file());

    if let Some(rp) = readme {
        let txt = read_text_lossy_limited(&rp, 2_000_000);
        let snippet = first_n_words(&txt, 500);
        Ok(("true".into(), snippet))
    } else {
        Ok(("false".into(), String::new()))
    }
}

fn file_meta(path: &Path) -> (String, String, String) {
    let md = fs::metadata(path);
    let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
    let filesize = human_bytes(size);

    let filetype = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into());

    let mime_guess = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    let mimetype = tree_magic_mini::from_filepath(path)
        .unwrap_or(mime_guess)
        .to_string();

    (filesize, filetype, mimetype)
}

fn read_text_lossy_limited(path: &Path, max_bytes: usize) -> String {
    match fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = Vec::with_capacity(max_bytes.min(1_000_000));
            let mut rdr = io::BufReader::new(&mut f);
            match io::Read::take(&mut rdr, max_bytes as u64).read_to_end(&mut buf) {
                Ok(_) => String::from_utf8_lossy(&buf).to_string(),
                Err(_) => String::new(),
            }
        }
        Err(_) => String::new(),
    }
}

fn first_n_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut val = b as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{b} {}", UNITS[idx])
    } else {
        format!("{:.1} {}", val, UNITS[idx])
    }
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(&mut file);
    let mut hasher = Hasher::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/* ---- Load/merge .dirdocs.nu (as JSON/NUON) ---- */

fn load_existing_tree(path: &Path, root_abs: &Path, cwd: &Path) -> DirdocsRoot {
    match fs::read_to_string(path) {
        Ok(s) => match serde_json::from_str::<DirdocsRoot>(&s) {
            Ok(tree) => tree,
            Err(_) => DirdocsRoot {
                root: rel_label(root_abs, cwd),
                updated_at: Utc::now(),
                entries: Vec::new(),
            },
        },
        Err(_) => DirdocsRoot {
            root: rel_label(root_abs, cwd),
            updated_at: Utc::now(),
            entries: Vec::new(),
        },
    }
}

fn rel_label(root_abs: &Path, cwd: &Path) -> String {
    let rel = pathdiff::diff_paths(root_abs, cwd).unwrap_or_else(|| PathBuf::from("."));
    let s = rel.to_string_lossy();
    if s.is_empty() {
        ".".to_string()
    } else {
        s.to_string()
    }
}

fn index_files_by_path(nodes: &[Node], map: &mut HashMap<String, FileEntry>) {
    for n in nodes {
        match n {
            Node::Dir(d) => index_files_by_path(&d.entries, map),
            Node::File(f) => {
                map.insert(f.path.clone(), f.clone());
            }
        }
    }
}

const CHILD_CACHE_NAMES: &[&str] = &[".dirdocs.nu", ".dir.nuon"];

/// Recursively find directories under `parent_root` that contain a child cache file.
/// If a directory has a cache, it is recorded and not descended further.
fn find_child_cache_dirs(parent_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![parent_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut has_cache = false;
        let rd = match fs::read_dir(&dir) {
            Ok(x) => x,
            Err(_) => continue,
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_file() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if CHILD_CACHE_NAMES
                        .iter()
                        .any(|&n| n.eq_ignore_ascii_case(name))
                    {
                        has_cache = true;
                        break;
                    }
                }
            }
        }

        if has_cache {
            match dir.canonicalize() {
                Ok(abs) => out.push(abs),
                Err(_) => out.push(dir.clone()),
            }
            // do not descend this directory
            continue;
        }

        // descend
        let rd = match fs::read_dir(&dir) {
            Ok(x) => x,
            Err(_) => continue,
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

/// Rebase a child tree into the parent's namespace and merge into `existing_by_path`.
fn rebase_child_tree_into_existing_by_path(
    child_root_abs: &Path,
    parent_root_abs: &Path,
    tree: &DirdocsRoot,
    map: &mut HashMap<String, FileEntry>,
) {
    let base_rel =
        pathdiff::diff_paths(child_root_abs, parent_root_abs).unwrap_or_else(|| PathBuf::from("."));

    fn walk(nodes: &[Node], base_rel: &Path, map: &mut HashMap<String, FileEntry>) {
        for n in nodes {
            match n {
                Node::Dir(d) => walk(&d.entries, base_rel, map),
                Node::File(f) => {
                    let mut fe = f.clone();
                    let rebased = base_rel.join(&f.path).to_string_lossy().to_string();
                    fe.path = rebased.clone();
                    map.insert(rebased, fe);
                }
            }
        }
    }

    walk(&tree.entries, &base_rel, map);
}

/* ---- Build nested tree from path parts ---- */

fn insert_file_into_tree(entries: &mut Vec<Node>, rel_path: &str, fe: &FileEntry) {
    let mut comps: Vec<String> = Vec::new();
    for c in Path::new(rel_path).components() {
        if let Component::Normal(os) = c {
            if let Some(s) = os.to_str() {
                comps.push(s.to_string());
            }
        }
    }
    if comps.is_empty() {
        return;
    }

    insert_recursive(entries, &comps, fe);
}

fn insert_recursive(entries: &mut Vec<Node>, comps: &[String], fe: &FileEntry) {
    if comps.len() == 1 {
        // leaf file
        let file = FileEntry {
            name: comps[0].clone(),
            path: fe.path.clone(),
            hash: fe.hash.clone(),
            updated_at: fe.updated_at,
            doc: fe.doc.clone(),
        };
        entries.push(Node::File(file));
        return;
    }

    // directory component
    let dir_name = &comps[0];
    if let Some(Node::Dir(dir)) = entries.iter_mut().find(|n| match n {
        Node::Dir(d) => d.name == *dir_name,
        _ => false,
    }) {
        insert_recursive(&mut dir.entries, &comps[1..], fe);
        // bump dir timestamp to reflect subtree change
        dir.updated_at = Utc::now();
    } else {
        let mut new_dir = DirEntry {
            name: dir_name.clone(),
            path: comps[..1].join("/"),
            updated_at: Utc::now(),
            entries: Vec::new(),
        };
        insert_recursive(&mut new_dir.entries, &comps[1..], fe);
        entries.push(Node::Dir(new_dir));
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}… ({} bytes total)", &s[..max], s.len())
    }
}

fn as_ms(d: std::time::Duration) -> u128 {
    d.as_millis()
}

fn indent_for_yaml(s: &str, n: usize) -> String {
    if s.is_empty() {
        return String::new();
    }
    let pad = " ".repeat(n);
    s.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sanitize_description(input: &str) -> String {
    use regex::Regex;

    // Trim whitespace and surrounding quotes
    let mut s = input.trim().trim_matches(['"', '\'']).to_string();

    // Strip leading "This file/script/module/..." (case-insensitive), plus punctuation after it
    let re_lead = Regex::new(
        r#"(?i)^\s*this\s+(?:file|script|module|class|service|program|document|config(?:uration)?(?:\s+file)?|shell\s+script)\b[,:;\-\s]*"#,
    )
    .unwrap();
    s = re_lead.replace(&s, "").to_string();

    // If it now starts with "is/does/provides/contains", strip that too
    let re_verb = Regex::new(r#"(?i)^\s*(?:is|does|provides|contains)\b[,:;\-\s]*"#).unwrap();
    s = re_verb.replace(&s, "").to_string();

    // Ensure first alphabetic letter is capitalized
    capitalize_first_alpha(&s)
}

fn capitalize_first_alpha(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalized = false;
    for ch in s.chars() {
        if !capitalized && ch.is_alphabetic() {
            for up in ch.to_uppercase() {
                out.push(up);
            }
            capitalized = true;
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Quick binary sniff: look for NULs and a high ratio of non-printable bytes.
/// Reads at most `limit` bytes for speed.
fn is_probably_text(path: &std::path::Path, limit: usize) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(x) => x,
        Err(_) => return true, // can't tell; don't fail the world
    };
    let mut buf = vec![0u8; limit.min(8192)];
    let n = match std::io::BufReader::new(&mut f).read(&mut buf) {
        Ok(n) => n,
        Err(_) => return true,
    };
    if n == 0 {
        return true;
    }
    let sample = &buf[..n];

    // Any NUL => binary
    if sample.iter().any(|&b| b == 0) {
        return false;
    }

    // Count "printable-ish": ASCII 9,10,13,32..126; allow a few highs
    let printable = sample
        .iter()
        .filter(|&&b| b == b'\n' || b == b'\r' || b == b'\t' || (b >= 0x20 && b <= 0x7E))
        .count();

    // Heuristic: if < 85% printable, treat as binary
    printable * 100 / n >= 85
}

/// Strip/neutralize YAML-hostile chars:
/// - Drop ASCII control chars except \n \r \t
/// - Replace Unicode line/para separators and BOM with plain space
/// - Optionally trim weird lone surrogates (not valid in Rust, but included for clarity)
fn sanitize_for_yaml(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            // keep common whitespace
            '\n' | '\r' | '\t' => Some(c),

            // ASCII controls (C0 + DEL): drop
            _ if c.is_control() => None,

            // Unicode separators / BOM that sometimes confuse parsers
            '\u{2028}' | '\u{2029}' | '\u{FEFF}' => Some(' '),

            // Everything else: keep
            _ => Some(c),
        })
        .collect()
}

/// Safe literal to embed in YAML block scalars when content is binary/unavailable
fn suppressed_block() -> String {
    String::from("[[binary content suppressed]]")
}

/// Exponential backoff + jitter around `api::ask`.
/// - `max_attempts` includes the first try
/// - base: 300ms, factor 2x, cap 8s, plus 0–250ms jitter
async fn ask_with_retry(
    cfg: &AwfulJadeConfig,
    prompt: String,
    tpl: &ChatTemplate,
    max_attempts: usize,
) -> Result<String, anyhow::Error> {
    let base = Duration::from_millis(300);
    let cap = Duration::from_secs(8);

    for attempt in 1..=max_attempts {
        match api::ask(cfg, prompt.clone(), tpl, None, None).await {
            Ok(answer) => {
                if attempt > 1 {
                    info!(attempt, "api::ask succeeded after retries");
                }
                return Ok(answer);
            }
            Err(e) => {
                let is_last = attempt == max_attempts;
                // Avoid trait-bound issues by stringifying the error:
                let emsg = e.to_string();
                warn!(attempt, error = %emsg, "api::ask failed");
                if is_last {
                    return Err(anyhow::anyhow!(emsg));
                }

                // backoff = min(base * 2^(attempt-1), cap) + jitter, with safe shift
                let exp: u32 = ((attempt - 1) as u32).min(16); // cap exponent
                let factor: u32 = 1u32 << exp; // safe left shift
                let mut delay = base.checked_mul(factor).unwrap_or(cap);
                if delay > cap {
                    delay = cap;
                }
                delay += jitter_0_to_250ms();

                info!(
                    attempt_next = attempt + 1,
                    delay_ms = delay.as_millis(),
                    "Retrying api::ask"
                );
                sleep(delay).await;
            }
        }
    }
    Err(anyhow::anyhow!("ask_with_retry: exhausted attempts"))
}

fn jitter_0_to_250ms() -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Duration::from_nanos((nanos % 250_000_000) as u64)
}

/// Wrapper that pretty-prints only the first N fields of a Debug value to keep logs readable
struct TruncDebug<'a, T>(&'a T, usize);

impl<'a, T: std::fmt::Debug> std::fmt::Debug for TruncDebug<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // naive: rely on Debug output but truncate the resulting string
        let s = format!("{:#?}", self.0);
        let max = 500;
        if s.len() <= max {
            write!(f, "{}", s)
        } else {
            write!(f, "{}… (truncated)", &s[..max])
        }
    }
}
