use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Node {
    Dir(DirEntry),
    File(FileEntry),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DirEntry {
    pub name: String,
    pub path: String,
    pub updated_at: DateTime<Utc>,
    pub entries: Vec<Node>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FileEntry {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub doc: Doc, // the model response
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct Doc {
    pub fileDescription: String,

    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    pub joyThisFileBrings: serde_json::Value,

    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    pub personalityEmoji: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DirdocsRoot {
    pub root: String,
    pub updated_at: DateTime<Utc>,
    pub entries: Vec<Node>,
}
