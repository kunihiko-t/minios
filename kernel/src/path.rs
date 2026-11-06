//! processの作業directory（cwd）とpathの連結。
//!
//! FAT32 layerのpathはroot directoryからの`/`区切りの相対path（`BIN/CAT.ELF`、
//! rootそのものは空文字列）である。guestが渡すpathは、`/`で始まればroot
//! からの絶対path、そうでなければcwdからの相対pathとして`resolve`がFAT32
//! layerの形へ直す。`.`は今のdirectory、`..`は親directoryで、rootの親は
//! root自身である。cwdも同じFAT32 layerの形で持つため、既定のcwd（空文字列）
//! では以前のroot相対のpathがそのまま同じ意味になる。

use alloc::string::String;
use minios_abi::syscall::{EINVAL, MAX_PATH_LEN};

/// `path`を`cwd`基準で解決し、FAT32 layerのpathを返す。結果が
/// `MAX_PATH_LEN`を超えれば`EINVAL`。空の`path`はcwd自身を指す。
/// 要素の文字種や存在はここでは検査せず、FAT32 layerへ任せる。
pub fn resolve(cwd: &str, path: &str) -> Result<String, isize> {
    let mut parts: alloc::vec::Vec<&str> = if path.starts_with('/') {
        alloc::vec::Vec::new()
    } else {
        cwd.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    let resolved = parts.join("/");
    if resolved.len() > MAX_PATH_LEN {
        return Err(EINVAL);
    }
    Ok(resolved)
}

/// FAT32 layerのpath（cwd）を、guestへ見せる`/`始まりの絶対pathにする。
pub fn display(cwd: &str) -> String {
    let mut shown = String::from("/");
    shown.push_str(cwd);
    shown
}

#[cfg(test)]
mod tests {
    use super::{display, resolve};
    use minios_abi::syscall::{EINVAL, MAX_PATH_LEN};

    // Catches relative paths ignoring the cwd or absolute paths keeping it,
    // and dot segments leaking through to the FAT32 layer.
    #[test]
    fn resolve_joins_relative_paths_and_normalizes_dots() {
        assert_eq!(resolve("", "BIN/CAT.ELF").as_deref(), Ok("BIN/CAT.ELF"));
        assert_eq!(resolve("work", "a.txt").as_deref(), Ok("work/a.txt"));
        assert_eq!(
            resolve("work/sub", "/BIN/LS.ELF").as_deref(),
            Ok("BIN/LS.ELF")
        );
        assert_eq!(resolve("work/sub", "../x").as_deref(), Ok("work/x"));
        assert_eq!(resolve("work", "./a//b/").as_deref(), Ok("work/a/b"));
        assert_eq!(resolve("work", "..").as_deref(), Ok(""));
        assert_eq!(resolve("", "../../x").as_deref(), Ok("x"));
        assert_eq!(resolve("work", "").as_deref(), Ok("work"));
        assert_eq!(resolve("work", "/").as_deref(), Ok(""));
    }

    // Catches a joined path exceeding what the FAT32 layer accepts.
    #[test]
    fn resolve_rejects_results_longer_than_the_path_limit() {
        let long = "a".repeat(MAX_PATH_LEN - 1);
        assert!(resolve("", &long).is_ok());
        assert_eq!(resolve("dir", &long), Err(EINVAL));
    }

    #[test]
    fn display_prefixes_the_root_slash() {
        assert_eq!(display(""), "/");
        assert_eq!(display("work/sub"), "/work/sub");
    }
}
