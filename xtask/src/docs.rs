use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

const REQUIRED_SECTIONS: [&str; 7] = [
    "学習目標",
    "背景",
    "実装",
    "実行と確認",
    "よくある失敗",
    "演習",
    "次の章",
];

#[derive(Debug, PartialEq, Eq)]
pub enum DocsError {
    Read {
        path: PathBuf,
        message: String,
    },
    MissingLink {
        source: PathBuf,
        destination: PathBuf,
        line: usize,
    },
    MissingSection {
        chapter: PathBuf,
        section: &'static str,
    },
}

impl DocsError {
    pub fn path(&self) -> &Path {
        match self {
            Self::Read { path, .. } => path,
            Self::MissingLink { destination, .. } => destination,
            Self::MissingSection { chapter, .. } => chapter,
        }
    }

    pub fn missing_section(&self) -> &str {
        match self {
            Self::MissingSection { section, .. } => section,
            _ => "",
        }
    }
}

impl fmt::Display for DocsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, message } => {
                write!(formatter, "could not read {}: {message}", path.display())
            }
            Self::MissingLink {
                source,
                destination,
                line,
            } => write!(
                formatter,
                "{}:{line}: missing local link destination {}",
                source.display(),
                destination.display()
            ),
            Self::MissingSection { chapter, section } => write!(
                formatter,
                "{}: missing required guide section {section}",
                chapter.display()
            ),
        }
    }
}

impl std::error::Error for DocsError {}

pub fn check_local_links(root: &Path) -> Result<(), DocsError> {
    for source in markdown_files(root)? {
        let contents = read_text(root, &source)?;
        let mut in_fenced_code = false;
        for (line_index, line) in contents.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                in_fenced_code = !in_fenced_code;
                continue;
            }
            if in_fenced_code {
                continue;
            }
            for destination in inline_destinations(line) {
                if ["http:", "https:", "mailto:"]
                    .iter()
                    .any(|scheme| destination.starts_with(scheme))
                {
                    continue;
                }
                let path_without_anchor = destination.split('#').next().unwrap_or_default();
                if path_without_anchor.is_empty() || Path::new(path_without_anchor).is_absolute() {
                    continue;
                }
                let source_parent = source.parent().unwrap_or_else(|| Path::new(""));
                let resolved = root.join(source_parent).join(path_without_anchor);
                if !resolved.exists() {
                    return Err(DocsError::MissingLink {
                        source,
                        destination: PathBuf::from(path_without_anchor),
                        line: line_index + 1,
                    });
                }
            }
        }
    }
    Ok(())
}

