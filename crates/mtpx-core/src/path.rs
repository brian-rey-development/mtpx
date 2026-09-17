//! The two path types shared by every operation and the segment rules they enforce.

use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

pub(crate) const SEPARATOR: char = '/';

/// Absolute, normalized POSIX-style path inside one storage. Never contains ".", ".." or empty segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePath(RelPath);

/// Relative path inside a snapshot, shared by both sides. Always forward slashes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(Vec<String>);

/// Why a string could not become a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// The input, or a segment, had no characters.
    #[error("path is empty")]
    Empty,
    /// A remote path was given without a leading slash.
    #[error("remote path must start with '/'")]
    NotAbsolute,
    /// A segment was `.` or `..`.
    #[error("'.' and '..' segments are not allowed")]
    DotSegment,
    /// A segment contained a character that cannot appear in a file name.
    #[error("invalid path segment: {0:?}")]
    InvalidSegment(String),
}

// One plain file name per segment, so joining under a local root can never escape it.
fn is_single_normal_component(segment: &str) -> bool {
    let mut components = Path::new(segment).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

const WIN32_FORBIDDEN_CHARS: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];
const WIN32_DEVICE_NAMES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

// Win32 reinterprets these rather than rejecting them: `:` opens an NTFS alternate data
// stream, trailing dots and spaces are stripped, and device names bypass the filesystem.
// Compiled everywhere so the rule is unit-tested on every host; only Windows applies it.
fn is_valid_windows_name(segment: &str) -> bool {
    let has_forbidden = segment
        .chars()
        .any(|c| WIN32_FORBIDDEN_CHARS.contains(&c) || c.is_control());
    if has_forbidden || segment.ends_with('.') || segment.ends_with(' ') {
        return false;
    }
    let stem = segment.split('.').next().unwrap_or_default().trim_end();
    !(is_win32_device(stem) || is_win32_port(stem))
}

fn is_win32_device(stem: &str) -> bool {
    WIN32_DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

// COM1 to COM9 and LPT1 to LPT9; COM0 and COM10 are ordinary names.
fn is_win32_port(stem: &str) -> bool {
    let [a, b, c, digit] = stem.as_bytes() else {
        return false;
    };
    let prefix = [*a, *b, *c];
    let is_port_prefix = prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT");
    is_port_prefix && (b'1'..=b'9').contains(digit)
}

fn validate_segment(segment: &str) -> Result<(), PathError> {
    if segment.is_empty() {
        return Err(PathError::Empty);
    }
    if segment == "." || segment == ".." {
        return Err(PathError::DotSegment);
    }
    let has_forbidden_char = segment.contains('\0') || segment.contains(SEPARATOR);
    let unusable_on_host = cfg!(windows) && !is_valid_windows_name(segment);
    if has_forbidden_char || !is_single_normal_component(segment) || unusable_on_host {
        return Err(PathError::InvalidSegment(segment.to_owned()));
    }
    Ok(())
}

fn split_last_segment(segments: &[String]) -> Option<(&[String], &str)> {
    segments
        .split_last()
        .map(|(last, init)| (init, last.as_str()))
}

impl RemotePath {
    /// Parses an absolute path, normalizing repeated and trailing slashes away.
    ///
    /// # Errors
    /// When the input is empty, not absolute, or contains a `.`, `..` or invalid segment.
    pub fn parse(input: &str) -> Result<Self, PathError> {
        if input.is_empty() {
            return Err(PathError::Empty);
        }
        let Some(rest) = input.strip_prefix(SEPARATOR) else {
            return Err(PathError::NotAbsolute);
        };
        let segments = rest.split(SEPARATOR).filter(|segment| !segment.is_empty());
        RelPath::new(segments).map(Self)
    }

    /// The storage root, `/`.
    #[must_use]
    pub const fn root() -> Self {
        Self(RelPath::root())
    }

    /// The path's components, without separators.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        self.0.segments()
    }

    /// Appends a relative path beneath this one.
    #[must_use]
    pub fn join(&self, rel: &RelPath) -> Self {
        Self(self.0.append(rel))
    }

    /// The last segment, or `None` for the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.0.file_name()
    }

    /// The containing directory, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0.parent().map(Self)
    }

    /// Whether this is the storage root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_root()
    }
}

