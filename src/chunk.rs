use crate::content::read_text_lossy_limited;
use std::path::Path;
use text_splitter::{ChunkConfig, CodeSplitter, MarkdownSplitter, TextSplitter};
use tiktoken_rs::cl100k_base;
use tree_sitter::Language;

#[derive(Debug, Clone)]
pub(crate) enum SplitterKind {
    Code(Language),
    Markdown,
    Text,
}

/// Handle token chunking for a file based on its mimetype and content.
///
/// Splits the text in `path` into chunks of tokens, using a splitter configured
/// with `max_tokens`. The function returns the first, middle, and last chunks of text.
///
/// # Parameters:
/// - `path`: Path to the file containing text.
/// - `mimetype`: MIME type of the file (used to determine splitter).
/// - `max_tokens`: Maximum number of tokens per chunk.
///
/// # Returns:
/// An `Option<(String, String, String, String)>` containing the first, middle, last chunks 
/// of text and a string indicating the splitter type (`"code"`, `"markdown"`, or `"text"`).
///
/// # Errors:
/// - Returns `None` if the file fails to be read or contains no content.
///
/// # Notes:
/// - The function handles empty files by returning empty strings.
/// - It uses a BPE tokenizer and configures chunking based on the file's content.
pub(crate) fn token_chunks_for_file(
    path: &Path,
    mimetype: &str,
    max_tokens: usize,
) -> Option<(String, String, String, String)> {
    let text = read_text_lossy_limited(path, 2_000_000);
    if text.trim().is_empty() {
        return Some((String::new(), String::new(), String::new(), "empty".into()));
    }

    let bpe = cl100k_base().ok()?;
    let cfg = ChunkConfig::new(max_tokens).with_sizer(&bpe);

    let kind = guess_splitter(mimetype, path);

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

/// Determine the appropriate splitter kind based on MIME type and file extension.
///
/// Parameters:
/// - `mime`: The MIME type of the content.
/// - `path`: A reference to a file path.
///
/// Returns:
/// - A [`SplitterKind`] indicating the type of content: `Code`, `Markdown`, or `Text`.
///
/// Notes:
/// - Checks MIME type for "markdown" and file extensions like .md, .markdown, or .mdx.
/// - Uses `guess_tree_sitter_language` to determine the specific code language.
pub(crate) fn guess_splitter(mime: &str, path: &Path) -> SplitterKind {
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

    if let Some(lang) = guess_tree_sitter_language(mime, path) {
        return SplitterKind::Code(lang);
    }

    SplitterKind::Text
}

/// Determine the Tree-sitter language based on file extension.
///
/// This function attempts to guess the appropriate Tree-sitter parser for a given file path
/// by examining its extension. It supports many common programming languages and uses
/// configuration flags to enable specific language detection.
///
/// Parameters:
/// - `_mime`: MIME type (unused in this implementation)
/// - `path`: File path to analyze
///
/// Returns:
/// - `Some(Language)` if a matching language is found, `None` otherwise.
///
/// Notes:
/// - The function uses a macro to compare file extensions against known language
///   patterns. It relies on Cargo features being enabled for specific languages.
pub(crate) fn guess_tree_sitter_language(_mime: &str, path: &Path) -> Option<Language> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    macro_rules! ext_is {
        ($($lit:literal),* $(,)?) => {{
            if let Some(ref e) = ext {
                matches!(e.as_str(), $( $lit )|*)
            } else { false }
        }};
    }

    // Bash / Shell
    #[cfg(feature = "lang-bash")]
    if ext_is!("sh", "bash", "zsh") {
        return Some(tree_sitter_bash::LANGUAGE.into());
    }

    // C
    #[cfg(feature = "lang-c")]
    if ext_is!("c", "h") {
        return Some(tree_sitter_c::LANGUAGE.into());
    }

    // C++
    #[cfg(feature = "lang-cpp")]
    if ext_is!("cpp", "cxx", "cc", "hpp", "hxx", "hh") {
        return Some(tree_sitter_cpp::LANGUAGE.into());
    }

    // C#
    #[cfg(feature = "lang-c-sharp")]
    if ext_is!("cs") {
        return Some(tree_sitter_c_sharp::LANGUAGE.into());
    }

    // CSS
    #[cfg(feature = "lang-css")]
    if ext_is!("css") {
        return Some(tree_sitter_css::LANGUAGE.into());
    }

    // ERB / EJS
    #[cfg(feature = "lang-embedded-template")]
    if ext_is!("erb", "ejs") {
        return Some(tree_sitter_embedded_template::LANGUAGE.into());
    }

    // Go
    #[cfg(feature = "lang-go")]
    if ext_is!("go") {
        return Some(tree_sitter_go::LANGUAGE.into());
    }

    // Haskell
    #[cfg(feature = "lang-haskell")]
    if ext_is!("hs") {
        return Some(tree_sitter_haskell::LANGUAGE.into());
    }

    // HTML
    #[cfg(feature = "lang-html")]
    if ext_is!("html", "htm") {
        return Some(tree_sitter_html::LANGUAGE.into());
    }

    // Java
    #[cfg(feature = "lang-java")]
    if ext_is!("java") {
        return Some(tree_sitter_java::LANGUAGE.into());
    }

    // JavaScript
    #[cfg(feature = "lang-javascript")]
    if ext_is!("js", "mjs", "cjs") {
        return Some(tree_sitter_javascript::LANGUAGE.into());
    }

    // JSDoc
    #[cfg(feature = "lang-jsdoc")]
    if ext_is!("jsdoc") {
        return Some(tree_sitter_jsdoc::LANGUAGE.into());
    }

    // JSON
    #[cfg(feature = "lang-json")]
    if ext_is!("json") {
        return Some(tree_sitter_json::LANGUAGE.into());
    }

    // Julia
    #[cfg(feature = "lang-julia")]
    if ext_is!("jl") {
        return Some(tree_sitter_julia::LANGUAGE.into());
    }

    // OCaml
    #[cfg(feature = "lang-ocaml")]
    if ext_is!("ml", "mli") {
        return Some(tree_sitter_ocaml::LANGUAGE_OCAML.into());
    }

    // PHP
    #[cfg(feature = "lang-php")]
    if ext_is!("php", "phtml") {
        return Some(tree_sitter_php::LANGUAGE_PHP.into());
    }

    // Python
    #[cfg(feature = "lang-python")]
    if ext_is!("py") {
        return Some(tree_sitter_python::LANGUAGE.into());
    }

    // Regex
    #[cfg(feature = "lang-regex")]
    if ext_is!("re", "regex") {
        return Some(tree_sitter_regex::LANGUAGE.into());
    }

    // Ruby
    #[cfg(feature = "lang-ruby")]
    if ext_is!("rb", "rake", "gemspec") {
        return Some(tree_sitter_ruby::LANGUAGE.into());
    }

    // Rust
    #[cfg(feature = "lang-rust")]
    if ext_is!("rs") {
        return Some(tree_sitter_rust::LANGUAGE.into());
    }

    // Scala
    #[cfg(feature = "lang-scala")]
    if ext_is!("scala", "sc") {
        return Some(tree_sitter_scala::LANGUAGE.into());
    }

    // TypeScript (+ TSX)
    #[cfg(feature = "lang-typescript")]
    if ext_is!("ts", "tsx") {
        return Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into());
    }

    // Verilog
    #[cfg(feature = "lang-verilog")]
    if ext_is!("v", "vh", "sv", "svh") {
        return Some(tree_sitter_verilog::LANGUAGE.into());
    }

    None
}
