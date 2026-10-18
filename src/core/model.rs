use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// Full hex object id of a git object.
pub type Oid = String;

/// A repository path as its original Git bytes.
///
/// Git paths are byte sequences on Unix and are not guaranteed to be UTF-8.
/// Keep those bytes for Git/filesystem operations and derive a lossy display
/// form only at the UI boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitPath(Vec<u8>);

const GIT_PATH_SERDE_MARKER: &str = "\u{f0000}gitpath:";

impl Serialize for GitPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Ok(valid) = std::str::from_utf8(&self.0) {
            if !valid.starts_with(GIT_PATH_SERDE_MARKER) {
                return serializer.serialize_str(valid);
            }
        }
        let mut encoded = String::with_capacity(GIT_PATH_SERDE_MARKER.len() + self.0.len() * 2);
        encoded.push_str(GIT_PATH_SERDE_MARKER);
        for byte in &self.0 {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for GitPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let Some(encoded) = value.strip_prefix(GIT_PATH_SERDE_MARKER) else {
            return Ok(Self(value.into_bytes()));
        };
        if encoded.len() % 2 != 0 {
            return Err(serde::de::Error::custom("invalid encoded Git path"));
        }
        let mut bytes = Vec::with_capacity(encoded.len() / 2);
        for pair in encoded.as_bytes().as_chunks::<2>().0 {
            let hex = std::str::from_utf8(pair).map_err(serde::de::Error::custom)?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(serde::de::Error::custom)?);
        }
        Ok(Self(bytes))
    }
}

impl GitPath {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn display(&self) -> Cow<'_, str> {
        if let Ok(valid) = std::str::from_utf8(&self.0) {
            if !valid
                .chars()
                .any(|character| character.is_control() || character == '\\')
            {
                return Cow::Borrowed(valid);
            }
        }

        let mut display = String::new();
        let mut remaining = self.0.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(valid) => {
                    append_display_text(&mut display, valid);
                    break;
                }
                Err(error) => {
                    let valid_length = error.valid_up_to();
                    if valid_length > 0 {
                        let valid = std::str::from_utf8(&remaining[..valid_length])
                            .expect("valid_up_to points at valid UTF-8");
                        append_display_text(&mut display, valid);
                    }
                    remaining = &remaining[valid_length..];
                    let invalid_length = error
                        .error_len()
                        .unwrap_or(remaining.len())
                        .max(1)
                        .min(remaining.len());
                    for byte in &remaining[..invalid_length] {
                        display.push_str(&format!("\\x{byte:02X}"));
                    }
                    remaining = &remaining[invalid_length..];
                }
            }
        }
        Cow::Owned(display)
    }

    pub fn to_os_string(&self) -> std::io::Result<OsString> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(OsString::from_vec(self.0.clone()))
        }
        #[cfg(windows)]
        {
            String::from_utf8(self.0.clone())
                .map(OsString::from)
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Git path is not valid UTF-8 on this platform",
                    )
                })
        }
        #[cfg(not(any(unix, windows)))]
        {
            String::from_utf8(self.0.clone())
                .map(OsString::from)
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Git path cannot be represented on this platform",
                    )
                })
        }
    }

    pub fn to_path_buf(&self) -> std::io::Result<PathBuf> {
        Ok(PathBuf::from(self.to_os_string()?))
    }
}

impl From<&str> for GitPath {
    fn from(path: &str) -> Self {
        Self(path.as_bytes().to_vec())
    }
}

impl From<String> for GitPath {
    fn from(path: String) -> Self {
        Self(path.into_bytes())
    }
}

impl fmt::Display for GitPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display())
    }
}

