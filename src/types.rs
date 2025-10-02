use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

/// A representation of a directory entry with metadata and nested nodes.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DirEntry {
    /// Name of the file/directory (with possible trailing "/").
    pub name: String,
    /// Full path to the file/directory (with possible leading "/").
    pub path: String,
    /// Time of last update for the file/directory; UTC time in ISO format.
    pub updated_at: DateTime<Utc>,
    /// Nested entries of this directory (child nodes).
    pub entries: Vec<Node>,
}

/// Represents a file entry with metadata. This struct stores basic file information and an optional model response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FileEntry {
    /// The file's name (e.g., 'example.txt').
    pub name: String,
    /// Absolute file path (e.g., '/users/aj/example.txt').
    pub path: String,
    /// SHA-256 hash of the file content (e.g., 'd41d8cd98f00b204e9800998ecf84279').
    pub hash: String,
    /// The datetime when the file was last updated (e.g., '2023-10-05T14:30:00Z').
    pub updated_at: DateTime<Utc>,
    /// The model's response, if any (default is empty).
    #[serde(default)]
    pub doc: Doc,
}

/// The fundamental unit that describes a file's characteristics. It stores information about the file's description, joy level, and personality emoji.
#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct Doc {
    /// The file's description.
    pub fileDescription: String,

    /// The joy level of the file.
    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    pub joyThisFileBrings: serde_json::Value,

    /// The personality emoji.
    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    pub personalityEmoji: String,
}

/// Represents a directory root with metadata and child nodes.
///
/// This struct is used to store the state of a directory structure, including its path.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DirdocsRoot {
    /// The absolute path to the directory root.
    pub root: String,
    /// A UTC DateTime indicating when the directory was last updated.
    pub updated_at: DateTime<Utc>,
    /// A list of child nodes in the directory.
    pub entries: Vec<Node>,
}
