use crate::types::{DirdocsRoot, FileEntry, Node};
use chrono::Utc;
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CHILD_CACHE_NAMES: &[&str] = &[".dirdocs.nu", ".dir.nuon"];

/// Load an existing dirdocs tree from a JSON file.
///
/// Reads the JSON content of `path`, deserializes it into a
/// [`DirdocsRoot`], and returns it. If the deserialization
/// fails, constructs a default `DirdocsRoot` with:
/// - A relative label of the root directory
/// - The current UTC time (`Utc::now()`)
/// - An empty list of entries
///
/// # Parameters:
/// - `path`: Path to the JSON file containing the dirdocs tree. If empty, no file is read.
/// - `root_abs`: Absolute path of the dirdocs root directory.
/// - `cwd`: Current working directory for relative path resolution.
///
/// # Returns:
/// The deserialized or newly created `DirdocsRoot` object.
///
/// # Errors:
/// - Any I/O error when reading the file (`fs::read_to_string`)
/// - Any JSON deserialization error (`serde_json::from_str`)
///
/// # Notes:
/// This function assumes the input file contains valid JSON for
/// a `DirdocsRoot` object. If not, it falls back to constructing
/// an empty default tree.
///
/// # See Also:
/// - `DirdocsRoot`
/// - `rel_label`
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

/// Writes a directory tree to disk as pretty-printed JSON.
///
/// This function serializes the provided `DirdocsRoot` structure into a pretty-printed
/// JSON string and writes it to the specified file path. The serialized data is then
/// written to disk using `fs::write`.
///
/// Parameters:
/// - `path`: The file path where the serialized JSON will be written.
/// - `tree`: A reference to a `DirdocsRoot` struct containing the data to serialize.
///
/// Returns:
/// - A result indicating success (`Ok(())`) or failure (with an ` anyhow::Error`).
///
/// Errors:
/// - If serialization to JSON fails.
/// - If writing the file fails.
///
/// Notes:
/// The function uses `serde_json::to_string_pretty` for serialization and `fs::write` to write the output.
pub(crate) fn write_tree(path: &Path, tree: &DirdocsRoot) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(tree)? + "\n";
    fs::write(path, body)?;
    Ok(())
}

/// Recursively indexes file nodes and their contents into a map, organizing files by path.
///
/// Parameters:
/// - `nodes`: A slice of file nodes to index. Each node is either a directory or a regular file.
/// - `map`: A mutable reference to a HashMap that maps file paths to FileEntry objects.
///
/// Returns: 
/// None, as this function does not return a value but performs side effects by populating the map.
///
/// Errors: 
/// This function does not propagate errors, as it is designed to handle all I/O and logic internally.
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
/// Handle finding child cache directories under a parent root.
///
/// This function scans the filesystem starting at `parent_root` to find all directories
/// that match known child cache names. It uses a stack-based traversal and returns
/// the full paths of matching directories.
///
/// Parameters:
/// - `parent_root`: The root directory to start searching from.
///
/// Returns:
/// A vector of full paths representing matching child cache directories.
///
/// Errors:
/// - I/O errors when reading directory contents or resolving paths.
///
/// Notes:
/// - It uses a stack for depth-first traversal of the filesystem.
/// - Matching is case-insensitive and does not require full path resolution.
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

/// Rebases a child tree of nodes into an existing directory structure by resolving relative paths from a parent root.
///
/// Parameters:
/// - `child_root_abs`: The absolute path to the child tree's root.
/// - `parent_root_abs`: The absolute path to the parent directory from which relative paths are resolved.
/// - `tree`: A reference to a root node containing the child tree's entries.
/// - `map`: A mutable reference to a hashmap where file and directory entries are inserted with rebased paths.
///
/// Returns:
/// - `()`: No return value; the function performs in-place operations on the map.
///
/// Errors:
/// - This function does not return errors explicitly; any I/O or path resolution issues are handled internally.
///
/// Notes:
/// - The function resolves relative paths between the child and parent roots using `pathdiff::diff_paths`.
/// - It recursively processes all nodes in the tree, inserting rebased file entries into a hashmap.
pub(crate) fn rebase_child_tree_into_existing_by_path(
    child_root_abs: &Path,
    parent_root_abs: &Path,
    tree: &DirdocsRoot,
    map: &mut HashMap<String, FileEntry>,
) {
    let base_rel =
        pathdiff::diff_paths(child_root_abs, parent_root_abs).unwrap_or_else(|| PathBuf::from("."));

    /// Handle directory traversal and file mapping based on a list of nodes.
    ///
    /// This function recursively processes each `Node` in the provided slice. If a
    /// directory node is encountered, it recursively calls itself with its entries.
    /// For file nodes, it clones the file data and adjusts the path relative to the
    /// base directory. The adjusted files are then stored in a hashmap under their
    /// rebased (relative) paths.
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

/// Handle relative path labeling by computing the difference between root and current working directory.
///
/// Takes two paths: `root_abs` (absolute) and `cwd` (current working directory),
/// computes the relative path between them using `pathdiff::diff_paths`,
/// and returns the result as a normalized string.
///
/// Parameters:
/// - `root_abs`: An absolute path to label against
/// - `cwd`: The current working directory for comparison
///
/// Returns:
/// A string representing the relative path between `root_abs` and `cwd`.
///
/// Errors:
/// None. This function does not return errors.
///
/// Notes:
/// - If no path is found, returns `"."
/// - Uses `to_string_lossy()` to handle non-ASCII characters
/// - Avoids unnecessary string conversions for efficiency
fn rel_label(root_abs: &Path, cwd: &Path) -> String {
    let rel = pathdiff::diff_paths(root_abs, cwd).unwrap_or_else(|| PathBuf::from("."));
    let s = rel.to_string_lossy();
    if s.is_empty() {
        ".".to_string()
    } else {
        s.to_string()
    }
}

/// Insert a file into the tree structure based on its relative path.
///
/// This function takes a `FileEntry` and inserts it into the tree under the specified
/// relative path. It uses component-wise traversal of the path, ignoring non-normal components,
/// and inserts the file recursively into the tree structure.
///
/// Parameters:
/// - `entries`: A mutable reference to a list of nodes (the tree).
/// - `rel_path`: The relative path where the file should be inserted.
/// - `fe`: A reference to a `FileEntry` representing the file data.
///
/// Returns: 
/// None
///
/// Errors:
/// - If there are no components in the path (i.e., it's empty).
///
/// Notes:
/// - The function handles path components by ignoring non-normal (e.g., special or absolute) components.
/// - It uses `Path::components()` to break down the path into its components for recursive insertion.
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
/// Handle recursively inserting a file entry into the `entries` vector.
///
/// This function creates or updates directory nodes in the tree based on filename components,
/// and inserts a file node at the appropriate location. It ensures that directories are
/// created as needed, with updated timestamps.
///
/// Parameters:
/// - `entries`: A mutable reference to a vector of nodes where the file will be inserted.
/// - `comps`: A slice of strings representing filename components to navigate the tree.
/// - `fe`: A reference to a file metadata object used for constructing the file node.
///
/// Returns:
/// - (), as there are no return values from this function.
///
/// Errors:
/// - This function does not explicitly return errors, but may panic if the file metadata
///   or node operations fail. See `FileEntry` and `Node` for more details.
///
/// Safety:
/// - This function is not thread-safe and must be called in a single-threaded context.
///
/// Notes:
/// - Recursion is used to build the directory tree from filename components, starting at
///   the root of `entries`.
/// - Updated timestamps are set on both directories and files after insertion.
///   This ensures accurate time tracking in the tree.
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
