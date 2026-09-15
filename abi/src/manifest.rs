use core::ops::Range;

pub const MANIFEST_MAX_LEN: usize = 4 * 1024;
pub const NAME_MAX_LEN: usize = 128;
pub const ARG_MAX_COUNT: usize = 16;
pub const ARG_MAX_LEN: usize = 256;
/// 1 bundleが起動しうるimageの上限。kernelのprocess table容量と同じ値である。
pub const IMAGE_MAX_COUNT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest<'a> {
    source: &'a str,
    version: u8,
    name: &'a str,
}

/// manifestが宣言する1 image。`elf`は`header.elf`領域先頭からの相対rangeで、
/// `version=1`では`None` (領域全体) を意味する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestImage<'a> {
    name: &'a str,
    args_source: &'a str,
    elf: Option<Range<u64>>,
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
    ArgumentContainsCarriageReturn,
    MissingImage,
    TooManyImages,
    MissingElf,
    MalformedElf,
    OverlappingElfRange,
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

        let version = match lines.next() {
            Some("version=1") => 1,
            Some("version=2") => 2,
            Some(line) if line.starts_with("version=") => {
                return Err(ManifestError::MissingVersion);
            }
            Some(line)
                if line.strip_prefix("name=").is_some()
                    || line.strip_prefix("arg=").is_some()
                    || line.strip_prefix("image=").is_some() =>
            {
                return Err(ManifestError::InvalidOrder);
            }
            _ => return Err(ManifestError::MissingVersion),
        };

        let name = if version == 1 {
            Self::parse_v1(lines)?
        } else {
            Self::parse_v2(lines)?
        };

        Ok(Self {
            source,
            version,
            name,
        })
    }

    // version=1: `name=`行、続く`arg=`行だけを受理する。
    fn parse_v1(mut lines: core::str::Split<'a, char>) -> Result<&'a str, ManifestError> {
        let name = match lines.next() {
            Some(line) => match line.strip_prefix("name=") {
                Some(name) => name,
                None if line.starts_with("version=") => {
                    return Err(ManifestError::DuplicateVersion);
                }
                None => return Err(ManifestError::InvalidOrder),
            },
            None => return Err(ManifestError::MissingName),
        };
        validate_name(name)?;

        let mut argument_count = 0;
        for line in lines {
            if line.starts_with("version=") {
                return Err(ManifestError::DuplicateVersion);
            }
            if line.strip_prefix("name=").is_some() || line.strip_prefix("image=").is_some() {
                return Err(ManifestError::InvalidOrder);
            }

            let argument = line.strip_prefix("arg=").ok_or(ManifestError::UnknownKey)?;
            validate_argument(argument, &mut argument_count)?;
        }

        Ok(name)
    }

    // version=2: `image=`行で始まるsectionの繰り返し。各sectionは
    // `arg=`行(任意)の後に必須の`elf=<offset>,<len>`行で閉じる。
    // 最初のimage名を`name`として返し、rangeの重複はここで拒否する。
    // 領域上限への収まりは領域長を知るkernel側が検査する。
    fn parse_v2(lines: core::str::Split<'a, char>) -> Result<&'a str, ManifestError> {
        let mut first_name = None;
        let mut image_count = 0usize;
        let mut has_elf = false;
        let mut argument_count = 0usize;
        let mut ranges: [(u64, u64); IMAGE_MAX_COUNT] = [(0, 0); IMAGE_MAX_COUNT];

        for line in lines {
            if let Some(name) = line.strip_prefix("image=") {
                if image_count > 0 && !has_elf {
                    return Err(ManifestError::MissingElf);
                }
                validate_name(name)?;
                if image_count == IMAGE_MAX_COUNT {
                    return Err(ManifestError::TooManyImages);
                }
                image_count += 1;
                has_elf = false;
                argument_count = 0;
                if first_name.is_none() {
                    first_name = Some(name);
                }
            } else if let Some(argument) = line.strip_prefix("arg=") {
                if image_count == 0 || has_elf {
                    return Err(ManifestError::InvalidOrder);
                }
                validate_argument(argument, &mut argument_count)?;
            } else if let Some(spec) = line.strip_prefix("elf=") {
                if image_count == 0 || has_elf {
                    return Err(ManifestError::InvalidOrder);
                }
                let range = parse_elf_range(spec)?;
                if ranges[..image_count - 1]
                    .iter()
                    .any(|&(offset, len)| range.0 < offset + len && offset < range.0 + range.1)
                {
                    return Err(ManifestError::OverlappingElfRange);
                }
                ranges[image_count - 1] = range;
                has_elf = true;
            } else if line.starts_with("version=") {
                return Err(ManifestError::DuplicateVersion);
            } else {
                return Err(ManifestError::UnknownKey);
            }
        }

        if image_count == 0 {
            return Err(ManifestError::MissingImage);
        }
        if !has_elf {
            return Err(ManifestError::MissingElf);
        }
        Ok(first_name.expect("image_count > 0 implies a first image"))
    }

    /// manifest format version (1 or 2) を返す。
    pub const fn version(&self) -> u8 {
        self.version
    }

    /// v1では`name=`の値、v2では最初のimage名を返す。
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// v1の`arg=`列を順に返す。v2では[`Self::images`]の各imageがargsを持つ。
    pub fn args(&self) -> ManifestArgs<'a> {
        let mut lines = self.source.split('\n');
        let _ = lines.next();
        let _ = lines.next();
        ManifestArgs {
            lines,
            stop_at_image: false,
        }
    }

    /// manifestが宣言するimageを順に返す。v1ではELF領域全体を占める
    /// 単一imageを1要素だけyieldする。
    pub fn images(&self) -> ManifestImages<'a> {
        if self.version == 1 {
            return ManifestImages {
                version: 1,
                remaining: self.source,
                done: false,
            };
        }

        // v2では最初の`image=`行から走査を始める。
        let start = self.source.find("\nimage=").map_or(0, |index| index + 1);
        ManifestImages {
            version: 2,
            remaining: &self.source[start..],
            done: false,
        }
    }
}