impl RelPath {
    /// Builds a relative path from segments; an empty list is the root itself.
    ///
    /// # Errors
    /// When any segment is empty, `.`, `..`, contains `/` or NUL, or is not a plain file name on this host.
    pub fn new(segments: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, PathError> {
        segments
            .into_iter()
            .map(Into::into)
            .map(|segment| validate_segment(&segment).map(|()| segment))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// The snapshot root, the empty path.
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// The path's components, without separators.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// The containing directory, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        split_last_segment(&self.0).map(|(init, _)| Self(init.to_vec()))
    }

    /// The last segment, or `None` for the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        split_last_segment(&self.0).map(|(_, last)| last)
    }

    /// Appends one segment.
    ///
    /// # Errors
    /// Same segment rules as [`RelPath::new`].
    pub fn join(&self, segment: &str) -> Result<Self, PathError> {
        validate_segment(segment)?;
        let mut segments = self.0.clone();
        segments.push(segment.to_owned());
        Ok(Self(segments))
    }

    fn append(&self, tail: &Self) -> Self {
        Self(self.0.iter().chain(&tail.0).cloned().collect())
    }

    /// Number of segments; the root has depth zero.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.len()
    }

    /// Whether this is the snapshot root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `prefix` is this path or one of its ancestors.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// Where this path lands under `root`: `root` itself for the empty path, otherwise
    /// strictly beneath it.
    #[must_use]
    pub fn to_local_path(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        path.extend(&self.0);
        path
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in self.0.segments() {
            write!(f, "{SEPARATOR}{segment}")?;
        }
        if self.is_root() {
            write!(f, "{SEPARATOR}")?;
        }
        Ok(())
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, "{SEPARATOR}")?;
            }
            f.write_str(segment)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn rel(segments: &[&str]) -> RelPath {
        RelPath::new(segments.iter().copied()).unwrap()
    }

    fn remote(s: &str) -> RemotePath {
        RemotePath::parse(s).unwrap()
    }

    #[test]
    fn parses_and_normalizes_remote_paths() {
        let cases = [
            ("/", vec![]),
            ("/DCIM", vec!["DCIM"]),
            ("/DCIM/Camera/", vec!["DCIM", "Camera"]),
            ("//a//b", vec!["a", "b"]),
            ("/Fotos/año", vec!["Fotos", "año"]),
        ];
        for (input, segments) in cases {
            assert_eq!(remote(input).segments(), segments, "{input}");
        }
    }

    #[test]
    fn rejects_invalid_remote_paths() {
        let cases = [
            ("", PathError::Empty),
            ("DCIM", PathError::NotAbsolute),
            ("/a/../b", PathError::DotSegment),
            ("/./a", PathError::DotSegment),
            ("/a\0b", PathError::InvalidSegment("a\0b".into())),
        ];
        for (input, expected) in cases {
            assert_eq!(RemotePath::parse(input).unwrap_err(), expected, "{input}");
        }
    }

    #[test]
    fn join_and_parent_round_trip() {
        let base = remote("/DCIM");
        let joined = base.join(&rel(&["Camera", "IMG_1.jpg"]));
        assert_eq!(joined, remote("/DCIM/Camera/IMG_1.jpg"));
        assert_eq!(joined.file_name(), Some("IMG_1.jpg"));
        let parent = joined.parent().unwrap();
        assert_eq!(parent, remote("/DCIM/Camera"));
        assert_eq!(parent.parent().unwrap(), base);
        assert_eq!(base.parent().unwrap(), RemotePath::root());
        assert!(RemotePath::root().parent().is_none());
        assert!(RemotePath::root().is_root());
        assert!(RemotePath::root().file_name().is_none());
    }

    #[test]
    fn rel_path_orders_parent_before_children() {
        let mut paths = vec![rel(&["b"]), rel(&["a", "b"]), rel(&["a"]), RelPath::root()];
        paths.sort();
        assert_eq!(
            paths,
            vec![RelPath::root(), rel(&["a"]), rel(&["a", "b"]), rel(&["b"])]
        );
    }

    #[test]
    fn rel_path_accessors() {
        let p = rel(&["a", "b", "c"]);
        assert_eq!(p.depth(), 3);
        assert!(!p.is_root());
        assert!(RelPath::root().is_root());
        assert_eq!(p.file_name(), Some("c"));
        assert_eq!(p.parent(), Some(rel(&["a", "b"])));
        assert_eq!(rel(&["a"]).parent(), Some(RelPath::root()));
        assert_eq!(RelPath::root().parent(), None);
        assert!(p.starts_with(&rel(&["a", "b"])));
        assert!(p.starts_with(&RelPath::root()));
        assert!(!p.starts_with(&rel(&["a", "c"])));
        assert!(!rel(&["a"]).starts_with(&p));
        assert_eq!(rel(&["a"]).join("b").unwrap(), rel(&["a", "b"]));
        assert_eq!(rel(&["a"]).join("..").unwrap_err(), PathError::DotSegment);
        assert_eq!(rel(&["a"]).join("").unwrap_err(), PathError::Empty);
    }

    #[test]
    fn rejects_bad_segments_on_every_platform() {
        let cases = [
            ("", PathError::Empty),
            (".", PathError::DotSegment),
            ("..", PathError::DotSegment),
            ("a/b", PathError::InvalidSegment("a/b".into())),
            ("a\0b", PathError::InvalidSegment("a\0b".into())),
        ];
        for (segment, expected) in cases {
            assert_eq!(
                RelPath::new([segment]).unwrap_err(),
                expected,
                "{segment:?}"
            );
            assert_eq!(
                RelPath::root().join(segment).unwrap_err(),
                expected,
                "{segment:?}"
            );
        }
        assert_eq!(RelPath::new(Vec::<String>::new()).unwrap(), RelPath::root());
    }

    const WIN32_REJECTED: [&str; 17] = [
        "12:30 recording.m4a",
        "photo.jpg.",
        "photo.jpg ",
        "a<b",
        "a>b",
        "a\"b",
        "a|b",
        "a?b",
        "a*b",
        "a\u{1}b",
        "nul",
        "NUL.txt",
        "Con",
        "aux.tar.gz",
        "com1",
        "LPT9.log",
        "COM1 .txt",
    ];
    const WIN32_ACCEPTED: [&str; 10] = [
        "com0", "com10", "console", "nul2", "a.b", "año", ".hidden", "ñññ1", "lpt", "x",
    ];

    #[test]
    fn windows_rule_rejects_names_win32_would_reinterpret() {
        for segment in WIN32_REJECTED {
            assert!(!is_valid_windows_name(segment), "{segment:?}");
        }
        for segment in WIN32_ACCEPTED {
            assert!(is_valid_windows_name(segment), "{segment:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn rejects_segments_with_windows_path_structure() {
        for segment in [
            "..\\x",
            "C:\\x",
            "C:x",
            "\\x",
            "a\\b",
            "12:30 recording.m4a",
            "photo.jpg.",
            "nul",
            "COM1",
        ] {
            assert_eq!(
                RelPath::new([segment]).unwrap_err(),
                PathError::InvalidSegment(segment.into()),
                "{segment:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn backslash_and_colon_are_ordinary_characters_on_unix() {
        assert_eq!(rel(&["a\\b"]).to_string(), "a\\b");
        assert_eq!(rel(&["12:30.png"]).to_string(), "12:30.png");
    }

    #[test]
    fn to_local_path_stays_beneath_root_for_every_valid_path() {
        let root = std::env::temp_dir();
        let paths = [
            RelPath::root(),
            rel(&["a"]),
            rel(&["a", "b.txt"]),
            rel(&["DCIM", "Camera", "IMG_1.jpg"]),
            rel(&["..a", "a..b"]),
            rel(&["Fotos", "año"]),
        ];
        for p in paths {
            let local = p.to_local_path(&root);
            assert!(local.starts_with(&root), "{p}");
            let expected_components = root.components().count() + p.depth();
            assert_eq!(local.components().count(), expected_components, "{p}");
        }
        assert_eq!(RelPath::root().to_local_path(&root), root);
    }

    #[test]
    fn display_renders_segments_with_forward_slashes() {
        assert_eq!(RelPath::root().to_string(), "");
        assert_eq!(rel(&["a"]).to_string(), "a");
        assert_eq!(rel(&["a", "b"]).to_string(), "a/b");
        assert_eq!(RemotePath::root().to_string(), "/");
        assert_eq!(remote("/a/b/").to_string(), "/a/b");
    }
}