impl PartialEq<str> for GitPath {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl PartialEq<&str> for GitPath {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

fn append_display_text(display: &mut String, text: &str) {
    for character in text.chars() {
        if character == '\\' {
            display.push_str("\\\\");
        } else if character.is_control() {
            display.push_str(&format!("\\u{{{:X}}}", character as u32));
        } else {
            display.push(character);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: Oid,
    pub parents: Vec<Oid>,
    pub summary: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    /// Author date (unix seconds). Used for display.
    pub timestamp: i64,
    /// Committer date (unix seconds). Used for ordering, matching git's
    /// default `--date-order`.
    pub committer_timestamp: i64,
    pub timezone_offset: i32,
    /// Committer timezone offset in seconds (for displaying the date).
    pub committer_timezone_offset: i32,
}

/// A worktree/index entry from `git status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEntry {
    pub path: GitPath,
    /// Index status (`' '` when unchanged in the index).
    pub index: char,
    /// Worktree status (`' '` when unchanged in the worktree).
    pub worktree: char,
}

impl StatusEntry {
    /// Whether Git reports this path as unmerged (it has unresolved conflict stages).
    pub fn is_conflict(&self) -> bool {
        matches!(self.index, 'U')
            || matches!(self.worktree, 'U')
            || matches!((self.index, self.worktree), ('A', 'A') | ('D', 'D'))
    }
}

impl Commit {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

impl RefKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Head => "HEAD",
            RefKind::LocalBranch => "branch",
            RefKind::RemoteBranch => "remote",
            RefKind::Tag => "tag",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefLabel {
    pub kind: RefKind,
    pub name: String,
    pub id: Oid,
}

/// Commit graph in topological order: children come before parents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphModel {
    pub commits: Vec<Commit>,
    pub refs: Vec<RefLabel>,
    #[serde(default)]
    pub branch_tracking: Vec<BranchTracking>,
    pub head_id: Option<Oid>,
    pub head_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTracking {
    pub branch: String,
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
}

impl GraphModel {
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// Map of commit id -> index into `commits`.
    pub fn commit_index(&self) -> HashMap<&str, usize> {
        self.commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), i))
            .collect()
    }

    /// All ref labels pointing at a given commit id.
    pub fn refs_for<'a>(&'a self, id: &str) -> Vec<&'a RefLabel> {
        self.refs.iter().filter(|r| r.id == id).collect()
    }

    pub fn tracking_for_branch(&self, name: &str) -> Option<&BranchTracking> {
        self.branch_tracking
            .iter()
            .find(|tracking| tracking.branch == name)
    }

    /// Display-ready refs for a commit, with a deterministic order (HEAD, local
    /// branches, remote branches, tags).
    ///
    /// A local branch that has a remote-tracking counterpart pointing at the
    /// same commit is returned once with `has_remote: true` and its remote
    /// labels are suppressed, so we render `<glyph>main` instead of repeating
    /// `main origin/main`.
    pub fn display_refs(&self, id: &str) -> Vec<DisplayRef> {
        let refs = self
            .refs
            .iter()
            .filter(|reference| reference.id == id)
            .collect::<Vec<_>>();
        display_refs_for(&refs)
    }

    /// Precompute display refs grouped by commit id for repeated row rendering.
    pub fn display_refs_by_commit(&self) -> HashMap<Oid, Vec<DisplayRef>> {
        let mut refs_by_commit: HashMap<&str, Vec<&RefLabel>> = HashMap::new();
        for reference in &self.refs {
            refs_by_commit
                .entry(reference.id.as_str())
                .or_default()
                .push(reference);
        }
        refs_by_commit
            .into_iter()
            .map(|(id, refs)| (id.to_string(), display_refs_for(&refs)))
            .collect()
    }
}

fn display_refs_for(refs: &[&RefLabel]) -> Vec<DisplayRef> {
    fn order(kind: RefKind) -> u8 {
        match kind {
            RefKind::Head => 0,
            RefKind::LocalBranch => 1,
            RefKind::RemoteBranch => 2,
            RefKind::Tag => 3,
        }
    }
    fn remote_short_name(full: &str) -> &str {
        // "origin/main" or "upstream/feature/x" -> "main" / "feature/x"
        full.split_once('/').map(|(_, path)| path).unwrap_or(full)
    }

    let mut idx: Vec<usize> = (0..refs.len()).collect();
    idx.sort_by_key(|&i| order(refs[i].kind));

    let mut consumed = vec![false; refs.len()];
    let mut out = Vec::with_capacity(refs.len());
    for &i in &idx {
        if consumed[i] {
            continue;
        }
        let reference = refs[i];
        if reference.kind == RefKind::LocalBranch {
            let matched = idx.iter().any(|&j| {
                !consumed[j]
                    && refs[j].kind == RefKind::RemoteBranch
                    && remote_short_name(&refs[j].name) == reference.name
            });
            if matched {
                out.push(DisplayRef {
                    kind: reference.kind,
                    name: reference.name.clone(),
                    has_remote: true,
                });
                for &j in &idx {
                    if refs[j].kind == RefKind::RemoteBranch
                        && remote_short_name(&refs[j].name) == reference.name
                    {
                        consumed[j] = true;
                    }
                }
                continue;
            }
        }
        out.push(DisplayRef {
            kind: reference.kind,
            name: reference.name.clone(),
            has_remote: false,
        });
    }
    out
}

