use crate::types::{DirdocsRoot, FileEntry, Node};
use chrono::Utc;
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CHILD_CACHE_NAMES: &[&str] = &[".dirdocs.nu", ".dir.nuon"];

pub(crate) fn load_existing_tree(path: &Path, root_abs: &Path, cwd: &Path) -> DirdocsRoot {
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

pub(crate) fn write_tree(path: &Path, tree: &DirdocsRoot) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(tree)? + "\n";
    fs::write(path, body)?;
    Ok(())
}

pub(crate) fn index_files_by_path(nodes: &[Node], map: &mut HashMap<String, FileEntry>) {
    for n in nodes {
        match n {
            Node::Dir(d) => index_files_by_path(&d.entries, map),
            Node::File(f) => {
                map.insert(f.path.clone(), f.clone());
            }
        }
    }
}

pub(crate) fn find_child_cache_dirs(parent_root: &Path) -> Vec<PathBuf> {
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
            continue; // don't descend
        }

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

pub(crate) fn rebase_child_tree_into_existing_by_path(
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

fn rel_label(root_abs: &Path, cwd: &Path) -> String {
    let rel = pathdiff::diff_paths(root_abs, cwd).unwrap_or_else(|| PathBuf::from("."));
    let s = rel.to_string_lossy();
    if s.is_empty() {
        ".".to_string()
    } else {
        s.to_string()
    }
}

/* ---- Build nested tree from path parts ---- */

pub(crate) fn insert_file_into_tree(entries: &mut Vec<Node>, rel_path: &str, fe: &FileEntry) {
    use std::path::Component;
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
    use chrono::Utc;
    if comps.len() == 1 {
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

    let dir_name = &comps[0];
    if let Some(Node::Dir(dir)) = entries
        .iter_mut()
        .find(|n| matches!(n, Node::Dir(d) if d.name == *dir_name))
    {
        insert_recursive(&mut dir.entries, &comps[1..], fe);
        dir.updated_at = Utc::now();
    } else {
        let mut new_dir = crate::types::DirEntry {
            name: dir_name.clone(),
            path: comps[..1].join("/"),
            updated_at: Utc::now(),
            entries: Vec::new(),
        };
        insert_recursive(&mut new_dir.entries, &comps[1..], fe);
        entries.push(Node::Dir(new_dir));
    }
}
