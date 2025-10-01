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
use crate::content::{
    as_ms, file_meta, hash_file, is_probably_text, readme_context, truncate,
};
use crate::prompt_llm::{
    ModelResp, ask_with_retry, indent_for_yaml, render_chat_template, sanitize_description,
    sanitize_for_yaml, suppressed_block,
};
use crate::types::{DirdocsRoot, Doc, FileEntry};

use awful_aj::config::AwfulJadeConfig;
use chrono::Utc;
use clap::Parser;
use handlebars::Handlebars;
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

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

/* ---------------- Template data for Handlebars ---------------- */

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
    info!(?args, "dir_docs starting");

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
    debug!(
        project_is_documented=%project_is_documented,
        doc_snippet_len=project_doc_snippet.len(),
        "README context collected"
    );

    // Existing .dirdocs.nu
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

        // Render → ChatTemplate (with safe error preview)
        let tpl = match render_chat_template(&hbs, &raw_template, &data) {
            Ok(t) => t,
            Err(e) => {
                error!(%e, file=%path.display(), "Template/YAML error");
                continue;
            }
        };

        let updated_at = Utc::now();

        // ---- Timed API call (with backoff) ----
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
    write_tree(&dirdocs_path, &new_root)?;

    info!("Done");
    Ok(())
}
