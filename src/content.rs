use blake3::Hasher;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

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

pub(crate) fn first_n_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

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

/* ---- tiny helpers used across the app ---- */

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}… ({} bytes total)", &s[..max], s.len())
    }
}

pub(crate) fn as_ms(d: std::time::Duration) -> u128 {
    d.as_millis()
}
