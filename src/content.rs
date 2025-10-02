use blake3::Hasher;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

/// Handle the `readme_context` function.
/// This function searches for a README file in various common formats (e.g., .md, .txt) under the given root directory.
/// It loads the first valid file found, reads its contents (with a maximum length of 2 million characters),
/// extracts the first 500 words, and returns a tuple with a boolean indicating success or failure.
/// If no README file is found, it returns `false` and an empty string.
///
/// Parameters:
/// - `root`: A reference to the directory path where to search for README files.
///
/// Returns:
/// - `Ok((true, String))` if a valid README file is found and processed.
/// - `Ok((false, String))` if no valid README file is found.
///
/// Errors:
/// - Returns an `anyhow::Error` if any I/O operations fail, or during text reading/processing.
/// 
/// Notes:
/// - The function searches for a README in case-insensitive, common formats.
/// - The maximum text length is set to 2 million characters for performance reasons.
pub(crate) fn readme_context(root: &Path) -> anyhow::Result<(String, String)> {
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

/// Get metadata about a file.
///
/// Returns the human-readable size, file type, and MIME type of the given path.
/// Uses `fs::metadata` to retrieve metadata, then uses methods from `Path`
/// and external crates like `mime_guess`, `tree_magic_mini`, and `human_bytes`.
///
/// Parameters:
/// - `path`: A reference to a file path.
///
/// Returns:
/// - A `(String, String, String)` tuple containing the human-readable size, 
/// file type (e.g., "txt", "unknown"), and MIME type (e.g., "text/plain", "application/octet-stream").
///
/// Errors:
/// - This function does not return an explicit error, but failures during I/O
///   operations or calls to external crates like `mime_guess::from_path`
///   or `tree_magic_mini::from_filepath` may result in panics.
///
/// Notes:
/// - The file type is determined by examining the extension of the path,
///   defaulting to "unknown" if no extension is found.
/// - The MIME type is determined by checking both `mime_guess` and
///   `tree_magic_mini`, with a fallback to "application/octet-stream"
///   if no specific type is found.
/// ```
pub(crate) fn file_meta(path: &Path) -> (String, String, String) {
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

/// Read up to `max_bytes` of text from a file, returning it as a UTF-8 lossy string.
///
/// This function opens the specified file, reads up to `max_bytes` of content, and returns
/// it as a UTF-8 lossy string. If the file is not found or an error occurs during reading,
/// it returns an empty string instead.
///
/// Parameters:
/// - `path`: The path to the file to read from.
/// - `max_bytes`: The maximum number of bytes to read from the file.
///
/// Returns:
/// - A `String` containing the UTF-8 lossy representation of up to `max_bytes` from
///   the file.
///
/// Errors:
/// - If the file cannot be opened or read, an empty `String` is returned.
///
/// Notes:
/// - The function uses `io::Read::take` to limit the number of bytes read.
/// - If no content is available (e.g., file is empty), it returns an empty string.
pub(crate) fn read_text_lossy_limited(path: &Path, max_bytes: usize) -> String {
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

/// Returns the first `n` words from a string.
///
/// Parameters:
/// - `s`: The input string to process.
/// - `n`: The number of words to return. If `n` is zero, an empty string is returned.
///
/// Returns:
/// - The first `n` words of the input string, joined with spaces. If `n` is zero,
///   an empty string is returned.
///
/// Errors:
/// - This function does not return any errors.
///
/// Notes:
/// - The function splits the string by whitespace and takes the first `n` words.
/// - If `n` is zero, the function returns an empty string.
pub(crate) fn first_n_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

/// Convert a byte count to a human-readable string, such as "3.5 GB" or "4 KB".
///
/// Returns a formatted string representing the given byte count in a more
/// user-friendly format, using appropriate SI prefixes (B, KB, MB, GB, TB).
///
/// # Parameters:
/// - `b`: A u64 representing the number of bytes to convert.
///
/// # Returns:
/// A String in the format "value unit", e.g., "4 KB" or "3.5 GB".
pub(crate) fn human_bytes(b: u64) -> String {
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

/// Handle a file by hashing its contents.
///
/// Opens the file, reads its contents in chunks,
/// and computes a hash using `Hasher`. The resulting
/// hexadecimal string is returned as the hash value.
///
/// # Parameters:
/// - `path`: A reference to a file path
///
/// # Returns:
/// The hexadecimal hash string of the file contents.
///
/// # Errors:
/// - I/O errors when opening or reading the file
/// - Hashing errors if a hash object cannot be created.
///
/// # Notes:
/// - Hashes are computed by reading the file in chunks
///   of 8192 bytes at a time.
/// - The final hash value is returned as a hexadecimal string
pub(crate) fn hash_file(path: &Path) -> io::Result<String> {
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

/// Checks if a file path contains primarily printable ASCII text.
///
/// This function reads the first `limit` bytes of a file to determine if it contains
/// mostly printable ASCII characters. It returns `true` if the file is likely to be text,
/// based on a threshold of at least 85% printable characters.
///
/// Parameters:
/// - `path`: Path to the file being checked
/// - `limit`: Maximum number of bytes to examine from the start of the file
///
/// Returns:
/// - `true` if at least 85% of the examined bytes are printable ASCII characters
/// - `false` otherwise
///
/// Errors:
/// This function does not return errors explicitly; it returns a `bool` based on content analysis.
///
/// Notes:
/// - The function reads the first `limit` bytes of a file starting from the beginning.
/// - It considers ASCII printable characters as: newline (`
/// `), carriage return (`\r`),
///   tab (`	`), and any byte between 0x20 (space) and 0x7E.
pub(crate) fn is_probably_text(path: &Path, limit: usize) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(x) => x,
        Err(_) => return true,
    };
    let mut buf = vec![0u8; limit.min(8192)];
    let n = match io::BufReader::new(&mut f).read(&mut buf) {
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

    // Count "printable-ish"
    let printable = sample
        .iter()
        .filter(|&&b| b == b'\n' || b == b'\r' || b == b'\t' || (b >= 0x20 && b <= 0x7E))
        .count();

    printable * 100 / n >= 85
}

/// Truncates a string to a specified maximum length, appending "…" if truncated.
///
/// Parameters:
/// - `s`: The string to truncate.
/// - `max`: The maximum number of bytes allowed in the result.
///
/// Returns:
/// A new string that is at most `max` bytes long. If the input string
/// exceeds this limit, it will be truncated and end with "…".
///
/// Safety:
/// This function is safe to use with any UTF-8 string.
///
/// Notes:
/// - If the input string is already shorter than or equal to `max`, it returns
///   a copy of the original string.
/// - If truncated, it appends "…" followed by the total length of the
///   original string.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}… ({} bytes total)", &s[..max], s.len())
    }
}

/// Converts a `Duration` into the number of milliseconds.
///
/// Takes a duration and returns its equivalent in milliseconds using
/// `Duration::as_millis()`.
///
/// Parameters:
/// - `d`: A `std::time::Duration` representing time.
///
/// Returns:
/// - The number of milliseconds equivalent to the duration.
///
/// Errors:
/// - None; this function does not return an error.
pub(crate) fn as_ms(d: std::time::Duration) -> u128 {
    d.as_millis()
}