/// A ref as it should be presented next to a commit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRef {
    pub kind: RefKind,
    pub name: String,
    /// True when this is a local branch that has a remote-tracking
    /// counterpart at the same commit (render a remote glyph, no separate
    /// `origin/...` label).
    pub has_remote: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_path_bytes_display_unambiguously() {
        let invalid = GitPath::from_bytes(b"file-\xff.txt".to_vec());
        let literal_escape = GitPath::from("file-\\xFF.txt");
        assert_eq!(invalid.display(), "file-\\xFF.txt");
        assert_eq!(literal_escape.display(), "file-\\\\xFF.txt");
        assert_ne!(invalid, literal_escape);
    }

    fn ref_label(kind: RefKind, name: &str, id: &str) -> RefLabel {
        RefLabel {
            kind,
            name: name.to_string(),
            id: id.to_string(),
        }
    }

    fn graph_with(refs: Vec<RefLabel>) -> GraphModel {
        GraphModel {
            commits: vec![],
            refs,
            branch_tracking: Vec::new(),
            head_id: None,
            head_name: None,
        }
    }

    #[test]
    fn precomputed_display_refs_match_individual_lookups() {
        let graph = graph_with(vec![
            ref_label(RefKind::LocalBranch, "main", "m"),
            ref_label(RefKind::RemoteBranch, "origin/main", "m"),
            ref_label(RefKind::RemoteBranch, "upstream/topic", "t"),
            ref_label(RefKind::Tag, "v1", "m"),
        ]);
        let indexed = graph.display_refs_by_commit();
        for id in ["m", "t"] {
            assert_eq!(indexed.get(id).unwrap(), &graph.display_refs(id));
        }
    }

    #[test]
    fn remote_equivalent_collapses_into_glyph_flag() {
        let g = graph_with(vec![
            ref_label(RefKind::LocalBranch, "main", "m"),
            ref_label(RefKind::RemoteBranch, "origin/main", "m"),
            ref_label(RefKind::LocalBranch, "feature/x", "f"),
            ref_label(RefKind::RemoteBranch, "origin/feature/x", "f"),
            ref_label(RefKind::RemoteBranch, "origin/other", "f"),
            ref_label(RefKind::Tag, "v1", "b"),
        ]);

        let main = g.display_refs("m");
        assert_eq!(
            main,
            vec![DisplayRef {
                kind: RefKind::LocalBranch,
                name: "main".into(),
                has_remote: true
            }]
        );

        // `origin/feature/x` is suppressed; unmatched `origin/other` remains.
        let f = g.display_refs("f");
        assert_eq!(
            f,
            vec![
                DisplayRef {
                    kind: RefKind::LocalBranch,
                    name: "feature/x".into(),
                    has_remote: true
                },
                DisplayRef {
                    kind: RefKind::RemoteBranch,
                    name: "origin/other".into(),
                    has_remote: false
                },
            ]
        );

        assert_eq!(
            g.display_refs("b"),
            vec![DisplayRef {
                kind: RefKind::Tag,
                name: "v1".into(),
                has_remote: false
            }]
        );
    }

    #[test]
    fn remote_branch_without_local_is_kept() {
        let g = graph_with(vec![ref_label(RefKind::RemoteBranch, "origin/only", "m")]);
        assert_eq!(
            g.display_refs("m"),
            vec![DisplayRef {
                kind: RefKind::RemoteBranch,
                name: "origin/only".into(),
                has_remote: false
            }]
        );
    }

    #[test]
    fn local_without_remote_has_no_flag() {
        let g = graph_with(vec![ref_label(RefKind::LocalBranch, "main", "m")]);
        assert_eq!(
            g.display_refs("m"),
            vec![DisplayRef {
                kind: RefKind::LocalBranch,
                name: "main".into(),
                has_remote: false
            }]
        );
    }

    #[test]
    fn head_id_does_not_add_a_duplicate_display_ref() {
        let mut graph = graph_with(vec![ref_label(RefKind::LocalBranch, "main", "m")]);
        graph.head_id = Some("m".into());
        graph.head_name = Some("main".into());

        assert_eq!(
            graph.display_refs("m"),
            vec![DisplayRef {
                kind: RefKind::LocalBranch,
                name: "main".into(),
                has_remote: false
            }]
        );
        assert!(graph.display_refs("other").is_empty());
    }
}