pub fn check_guide_structure(root: &Path) -> Result<(), DocsError> {
    for chapter in markdown_files(&root.join("docs/guide"))? {
        let relative = Path::new("docs/guide").join(&chapter);
        let Some(file_name) = chapter.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_numbered_chapter(file_name) {
            continue;
        }
        let contents = read_text(root, &relative)?;
        for section in REQUIRED_SECTIONS {
            if !contents
                .lines()
                .any(|line| line.trim() == format!("## {section}"))
            {
                return Err(DocsError::MissingSection {
                    chapter: relative,
                    section,
                });
            }
        }
    }
    Ok(())
}

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>, DocsError> {
    let mut files = Vec::new();
    collect_markdown_files(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_markdown_files(
    scan_root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), DocsError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DocsError::Read {
                path: directory.to_owned(),
                message: error.to_string(),
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| DocsError::Read {
            path: directory.to_owned(),
            message: error.to_string(),
        })?;
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_markdown_files(scan_root, &path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            files.push(
                path.strip_prefix(scan_root)
                    .expect("walked path must remain below scan root")
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn read_text(root: &Path, relative: &Path) -> Result<String, DocsError> {
    fs::read_to_string(root.join(relative)).map_err(|error| DocsError::Read {
        path: relative.to_owned(),
        message: error.to_string(),
    })
}

fn inline_destinations(line: &str) -> Vec<&str> {
    let mut destinations = Vec::new();
    let mut remainder = line;
    while let Some(label_end) = remainder.find("](") {
        remainder = &remainder[label_end + 2..];
        let Some(destination_end) = remainder.find(')') else {
            break;
        };
        destinations.push(&remainder[..destination_end]);
        remainder = &remainder[destination_end + 1..];
    }
    destinations
}

fn is_numbered_chapter(file_name: &str) -> bool {
    let bytes = file_name.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_digit()
        || !bytes[1].is_ascii_digit()
        || bytes[2] != b'-'
    {
        return false;
    }
    let chapter = usize::from(bytes[0] - b'0') * 10 + usize::from(bytes[1] - b'0');
    (1..=12).contains(&chapter)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_TREE_ID: AtomicU64 = AtomicU64::new(0);

    struct TestTree {
        root: PathBuf,
    }

    impl TestTree {
        fn new() -> Self {
            let id = NEXT_TREE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("minios-docs-{}-{id}", process::id()));
            fs::create_dir_all(&root).expect("must create documentation fixture directory");
            Self { root }
        }

        fn path(&self) -> &Path {
            &self.root
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("must create documentation fixture parent");
            }
            fs::write(path, contents).expect("must write documentation fixture");
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn reports_a_missing_relative_markdown_link() {
        let temp = TestTree::new();
        temp.write("README.md", "[missing](docs/missing.md)\n");
        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("docs/missing.md"));
    }

    #[test]
    fn missing_link_diagnostic_names_source_destination_and_line() {
        let temp = TestTree::new();
        temp.write("docs/index.md", "# Index\n[missing](missing.md)\n");

        let diagnostic = check_local_links(temp.path()).unwrap_err().to_string();

        assert!(diagnostic.contains("docs/index.md:2"));
        assert!(diagnostic.contains("missing.md"));
    }

    #[test]
    fn accepts_existing_relative_markdown_links_and_anchors() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](docs/guide.md#start)\n");
        temp.write("docs/guide.md", "# Start\n");
        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn resolves_relative_destination_from_the_source_directory() {
        let temp = TestTree::new();
        temp.write("docs/案内.md", "[用語集](用語集.md)\n");
        temp.write("docs/用語集.md", "# 用語集\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn ignores_markdown_below_git_and_target_directories() {
        let temp = TestTree::new();
        temp.write(".git/notes.md", "[missing](missing.md)\n");
        temp.write("target/generated/docs.md", "[missing](missing.md)\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn ignores_http_https_and_mailto_destinations() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "[http](http://example.com) [https](https://example.com) [mail](mailto:learner@example.com)\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn ignores_absolute_markdown_destinations() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "[site-root](/not-a-repository-relative-document.md)\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn ignores_inline_destination_inside_a_fenced_code_block() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "```rust\nlet fixture = \"[missing](docs/missing.md)\";\n```\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn rejects_a_guide_chapter_missing_required_sections() {
        let temp = TestTree::new();
        temp.write("docs/guide/01-example.md", "# Example\n## 学習目標\n");
        let error = check_guide_structure(temp.path()).unwrap_err();
        assert_eq!(error.missing_section(), "背景");
    }

    #[test]
    fn every_required_guide_section_is_mandatory() {
        let sections = [
            "学習目標",
            "背景",
            "実装",
            "実行と確認",
            "よくある失敗",
            "演習",
            "次の章",
        ];

        for omitted in sections {
            let temp = TestTree::new();
            let mut chapter = String::from("# Example\n");
            for section in sections {
                if section != omitted {
                    chapter.push_str(&format!("## {section}\ncontent\n"));
                }
            }
            temp.write("docs/guide/01-example.md", &chapter);

            let error = check_guide_structure(temp.path()).unwrap_err();
            assert_eq!(error.missing_section(), omitted);
        }
    }

    #[test]
    fn guide_structure_scope_is_chapters_one_through_twelve() {
        let temp = TestTree::new();
        temp.write("docs/guide/13-not-a-milestone-chapter.md", "# Appendix\n");

        assert_eq!(check_guide_structure(temp.path()), Ok(()));
    }
}
