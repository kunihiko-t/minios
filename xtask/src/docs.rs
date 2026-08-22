use std::{
    fmt, fs, io,
    path::{Component, Path, PathBuf},
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

const REQUIRED_CHAPTERS: [&str; 12] = [
    "01-introduction.md",
    "02-setup.md",
    "03-no-std-and-linking.md",
    "04-boot-with-opensbi.md",
    "05-uart.md",
    "06-panic-and-diagnostics.md",
    "07-traps-and-interrupts.md",
    "08-timer-interrupts.md",
    "09-physical-memory.md",
    "10-shell.md",
    "11-test-harness.md",
    "12-next-steps.md",
];

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
}

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
    EscapesRoot {
        source: PathBuf,
        destination: PathBuf,
        line: usize,
    },
    MissingSection {
        chapter: PathBuf,
        section: &'static str,
    },
    MissingChapter {
        chapter: PathBuf,
    },
}

impl DocsError {
    pub fn path(&self) -> &Path {
        match self {
            Self::Read { path, .. } => path,
            Self::MissingLink { destination, .. } | Self::EscapesRoot { destination, .. } => {
                destination
            }
            Self::MissingSection { chapter, .. } => chapter,
            Self::MissingChapter { chapter } => chapter,
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
            Self::EscapesRoot {
                source,
                destination,
                line,
            } => write!(
                formatter,
                "{}:{line}: local link destination escapes repository root: {}",
                source.display(),
                destination.display()
            ),
            Self::MissingSection { chapter, section } => write!(
                formatter,
                "{}: missing required guide section {section}",
                chapter.display()
            ),
            Self::MissingChapter { chapter } => {
                write!(
                    formatter,
                    "{}: missing required guide chapter",
                    chapter.display()
                )
            }
        }
    }
}

impl std::error::Error for DocsError {}

