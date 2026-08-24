pub const MANIFEST_MAX_LEN: usize = 4 * 1024;
pub const NAME_MAX_LEN: usize = 128;
pub const ARG_MAX_COUNT: usize = 16;
pub const ARG_MAX_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest<'a> {
    source: &'a str,
    name: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    TooLong,
    InvalidUtf8,
    MissingTrailingLf,
    MissingVersion,
    DuplicateVersion,
    MissingName,
    InvalidOrder,
    EmptyName,
    NameTooLong,
    InvalidName,
    UnknownKey,
    TooManyArgs,
    ArgumentTooLong,
    ArgumentContainsNul,
}

impl<'a> Manifest<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MANIFEST_MAX_LEN {
            return Err(ManifestError::TooLong);
        }

        let source = core::str::from_utf8(bytes).map_err(|_| ManifestError::InvalidUtf8)?;
        let source = source
            .strip_suffix('\n')
            .ok_or(ManifestError::MissingTrailingLf)?;
        if source.strip_suffix('\n').is_some() {
            return Err(ManifestError::UnknownKey);
        }
        let mut lines = source.split('\n');

        match lines.next() {
            Some("version=1") => {}
            Some(line)
                if line.strip_prefix("name=").is_some() || line.strip_prefix("arg=").is_some() =>
            {
                return Err(ManifestError::InvalidOrder);
            }
            _ => return Err(ManifestError::MissingVersion),
        }

        let name = match lines.next() {
            Some(line) => match line.strip_prefix("name=") {
                Some(name) => name,
                None if line == "version=1" => return Err(ManifestError::DuplicateVersion),
                None => return Err(ManifestError::InvalidOrder),
            },
            None => return Err(ManifestError::MissingName),
        };
        if name.is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if name.len() > NAME_MAX_LEN {
            return Err(ManifestError::NameTooLong);
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ManifestError::InvalidName);
        }

        let mut argument_count = 0;
        for line in lines {
            if line == "version=1" {
                return Err(ManifestError::DuplicateVersion);
            }
            if line.strip_prefix("name=").is_some() {
                return Err(ManifestError::InvalidOrder);
            }

            let argument = line.strip_prefix("arg=").ok_or(ManifestError::UnknownKey)?;
            argument_count += 1;
            if argument_count > ARG_MAX_COUNT {
                return Err(ManifestError::TooManyArgs);
            }
            if argument.len() > ARG_MAX_LEN {
                return Err(ManifestError::ArgumentTooLong);
            }
            if argument.as_bytes().contains(&0) {
                return Err(ManifestError::ArgumentContainsNul);
            }
        }

        Ok(Self { source, name })
    }

    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub fn args(&self) -> ManifestArgs<'a> {
        let mut lines = self.source.split('\n');
        let _ = lines.next();
        let _ = lines.next();
        ManifestArgs { lines }
    }
}

#[derive(Debug, Clone)]
pub struct ManifestArgs<'a> {
    lines: core::str::Split<'a, char>,
}

