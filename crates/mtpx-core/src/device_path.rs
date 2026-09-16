//! Storage selectors and the `storage:/path` form every CLI remote argument parses into.

use crate::path::{PathError, RemotePath, SEPARATOR};
use std::{fmt, str::FromStr};

const STORAGE_DELIMITER: char = ':';

/// Which storage on the device. Separate from the path: "Internal" is not a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageSelector {
    /// The only storage, or the configured default; an error when the device has several.
    Default,
    /// Storage by its position in the device's enumeration order.
    Index(usize),
    /// Storage by description or volume identifier, matched case-insensitively.
    Named(String),
}

/// A storage plus a path. What every CLI remote argument parses into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePath {
    /// Which storage the path lives on.
    pub storage: StorageSelector,
    /// Absolute path inside that storage.
    pub path: RemotePath,
}

fn parse_selector(prefix: &str) -> StorageSelector {
    prefix.parse::<usize>().map_or_else(
        |_| StorageSelector::Named(prefix.to_owned()),
        StorageSelector::Index,
    )
}

fn is_storage_prefix(prefix: &str) -> bool {
    !prefix.is_empty() && !prefix.contains(SEPARATOR)
}

fn split_storage(input: &str) -> (StorageSelector, &str) {
    match input.split_once(STORAGE_DELIMITER) {
        Some((prefix, rest)) if is_storage_prefix(prefix) && rest.starts_with(SEPARATOR) => {
            (parse_selector(prefix), rest)
        }
        _ => (StorageSelector::Default, input),
    }
}

impl FromStr for DevicePath {
    type Err = PathError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (storage, path) = split_storage(input);
        Ok(Self {
            storage,
            path: RemotePath::parse(path)?,
        })
    }
}

impl fmt::Display for StorageSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => Ok(()),
            Self::Index(index) => write!(f, "{index}:"),
            Self::Named(name) => write!(f, "{name}:"),
        }
    }
}

impl fmt::Display for DevicePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.storage, self.path)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "table test enumerating every parse case"
    )]
    fn parses_valid_device_paths() {
        let cases = [
            ("/", StorageSelector::Default, vec![]),
            ("/DCIM", StorageSelector::Default, vec!["DCIM"]),
            (
                "/DCIM/Camera/",
                StorageSelector::Default,
                vec!["DCIM", "Camera"],
            ),
            ("//a//b", StorageSelector::Default, vec!["a", "b"]),
            (
                "Internal:/DCIM",
                StorageSelector::Named("Internal".into()),
                vec!["DCIM"],
            ),
            (
                "sd card:/Music",
                StorageSelector::Named("sd card".into()),
                vec!["Music"],
            ),
            ("1:/Music", StorageSelector::Index(1), vec!["Music"]),
            ("/Fotos/año", StorageSelector::Default, vec!["Fotos", "año"]),
        ];
        for (input, storage, segments) in cases {
            let parsed: DevicePath = input.parse().unwrap();
            assert_eq!(parsed.storage, storage, "{input}");
            assert_eq!(parsed.path.segments(), segments, "{input}");
        }
    }

    #[test]
    fn rejects_invalid_device_paths() {
        let cases = [
            ("", PathError::Empty),
            ("DCIM", PathError::NotAbsolute),
            (":/DCIM", PathError::NotAbsolute),
            ("/a/../b", PathError::DotSegment),
            ("Internal:DCIM", PathError::NotAbsolute),
            ("/a\0b", PathError::InvalidSegment("a\0b".into())),
        ];
        for (input, expected) in cases {
            let err = input.parse::<DevicePath>().unwrap_err();
            assert_eq!(err, expected, "{input}");
        }
    }

    #[test]
    fn digit_prefix_too_large_for_an_index_is_a_name() {
        let parsed: DevicePath = "99999999999999999999:/a".parse().unwrap();
        assert_eq!(
            parsed.storage,
            StorageSelector::Named("99999999999999999999".into())
        );
    }

    #[test]
    fn display_round_trips_through_from_str() {
        let cases = ["/", "/a/b", "Internal:/a/b", "1:/a", "sd card:/Music/año"];
        for input in cases {
            let parsed: DevicePath = input.parse().unwrap();
            let shown = parsed.to_string();
            assert_eq!(shown, input);
            assert_eq!(shown.parse::<DevicePath>().unwrap(), parsed);
        }
    }
}