fn validate_name(name: &str) -> Result<(), ManifestError> {
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
    Ok(())
}

fn validate_argument(argument: &str, count: &mut usize) -> Result<(), ManifestError> {
    *count += 1;
    if *count > ARG_MAX_COUNT {
        return Err(ManifestError::TooManyArgs);
    }
    if argument.len() > ARG_MAX_LEN {
        return Err(ManifestError::ArgumentTooLong);
    }
    if argument.as_bytes().contains(&0) {
        return Err(ManifestError::ArgumentContainsNul);
    }
    if argument.as_bytes().contains(&b'\r') {
        return Err(ManifestError::ArgumentContainsCarriageReturn);
    }
    Ok(())
}

// `elf=<offset>,<len>`を10進数でparseする。len=0やu64溢れは拒否する。
fn parse_elf_range(spec: &str) -> Result<(u64, u64), ManifestError> {
    let (offset, len) = spec.split_once(',').ok_or(ManifestError::MalformedElf)?;
    let parse_cell = |cell: &str| -> Result<u64, ManifestError> {
        if cell.is_empty() || !cell.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ManifestError::MalformedElf);
        }
        cell.parse::<u64>().map_err(|_| ManifestError::MalformedElf)
    };
    let offset = parse_cell(offset)?;
    let len = parse_cell(len)?;
    if len == 0 || offset.checked_add(len).is_none() {
        return Err(ManifestError::MalformedElf);
    }
    Ok((offset, len))
}

impl<'a> ManifestImage<'a> {
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// `header.elf`領域先頭からの相対range。`None`は領域全体 (v1) を意味する。
    pub fn elf(&self) -> Option<Range<u64>> {
        self.elf.clone()
    }

    pub fn args(&self) -> ManifestArgs<'a> {
        ManifestArgs {
            lines: self.args_source.split('\n'),
            stop_at_image: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManifestArgs<'a> {
    lines: core::str::Split<'a, char>,
    stop_at_image: bool,
}

impl<'a> Iterator for ManifestArgs<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        if self.stop_at_image && !line.starts_with("arg=") {
            return None;
        }
        line.strip_prefix("arg=")
    }
}