impl<'a> Iterator for ManifestArgs<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next()?.strip_prefix("arg=")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use self::std::{string::String, vec, vec::Vec};

    #[test]
    fn parses_name_and_reiterable_arguments() {
        let bytes = b"version=1\nname=hello\narg=first\narg=second\n";

        let manifest = Manifest::parse(bytes).unwrap();

        assert_eq!(manifest.name(), "hello");
        assert_eq!(manifest.args().collect::<Vec<_>>(), vec!["first", "second"]);
        assert_eq!(manifest.args().collect::<Vec<_>>(), vec!["first", "second"]);
    }

    #[test]
    fn name_and_arguments_borrow_from_source() {
        let bytes = b"version=1\nname=hello\narg=first\n";

        let manifest = Manifest::parse(bytes).unwrap();
        let argument = manifest.args().next().unwrap();

        assert_eq!(manifest.name().as_ptr(), bytes[15..20].as_ptr());
        assert_eq!(argument.as_ptr(), bytes[25..30].as_ptr());
    }

    #[test]
    fn accepts_name_at_128_byte_limit() {
        let bytes = concat!(
            "version=1\nname=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\n",
        )
        .as_bytes();

        assert_eq!(Manifest::parse(bytes).unwrap().name().len(), 128);
    }

    #[test]
    fn accepts_allowed_name_punctuation() {
        let bytes = b"version=1\nname=hello.world_1-test\n";

        assert_eq!(Manifest::parse(bytes).unwrap().name(), "hello.world_1-test");
    }

    #[test]
    fn rejects_crlf_version_line() {
        let bytes = b"version=1\r\nname=hello\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingVersion));
    }

    #[test]
    fn rejects_crlf_name_line() {
        let bytes = b"version=1\nname=hello\r\narg=first\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::InvalidName));
    }

    #[test]
    fn rejects_crlf_name_length_bypass() {
        let bytes = concat!(
            "version=1\nname=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\r\narg=first\n",
        )
        .as_bytes();

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::NameTooLong));
    }

    #[test]
    fn rejects_crlf_argument_length_bypass() {
        let bytes = concat!(
            "version=1\nname=hello\narg=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\r\narg=tail\n",
        )
        .as_bytes();

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::ArgumentTooLong));
    }

    #[test]
    fn preserves_carriage_return_in_crlf_argument() {
        let bytes = b"version=1\nname=hello\narg=first\r\narg=second\n";

        let manifest = Manifest::parse(bytes).unwrap();

        assert_eq!(
            manifest.args().collect::<Vec<_>>(),
            vec!["first\r", "second"]
        );
    }

    #[test]
    fn accepts_16_arguments() {
        let bytes = b"version=1\nname=hello\n\
arg=\narg=\narg=\narg=\narg=\narg=\narg=\narg=\n\
arg=\narg=\narg=\narg=\narg=\narg=\narg=\narg=\n";

        assert_eq!(Manifest::parse(bytes).unwrap().args().count(), 16);
    }

    #[test]
    fn accepts_argument_at_256_byte_limit() {
        let bytes = concat!(
            "version=1\nname=hello\narg=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\n",
        )
        .as_bytes();

        assert_eq!(
            Manifest::parse(bytes).unwrap().args().next().unwrap().len(),
            256
        );
    }

    #[test]
    fn accepts_manifest_at_four_kibibyte_limit() {
        let mut source = String::from("version=1\nname=a\n");
        for _ in 0..15 {
            source.push_str("arg=");
            source.push_str(&"a".repeat(256));
            source.push('\n');
        }
        source.push_str("arg=");
        source.push_str(&"a".repeat(159));
        source.push('\n');
        assert_eq!(source.len(), 4_096);

        let manifest = Manifest::parse(source.as_bytes()).unwrap();

        assert_eq!(manifest.name(), "a");
        assert_eq!(manifest.args().count(), 16);
    }

    #[test]
    fn rejects_manifest_larger_than_four_kibibytes() {
        let bytes = [b'a'; MANIFEST_MAX_LEN + 1];

        assert_eq!(Manifest::parse(&bytes), Err(ManifestError::TooLong));
    }

    #[test]
    fn rejects_invalid_utf8() {
        let bytes = b"version=1\nname=hello\narg=\xff\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::InvalidUtf8));
    }

    #[test]
    fn rejects_manifest_without_trailing_lf() {
        let bytes = b"version=1\nname=hello";

        assert_eq!(
            Manifest::parse(bytes),
            Err(ManifestError::MissingTrailingLf)
        );
    }

    #[test]
    fn rejects_missing_version_line() {
        let bytes = b"\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingVersion));
    }

    #[test]
    fn rejects_non_v1_version() {
        let bytes = b"version=2\nname=hello\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingVersion));
    }

    #[test]
    fn rejects_duplicate_version_line() {
        let bytes = b"version=1\nname=hello\nversion=1\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::DuplicateVersion));
    }

    #[test]
    fn rejects_version_and_name_in_the_wrong_order() {
        let bytes = b"name=hello\nversion=1\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::InvalidOrder));
    }

    #[test]
    fn rejects_missing_name_line() {
        let bytes = b"version=1\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingName));
    }

    #[test]
    fn rejects_empty_name() {
        let bytes = b"version=1\nname=\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::EmptyName));
    }

    #[test]
    fn rejects_name_larger_than_128_bytes() {
        let bytes = concat!(
            "version=1\nname=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "a\n",
        )
        .as_bytes();

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::NameTooLong));
    }

    #[test]
    fn rejects_forbidden_character_in_name() {
        let bytes = b"version=1\nname=hello world\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::InvalidName));
    }

    #[test]
    fn rejects_unknown_key() {
        let bytes = b"version=1\nname=hello\nenv=KEY=value\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::UnknownKey));
    }

    #[test]
    fn rejects_blank_line_after_name() {
        let bytes = b"version=1\nname=hello\n\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::UnknownKey));
    }

    #[test]
    fn rejects_more_than_16_arguments() {
        let bytes = b"version=1\nname=hello\n\
arg=\narg=\narg=\narg=\narg=\narg=\narg=\narg=\n\
arg=\narg=\narg=\narg=\narg=\narg=\narg=\narg=\narg=\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::TooManyArgs));
    }

    #[test]
    fn rejects_argument_larger_than_256_bytes() {
        let bytes = concat!(
            "version=1\nname=hello\narg=",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "a\n",
        )
        .as_bytes();

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::ArgumentTooLong));
    }

    #[test]
    fn rejects_nul_in_argument() {
        let bytes = b"version=1\nname=hello\narg=before\0after\n";

        assert_eq!(
            Manifest::parse(bytes),
            Err(ManifestError::ArgumentContainsNul)
        );
    }
}