pub fn check_local_links(root: &Path) -> Result<(), DocsError> {
    let canonical_root = fs::canonicalize(root).map_err(|error| DocsError::Read {
        path: root.to_owned(),
        message: error.to_string(),
    })?;
    for source in markdown_files(&canonical_root)? {
        let contents = read_text(&canonical_root, &source)?;
        let mut fence = None;
        for (line_index, line) in contents.lines().enumerate() {
            if update_fence(line, &mut fence) {
                continue;
            }
            if fence.is_some() {
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
                let destination = PathBuf::from(path_without_anchor);
                let Some(relative) = normalize_relative(&source_parent.join(&destination)) else {
                    return Err(DocsError::EscapesRoot {
                        source,
                        destination,
                        line: line_index + 1,
                    });
                };
                let resolved = canonical_root.join(relative);
                if !resolved.exists() {
                    return Err(DocsError::MissingLink {
                        source,
                        destination,
                        line: line_index + 1,
                    });
                }
                let canonical_destination =
                    fs::canonicalize(&resolved).map_err(|error| DocsError::Read {
                        path: resolved.clone(),
                        message: error.to_string(),
                    })?;
                if !canonical_destination.starts_with(&canonical_root) {
                    return Err(DocsError::EscapesRoot {
                        source,
                        destination,
                        line: line_index + 1,
                    });
                }
            }
        }
    }
    Ok(())
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

pub fn check_guide_structure(root: &Path) -> Result<(), DocsError> {
    let guide_root = root.join("docs/guide");
    fs::read_dir(&guide_root).map_err(|error| DocsError::Read {
        path: guide_root,
        message: error.to_string(),
    })?;
    for chapter in REQUIRED_CHAPTERS {
        let relative = Path::new("docs/guide").join(chapter);
        if !root.join(&relative).is_file() {
            return Err(DocsError::MissingChapter { chapter: relative });
        }
        let contents = read_text(root, &relative)?;
        for section in REQUIRED_SECTIONS {
            if !has_heading(&contents, section) {
                return Err(DocsError::MissingSection {
                    chapter: relative,
                    section,
                });
            }
        }
    }
    Ok(())
}

fn has_heading(contents: &str, heading: &str) -> bool {
    let expected = format!("## {heading}");
    let mut fence = None;
    contents.lines().any(|line| {
        if update_fence(line, &mut fence) || fence.is_some() {
            return false;
        }
        commonmark_content(line).is_some_and(|content| content.trim_end() == expected)
    })
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
        let file_type = entry.file_type().map_err(|error| DocsError::Read {
            path: path.clone(),
            message: error.to_string(),
        })?;
        if file_type.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_markdown_files(scan_root, &path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
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

fn update_fence(line: &str, fence: &mut Option<Fence>) -> bool {
    let Some(content) = commonmark_content(line) else {
        return false;
    };
    let Some(marker) = content.as_bytes().first().copied() else {
        return false;
    };
    if marker != b'`' && marker != b'~' {
        return false;
    }
    let length = content
        .as_bytes()
        .iter()
        .take_while(|candidate| **candidate == marker)
        .count();
    if length < 3 {
        return false;
    }
    match *fence {
        None => {
            *fence = Some(Fence { marker, length });
            true
        }
        Some(open) => {
            if marker == open.marker && length >= open.length && content[length..].trim().is_empty()
            {
                *fence = None;
            }
            true
        }
    }
}

fn commonmark_content(line: &str) -> Option<&str> {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    (indentation <= 3).then(|| &line[indentation..])
}

fn inline_destinations(line: &str) -> Vec<String> {
    let characters: Vec<char> = line.chars().collect();
    let mut destinations = Vec::new();
    let mut cursor = 0;
    while cursor + 1 < characters.len() {
        if characters[cursor] != ']' || characters[cursor + 1] != '(' {
            cursor += 1;
            continue;
        }
        cursor += 2;
        if let Some((destination, next)) = parse_inline_destination(&characters, cursor) {
            destinations.push(destination);
            cursor = next;
        }
    }
    destinations
}

fn parse_inline_destination(characters: &[char], mut cursor: usize) -> Option<(String, usize)> {
    while characters
        .get(cursor)
        .is_some_and(|character| character.is_whitespace())
    {
        cursor += 1;
    }
    let character = *characters.get(cursor)?;
    if character == ')' {
        return Some((String::new(), cursor + 1));
    }
    if character == '<' {
        let (destination, suffix_start) = parse_angle_destination(characters, cursor + 1)?;
        let end = consume_link_suffix(characters, suffix_start)?;
        return Some((destination, end));
    }

    let (destination, suffix_start) = parse_bare_destination(characters, cursor)?;
    match suffix_start {
        LinkSuffix::Consumed(end) => Some((destination, end)),
        LinkSuffix::Pending(start) => Some((destination, consume_link_suffix(characters, start)?)),
    }
}

fn parse_angle_destination(characters: &[char], mut cursor: usize) -> Option<(String, usize)> {
    let mut destination = String::new();
    while let Some(character) = characters.get(cursor).copied() {
        match character {
            '\\' => {
                cursor += 1;
                destination.push(*characters.get(cursor)?);
            }
            '>' => return Some((destination, cursor + 1)),
            _ => destination.push(character),
        }
        cursor += 1;
    }
    None
}

enum LinkSuffix {
    Consumed(usize),
    Pending(usize),
}

fn parse_bare_destination(characters: &[char], mut cursor: usize) -> Option<(String, LinkSuffix)> {
    let mut destination = String::new();
    let mut parentheses = 0usize;
    while let Some(character) = characters.get(cursor).copied() {
        match character {
            '\\' => {
                cursor += 1;
                destination.push(*characters.get(cursor)?);
            }
            '(' => {
                parentheses += 1;
                destination.push(character);
            }
            ')' if parentheses == 0 => {
                return Some((destination, LinkSuffix::Consumed(cursor + 1)));
            }
            ')' => {
                parentheses -= 1;
                destination.push(character);
            }
            character if character.is_whitespace() && parentheses == 0 => {
                return Some((destination, LinkSuffix::Pending(cursor)));
            }
            _ => destination.push(character),
        }
        cursor += 1;
    }
    None
}

fn consume_link_suffix(characters: &[char], mut cursor: usize) -> Option<usize> {
    let before_whitespace = cursor;
    while characters
        .get(cursor)
        .is_some_and(|character| character.is_whitespace())
    {
        cursor += 1;
    }
    if characters.get(cursor) == Some(&')') {
        return Some(cursor + 1);
    }
    if cursor == before_whitespace {
        return None;
    }

    let closing = match characters.get(cursor)? {
        '"' => '"',
        '\'' => '\'',
        '(' => ')',
        _ => return None,
    };
    cursor += 1;
    loop {
        match characters.get(cursor).copied()? {
            '\\' => cursor += 2,
            character if character == closing => {
                cursor += 1;
                break;
            }
            _ => cursor += 1,
        }
    }
    while characters
        .get(cursor)
        .is_some_and(|character| character.is_whitespace())
    {
        cursor += 1;
    }
    (characters.get(cursor) == Some(&')')).then_some(cursor + 1)
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
        container: PathBuf,
        root: PathBuf,
    }

    impl TestTree {
        fn new() -> Self {
            let id = NEXT_TREE_ID.fetch_add(1, Ordering::Relaxed);
            let container =
                std::env::temp_dir().join(format!("minios-docs-{}-{id}", process::id()));
            let root = container.join("repository");
            fs::create_dir_all(&root).expect("must create documentation fixture directory");
            Self { container, root }
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

        fn write_outside(&self, relative: &str, contents: &str) {
            let path = self.container.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("must create outside fixture parent");
            }
            fs::write(path, contents).expect("must write outside fixture");
        }

        fn remove(&self, relative: &str) {
            fs::remove_file(self.root.join(relative)).expect("must remove documentation fixture");
        }

        fn write_complete_guide(&self) {
            for chapter in REQUIRED_CHAPTERS {
                self.write(
                    &format!("docs/guide/{chapter}"),
                    &complete_chapter("# Chapter\n"),
                );
            }
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.container);
        }
    }

    fn complete_chapter(prefix: &str) -> String {
        let mut chapter = String::from(prefix);
        for section in REQUIRED_SECTIONS {
            chapter.push_str(&format!("## {section}\ncontent\n"));
        }
        chapter
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
    fn accepts_parent_segments_that_remain_inside_repository() {
        let temp = TestTree::new();
        temp.write("docs/guide/chapter.md", "[用語集](../glossary.md)\n");
        temp.write("docs/glossary.md", "# 用語集\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_repository_root_given_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TestTree::new();
        temp.write("README.md", "[guide](guide.md)\n");
        temp.write("guide.md", "# Guide\n");
        let linked_root = temp.container.join("linked-repository");
        symlink(temp.path(), &linked_root).expect("must create repository root symlink fixture");

        assert_eq!(check_local_links(&linked_root), Ok(()));
    }

    #[test]
    fn rejects_parent_segments_that_escape_repository() {
        let temp = TestTree::new();
        temp.write("README.md", "[outside](../outside.md)\n");
        temp.write_outside("outside.md", "# Outside\n");

        let error = check_local_links(temp.path()).unwrap_err();
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("README.md:1"));
        assert!(diagnostic.contains("../outside.md"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_file_symlink_destination_outside_repository() {
        use std::os::unix::fs::symlink;

        let temp = TestTree::new();
        temp.write("README.md", "[outside](linked.md)\n");
        temp.write_outside("outside.md", "# Outside\n");
        symlink(
            temp.container.join("outside.md"),
            temp.root.join("linked.md"),
        )
        .expect("must create file symlink fixture");

        let diagnostic = check_local_links(temp.path()).unwrap_err().to_string();
        assert!(diagnostic.contains("README.md:1"));
        assert!(diagnostic.contains("linked.md"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_directory_symlink_destination_outside_repository() {
        use std::os::unix::fs::symlink;

        let temp = TestTree::new();
        temp.write("README.md", "[outside](linked/outside.md)\n");
        temp.write_outside("outside/destination.md", "# Outside\n");
        symlink(temp.container.join("outside"), temp.root.join("linked"))
            .expect("must create directory symlink fixture");

        let diagnostic = check_local_links(temp.path()).unwrap_err().to_string();
        assert!(diagnostic.contains("README.md:1"));
        assert!(diagnostic.contains("linked/outside.md"));
    }

    #[cfg(unix)]
    #[test]
    fn does_not_traverse_directory_symlinks_while_finding_markdown() {
        use std::os::unix::fs::symlink;

        let temp = TestTree::new();
        temp.write("README.md", "# Repository\n");
        temp.write_outside("outside/broken.md", "[missing](missing.md)\n");
        symlink(temp.container.join("outside"), temp.root.join("linked"))
            .expect("must create directory symlink fixture");

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
    fn ignores_inline_destination_inside_a_tilde_fenced_code_block() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "~~~~rust\nlet fixture = \"[missing](docs/missing.md)\";\n~~~~\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn shorter_marker_does_not_close_a_fenced_code_block() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "~~~~markdown\n~~~\n[missing](docs/missing.md)\n~~~~\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn four_space_indented_backticks_do_not_open_a_fence() {
        let temp = TestTree::new();
        temp.write("README.md", "    ```rust\n[missing](docs/missing.md)\n");

        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("docs/missing.md"));
    }

    #[test]
    fn four_space_indented_tildes_do_not_open_a_fence() {
        let temp = TestTree::new();
        temp.write("README.md", "    ~~~rust\n[missing](docs/missing.md)\n");

        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("docs/missing.md"));
    }

    #[test]
    fn tab_indented_marker_does_not_open_a_fence() {
        let temp = TestTree::new();
        temp.write("README.md", "\t```rust\n[missing](docs/missing.md)\n");

        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("docs/missing.md"));
    }

    #[test]
    fn four_space_indented_marker_does_not_close_a_fence() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "~~~~rust\n    ~~~~\n[missing](docs/missing.md)\n~~~~\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn three_space_indented_markers_delimit_a_fence() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "   ```rust\n[missing](docs/missing.md)\n   ```\n",
        );

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn accepts_inline_destination_followed_by_a_title() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](guide.md \"学習ガイド\")\n");
        temp.write("guide.md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn does_not_parse_a_link_inside_a_double_quoted_title() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](guide.md \"see ](missing.md)\")\n");
        temp.write("guide.md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn does_not_parse_a_link_inside_a_single_quoted_title() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](guide.md 'see ](missing.md)')\n");
        temp.write("guide.md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn accepts_a_parenthesized_title_with_escaped_delimiters() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](missing.md (see \\(details\\)))\n");

        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("missing.md"));
    }

    #[test]
    fn escaped_quote_does_not_end_a_double_quoted_title() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "[guide](guide.md \"quoted \\\" ](missing.md)\")\n",
        );
        temp.write("guide.md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn finds_a_second_link_after_a_titled_link() {
        let temp = TestTree::new();
        temp.write(
            "README.md",
            "[guide](guide.md \"see it\") [missing](missing.md)\n",
        );
        temp.write("guide.md", "# Guide\n");

        let error = check_local_links(temp.path()).unwrap_err();
        assert_eq!(error.path(), Path::new("missing.md"));
    }

    #[test]
    fn malformed_title_suffix_is_not_treated_as_a_link() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](missing.md \"title\" trailing)\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn unterminated_escaped_title_does_not_panic() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](missing.md \"unterminated\\\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn accepts_angle_bracket_destination_with_spaces() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](<docs/file name.md>)\n");
        temp.write("docs/file name.md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn accepts_destination_with_escaped_parentheses() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](docs/guide\\(ja\\).md)\n");
        temp.write("docs/guide(ja).md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn accepts_destination_with_nested_parentheses() {
        let temp = TestTree::new();
        temp.write("README.md", "[guide](docs/guide(ja).md)\n");
        temp.write("docs/guide(ja).md", "# Guide\n");

        assert_eq!(check_local_links(temp.path()), Ok(()));
    }

    #[test]
    fn rejects_a_guide_chapter_missing_required_sections() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        temp.write("docs/guide/01-introduction.md", "# Example\n## 学習目標\n");
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
            temp.write_complete_guide();
            let mut chapter = String::from("# Example\n");
            for section in sections {
                if section != omitted {
                    chapter.push_str(&format!("## {section}\ncontent\n"));
                }
            }
            temp.write("docs/guide/01-introduction.md", &chapter);

            let error = check_guide_structure(temp.path()).unwrap_err();
            assert_eq!(error.missing_section(), omitted);
        }
    }

    #[test]
    fn guide_structure_scope_is_chapters_one_through_twelve() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        temp.write("docs/guide/13-not-a-milestone-chapter.md", "# Appendix\n");

        assert_eq!(check_guide_structure(temp.path()), Ok(()));
    }

    #[test]
    fn rejects_a_missing_guide_root() {
        let temp = TestTree::new();

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert!(error.path().ends_with("docs/guide"));
    }

    #[test]
    fn rejects_a_missing_required_chapter() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        temp.remove("docs/guide/07-traps-and-interrupts.md");

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert!(error.path().ends_with("07-traps-and-interrupts.md"));
        assert!(error.to_string().contains("missing required guide chapter"));
    }

    #[test]
    fn ignores_required_heading_inside_a_tilde_fence() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        let chapter = complete_chapter("# Chapter\n~~~~markdown\n## 背景\n~~~~\n");
        let chapter = chapter.replacen("## 背景\ncontent\n", "", 1);
        temp.write("docs/guide/01-introduction.md", &chapter);

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert_eq!(error.missing_section(), "背景");
    }

    #[test]
    fn level_three_heading_does_not_satisfy_required_section() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        let chapter = complete_chapter("# Chapter\n### 背景\n");
        let chapter = chapter.replacen("## 背景\ncontent\n", "", 1);
        temp.write("docs/guide/01-introduction.md", &chapter);

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert_eq!(error.missing_section(), "背景");
    }

    #[test]
    fn four_space_indented_heading_does_not_satisfy_required_section() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        let chapter = complete_chapter("# Chapter\n    ## 背景\n");
        let chapter = chapter.replacen("## 背景\ncontent\n", "", 1);
        temp.write("docs/guide/01-introduction.md", &chapter);

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert_eq!(error.missing_section(), "背景");
    }

    #[test]
    fn tab_indented_heading_does_not_satisfy_required_section() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        let chapter = complete_chapter("# Chapter\n\t## 背景\n");
        let chapter = chapter.replacen("## 背景\ncontent\n", "", 1);
        temp.write("docs/guide/01-introduction.md", &chapter);

        let error = check_guide_structure(temp.path()).unwrap_err();
        assert_eq!(error.missing_section(), "背景");
    }

    #[test]
    fn three_space_indented_heading_satisfies_required_section() {
        let temp = TestTree::new();
        temp.write_complete_guide();
        let chapter = complete_chapter("# Chapter\n   ## 背景\n");
        let chapter = chapter.replacen("## 背景\ncontent\n", "", 1);
        temp.write("docs/guide/01-introduction.md", &chapter);

        assert_eq!(check_guide_structure(temp.path()), Ok(()));
    }
}