/// [`Manifest::images`]のiterator。v2では残りsourceを`image=`行で区切りながら
/// 各sectionを再parseする (eager検証済みなので失敗しない)。
#[derive(Debug, Clone)]
pub struct ManifestImages<'a> {
    version: u8,
    remaining: &'a str,
    done: bool,
}

impl<'a> Iterator for ManifestImages<'a> {
    type Item = ManifestImage<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.version == 1 {
            self.done = true;
            let mut lines = self.remaining.split('\n');
            let _ = lines.next();
            let name = lines
                .next()
                .and_then(|line| line.strip_prefix("name="))
                .expect("v1 manifest keeps a validated name line");
            // args_sourceはname行の直後 (version行とname行の2行分を落とす)。
            let args_source = self.remaining.splitn(3, '\n').nth(2).unwrap_or("");
            return Some(ManifestImage {
                name,
                args_source,
                elf: None,
            });
        }

        // sectionは次の`image=`行の手前の改行まで。remainingは次の`image=`行
        // 先頭へ進める (末尾に達したら空にして次回Noneを返す)。
        let section = match self.remaining.find("\nimage=") {
            Some(index) => {
                let section = &self.remaining[..index];
                self.remaining = &self.remaining[index + 1..];
                section
            }
            None => core::mem::take(&mut self.remaining),
        };
        if section.is_empty() {
            self.done = true;
            return None;
        }
        let (image_line, body) = section
            .split_once('\n')
            .expect("validated section keeps an image line and an elf line");
        let name = image_line
            .strip_prefix("image=")
            .expect("validated section starts with image=");
        let elf_spec = body
            .rsplit('\n')
            .next()
            .and_then(|line| line.strip_prefix("elf="))
            .expect("validated section ends with elf=");
        let (offset, len) = parse_elf_range(elf_spec).expect("validated elf range");
        Some(ManifestImage {
            name,
            args_source: body,
            elf: Some(offset..offset + len),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use self::std::{format, string::String, vec, vec::Vec};

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
    fn rejects_carriage_return_in_argument() {
        let bytes = b"version=1\nname=hello\narg=first\r\narg=second\n";

        assert_eq!(
            Manifest::parse(bytes),
            Err(ManifestError::ArgumentContainsCarriageReturn)
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
        let bytes = b"version=3\nname=hello\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingVersion));
    }

    #[test]
    fn v2_rejects_v1_style_name_line() {
        // `name=`はv1のkeyであり、v2ではimage sectionしか受理しない。
        let bytes = b"version=2\nname=hello\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::UnknownKey));
    }

    #[test]
    fn v2_parses_multiple_images_with_args_and_elf_ranges() {
        let bytes = b"version=2\n\
image=spin\narg=slow\narg=fast\nelf=0,4096\n\
image=cat\nelf=4096,2048\n";

        let manifest = Manifest::parse(bytes).unwrap();
        assert_eq!(manifest.version(), 2);
        assert_eq!(manifest.name(), "spin");

        let images: Vec<_> = manifest.images().collect();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].name(), "spin");
        assert_eq!(images[0].args().collect::<Vec<_>>(), vec!["slow", "fast"]);
        assert_eq!(images[0].elf(), Some(0..4096));
        assert_eq!(images[1].name(), "cat");
        assert_eq!(images[1].args().count(), 0);
        assert_eq!(images[1].elf(), Some(4096..6144));
    }

    #[test]
    fn v1_yields_a_single_image_spanning_the_whole_elf_area() {
        let bytes = b"version=1\nname=hello\narg=first\narg=second\n";

        let manifest = Manifest::parse(bytes).unwrap();
        let images: Vec<_> = manifest.images().collect();

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].name(), "hello");
        assert_eq!(
            images[0].args().collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(images[0].elf(), None);
    }

    #[test]
    fn v2_rejects_a_manifest_without_any_image() {
        let bytes = b"version=2\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingImage));
    }

    #[test]
    fn v2_rejects_an_image_without_an_elf_range() {
        for bytes in [
            b"version=2\nimage=a\n".as_slice(),
            b"version=2\nimage=a\nelf=0,4\nimage=b\n".as_slice(),
        ] {
            assert_eq!(Manifest::parse(bytes), Err(ManifestError::MissingElf));
        }
    }

    #[test]
    fn v2_rejects_a_fifth_image() {
        let bytes = b"version=2\n\
image=a\nelf=0,4\nimage=b\nelf=4,4\nimage=c\nelf=8,4\n\
image=d\nelf=12,4\nimage=e\nelf=16,4\n";

        assert_eq!(Manifest::parse(bytes), Err(ManifestError::TooManyImages));
    }

    #[test]
    fn v2_rejects_malformed_elf_ranges() {
        for spec in [
            "elf=",      // 欠落
            "elf=0",     // commaなし
            "elf=,4",    // offset欠落
            "elf=0,",    // len欠落
            "elf=a,4",   // 非数字
            "elf=0,4x",  // 非数字
            "elf=0,0",   // len=0
            "elf=0,1,2", // 余分なcell
        ] {
            let text = format!("version=2\nimage=a\n{spec}\n");
            assert_eq!(
                Manifest::parse(text.as_bytes()),
                Err(ManifestError::MalformedElf),
                "spec={spec}"
            );
        }
        let overflow = b"version=2\nimage=a\nelf=18446744073709551615,1\n";
        assert_eq!(Manifest::parse(overflow), Err(ManifestError::MalformedElf));
    }

    #[test]
    fn v2_rejects_overlapping_elf_ranges() {
        let bytes = b"version=2\nimage=a\nelf=0,4096\nimage=b\nelf=2048,4096\n";

        assert_eq!(
            Manifest::parse(bytes),
            Err(ManifestError::OverlappingElfRange)
        );

        // 隣接はoverlapではない。
        let bytes = b"version=2\nimage=a\nelf=0,4096\nimage=b\nelf=4096,4096\n";
        assert!(Manifest::parse(bytes).is_ok());
    }

    #[test]
    fn v2_rejects_keys_outside_image_sections() {
        // elf=はimageより前に置けない。
        assert_eq!(
            Manifest::parse(b"version=2\nelf=0,4\nimage=a\nelf=4,4\n"),
            Err(ManifestError::InvalidOrder)
        );
        // arg=もimageより前に置けない。
        assert_eq!(
            Manifest::parse(b"version=2\narg=x\nimage=a\nelf=0,4\n"),
            Err(ManifestError::InvalidOrder)
        );
        // elf=の後にarg=は置けない (section順序は arg* elf)。
        assert_eq!(
            Manifest::parse(b"version=2\nimage=a\nelf=0,4\narg=x\n"),
            Err(ManifestError::InvalidOrder)
        );
        // 2度目のelf=は拒否する。
        assert_eq!(
            Manifest::parse(b"version=2\nimage=a\nelf=0,4\nelf=4,4\n"),
            Err(ManifestError::InvalidOrder)
        );
    }

    #[test]
    fn v2_rejects_duplicate_version_and_unknown_keys() {
        assert_eq!(
            Manifest::parse(b"version=2\nimage=a\nelf=0,4\nversion=2\n"),
            Err(ManifestError::DuplicateVersion)
        );
        assert_eq!(
            Manifest::parse(b"version=2\nimage=a\nelf=0,4\nenv=K=V\n"),
            Err(ManifestError::UnknownKey)
        );
    }

    #[test]
    fn v2_applies_per_image_argument_limits() {
        let mut text = String::from("version=2\nimage=a\n");
        for _ in 0..17 {
            text.push_str("arg=x\n");
        }
        text.push_str("elf=0,4\n");

        assert_eq!(
            Manifest::parse(text.as_bytes()),
            Err(ManifestError::TooManyArgs)
        );
    }

    #[test]
    fn v2_rejects_an_empty_image_name() {
        assert_eq!(
            Manifest::parse(b"version=2\nimage=\nelf=0,4\n"),
            Err(ManifestError::EmptyName)
        );
        assert_eq!(
            Manifest::parse(b"version=2\nimage=a b\nelf=0,4\n"),
            Err(ManifestError::InvalidName)
        );
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
