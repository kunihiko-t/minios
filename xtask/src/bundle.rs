//! ビルド済みguest ELFとmanifestからMiniBundle (v1単一image / v2複数image) を生成する。
//!
//! `cargo xtask bundle`はguest binaryのbuildとbundle fileの書き出しを
//! 一つの開発commandにまとめ、配置とdigestはABI decoderと同じ規約で作る。

use std::{fmt, path::PathBuf};

use minios_abi::boot::{BOOT_HEADER_LEN, BUNDLE_MAX_LEN, BootHeader, ByteRange};
use minios_abi::manifest::{Manifest, ManifestError};

/// manifestの`name`の既定値。guest binary名とそろえる。
pub const DEFAULT_BUNDLE_NAME: &str = "minios-guest";
/// bundle出力の既定file名。workspaceの`target/`直下へ置く。
pub const DEFAULT_BUNDLE_FILE_NAME: &str = "minios-guest.mcb";

/// bundle生成に失敗した理由。
#[derive(Debug)]
pub enum BundleError {
    /// guest binaryのbuildに失敗した。
    GuestBuild(crate::guest::GuestError),
    /// build済みguest ELFの読み取りに失敗した。
    ReadElf { path: PathBuf, message: String },
    /// manifestがABI契約に反する。入力の特定に必要なnameとarg数を添える。
    Manifest {
        name: String,
        arguments: usize,
        source: ManifestError,
    },
    /// bundle全体が6 MiB上限を超えた。
    TooLarge { total_len: u64, max_len: u64 },
    /// bundle fileの書き出しに失敗した。
    Write { path: PathBuf, message: String },
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestBuild(error) => error.fmt(formatter),
            Self::ReadElf { path, message } => write!(
                formatter,
                "could not read guest ELF {}: {message}",
                path.display()
            ),
            Self::Manifest {
                name,
                arguments,
                source,
            } => write!(
                formatter,
                "invalid bundle manifest (name={name:?}, args={arguments}): {source:?}"
            ),
            Self::TooLarge { total_len, max_len } => write!(
                formatter,
                "bundle of {total_len} bytes exceeds the {max_len} byte limit"
            ),
            Self::Write { path, message } => write!(
                formatter,
                "could not write bundle {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for BundleError {}

/// `cargo xtask bundle`の入力。`None`の項目は既定値を使う。
/// `images`が空でなければmanifest v2の複数image bundleを生成する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleRequest {
    pub name: Option<String>,
    pub args: Vec<String>,
    /// 複数image modeのguest bin名。`--image`で順に指定する。
    pub images: Vec<String>,
    pub output: Option<PathBuf>,
}

/// 複数image bundleに載せる1 imageの入力。`elf` rangeは連結順の累積offsetから
/// 生成側が計算するため、ここではbytesだけを渡す。
#[derive(Debug)]
pub struct BundleImage<'a> {
    pub name: &'a str,
    pub args: &'a [String],
    pub elf: &'a [u8],
}

/// 書き出したbundleの要約。digestはcontent addressingに使える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleProduct {
    pub path: PathBuf,
    pub total_len: u64,
    pub name: String,
    pub arguments: usize,
    /// bundle内のimage数。単一image (v1) では1。
    pub images: usize,
    pub digest: [u8; 32],
}

/// layout済みbundle bytesとheaderへ格納したdigest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltBundle {
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl BuiltBundle {
    /// bundle全体のbytesを借りる。
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// headerの`digest` fieldと同じSHA-256値を返す。
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// guestのbuildからbundle fileの書き出しまでを一括で実行する。
pub fn create_bundle_file(request: &BundleRequest) -> Result<BundleProduct, BundleError> {
    if request.images.is_empty() {
        let elf_path = crate::guest::build_guest().map_err(BundleError::GuestBuild)?;
        let elf = std::fs::read(&elf_path).map_err(|error| BundleError::ReadElf {
            path: elf_path,
            message: error.to_string(),
        })?;
        return assemble_bundle_file(request, &elf);
    }

    // 複数image mode: `--image`で指定されたguest binを順にbuildし、
    // manifest v2のbundleへ組み立てる。
    let mut elfs = Vec::with_capacity(request.images.len());
    for name in &request.images {
        let path = crate::guest::build_guest_bin(name).map_err(BundleError::GuestBuild)?;
        let elf = std::fs::read(&path).map_err(|error| BundleError::ReadElf {
            path,
            message: error.to_string(),
        })?;
        elfs.push(elf);
    }
    let images: Vec<BundleImage<'_>> = request
        .images
        .iter()
        .zip(&elfs)
        .map(|(name, elf)| BundleImage {
            name,
            args: &[],
            elf,
        })
        .collect();
    assemble_multi_bundle_file(request, &images)
}

/// 取得済みELFからmanifest生成、bundle組み立て、file書き出しを行う。
/// unit testは共有の`guest_bytes`を渡し、test process内のcargo起動を
/// 一度だけに絞る。並行testが別々にcargoを起動すると成果物の再linkと
/// 読み取りが競合し、存在したはずのELFが読めなくなる。
fn assemble_bundle_file(request: &BundleRequest, elf: &[u8]) -> Result<BundleProduct, BundleError> {
    let name = request
        .name
        .clone()
        .unwrap_or_else(|| DEFAULT_BUNDLE_NAME.to_owned());
    let manifest = render_manifest(&name, &request.args)?;
    let bundle = build_bundle(&manifest, elf)?;
    let output = request.output.clone().unwrap_or_else(default_bundle_path);
    std::fs::write(&output, bundle.bytes()).map_err(|error| BundleError::Write {
        path: output.clone(),
        message: error.to_string(),
    })?;
    Ok(BundleProduct {
        path: output,
        total_len: bundle.bytes().len() as u64,
        name,
        arguments: request.args.len(),
        images: 1,
        digest: bundle.digest(),
    })
}

/// 複数image (manifest v2) のbundleを組み立てて書き出す。
///
/// 各imageのELFは`header.elf`領域へ指定順に隙間なく連結し、
/// `elf=<offset>,<len>`行がその累積offsetを記録する。
pub fn assemble_multi_bundle_file(
    request: &BundleRequest,
    images: &[BundleImage<'_>],
) -> Result<BundleProduct, BundleError> {
    let manifest = render_manifest_multi(images)?;
    let mut elf_area = Vec::new();
    for image in images {
        elf_area.extend_from_slice(image.elf);
    }
    let bundle = build_bundle(&manifest, &elf_area)?;
    let output = request.output.clone().unwrap_or_else(default_bundle_path);
    std::fs::write(&output, bundle.bytes()).map_err(|error| BundleError::Write {
        path: output.clone(),
        message: error.to_string(),
    })?;
    Ok(BundleProduct {
        path: output,
        total_len: bundle.bytes().len() as u64,
        name: images
            .first()
            .map_or(String::new(), |image| image.name.to_owned()),
        arguments: images.iter().map(|image| image.args.len()).sum(),
        images: images.len(),
        digest: bundle.digest(),
    })
}

/// manifest v2 (`version=2` + `image=` section列) を生成し、ABI parserで検査する。
///
/// `elf=<offset>,<len>`は各imageの連結順累積offsetから求める。
pub fn render_manifest_multi(images: &[BundleImage<'_>]) -> Result<Vec<u8>, BundleError> {
    let mut text = String::from("version=2\n");
    let mut offset = 0u64;
    for image in images {
        text.push_str("image=");
        text.push_str(image.name);
        text.push('\n');
        for argument in image.args {
            text.push_str("arg=");
            text.push_str(argument);
            text.push('\n');
        }
        text.push_str(&format!("elf={},{}\n", offset, image.elf.len()));
        offset += image.elf.len() as u64;
    }
    let bytes = text.into_bytes();
    if let Err(source) = Manifest::parse(&bytes) {
        return Err(BundleError::Manifest {
            name: images
                .first()
                .map_or(String::new(), |image| image.name.to_owned()),
            arguments: images.iter().map(|image| image.args.len()).sum(),
            source,
        });
    }
    Ok(bytes)
}

/// `name`と`arg=`行からmanifest bytesを作り、ABI parserで全制限を検査する。
///
/// 文字種、件数、長さ、全体4 KiBの判定は`Manifest::parse`に一任するため、
/// 生成側とkernel側の受理条件がずれない。
pub fn render_manifest(name: &str, args: &[String]) -> Result<Vec<u8>, BundleError> {
    let mut text = String::from("version=1\nname=");
    text.push_str(name);
    text.push('\n');
    for argument in args {
        text.push_str("arg=");
        text.push_str(argument);
        text.push('\n');
    }
    let bytes = text.into_bytes();
    if let Err(source) = Manifest::parse(&bytes) {
        return Err(BundleError::Manifest {
            name: name.to_owned(),
            arguments: args.len(),
            source,
        });
    }
    Ok(bytes)
}

/// manifestとELFをMiniBundle v1の正規配置へ組み立てる。
///
/// header 96 byte、manifest、8 byte境界までのゼロpadding、ELFの順に並べ、
/// `digest` fieldだけをゼロにしたheaderと残り全部のSHA-256を格納する。
/// 入力だけから決まる固定手順のため、同じ入力は同じbyte列になる。
/// manifestの内容検査は[`render_manifest`]の責務であり、ここでは配置だけ扱う。
pub fn build_bundle(manifest: &[u8], elf: &[u8]) -> Result<BuiltBundle, BundleError> {
    let manifest_end = BOOT_HEADER_LEN + manifest.len();
    let padding_len = (8 - manifest_end % 8) % 8;
    let elf_offset = manifest_end + padding_len;
    // 飽和しても上限超過として扱うため、桁あふれを見逃さない。
    let total_len = elf_offset.saturating_add(elf.len());
    if total_len as u64 > BUNDLE_MAX_LEN {
        return Err(BundleError::TooLarge {
            total_len: total_len as u64,
            max_len: BUNDLE_MAX_LEN,
        });
    }

    let header = BootHeader {
        total_len: total_len as u64,
        manifest: ByteRange {
            offset: BOOT_HEADER_LEN as u64,
            len: manifest.len() as u64,
        },
        elf: ByteRange {
            offset: elf_offset as u64,
            len: elf.len() as u64,
        },
        digest: [0; 32],
    };
    let mut bytes = vec![0u8; total_len];
    bytes[BOOT_HEADER_LEN..manifest_end].copy_from_slice(manifest);
    bytes[elf_offset..].copy_from_slice(elf);
    // digest入力はdigest fieldをゼロにしたheaderとoffset 96以降の全部である。
    let mut digest_input = Vec::with_capacity(total_len);
    digest_input.extend_from_slice(&header.encode_with_zero_digest());
    digest_input.extend_from_slice(&bytes[BOOT_HEADER_LEN..]);
    let digest = sha256(&digest_input);
    let header = BootHeader { digest, ..header };
    bytes[..BOOT_HEADER_LEN].copy_from_slice(&header.encode());
    Ok(BuiltBundle { bytes, digest })
}

fn default_bundle_path() -> PathBuf {
    crate::workspace_root()
        .join("target")
        .join(DEFAULT_BUNDLE_FILE_NAME)
}

/// SHA-256 (FIPS 180-4)。外部crateを追加せずにbundle digestを計算するための
/// 最小実装である。
mod sha256_impl {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub fn digest(message: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (message.len() as u64).wrapping_mul(8);
        let mut padded = message.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_be_bytes());

        for block in padded.chunks(64) {
            let mut w = [0u32; 64];
            for (index, word) in block.chunks(4).enumerate() {
                w[index] = u32::from_be_bytes(word.try_into().expect("4-byte chunk"));
            }
            for index in 16..64 {
                let s0 = w[index - 15].rotate_right(7)
                    ^ w[index - 15].rotate_right(18)
                    ^ (w[index - 15] >> 3);
                let s1 = w[index - 2].rotate_right(17)
                    ^ w[index - 2].rotate_right(19)
                    ^ (w[index - 2] >> 10);
                w[index] = w[index - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[index - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for index in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[index])
                    .wrapping_add(w[index]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut digest = [0u8; 32];
        for (index, word) in h.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

pub(crate) fn sha256(message: &[u8]) -> [u8; 32] {
    sha256_impl::digest(message)
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec, vec::Vec};

    use super::{
        BOOT_HEADER_LEN, BUNDLE_MAX_LEN, BundleError, BundleProduct, BundleRequest, build_bundle,
        render_manifest, sha256,
    };
    use minios_abi::{
        boot::BootHeader,
        manifest::{Manifest, ManifestError},
    };

    const MANIFEST: &[u8] = b"version=1\nname=hello\narg=alpha\narg=bravo\n";

    fn fixture_elf() -> Vec<u8> {
        let mut elf = vec![0u8; 256];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf
    }

    // Catches a broken SHA-256 round (message schedule or padding drift)
    // silently producing a digest that real importers would reject.
    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(
            sha256(b""),
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ]
        );
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    // Catches a bundle whose header, manifest, padding, ELF placement, or
    // digest drifts from the canonical layout the kernel parser validates.
    #[test]
    fn bundle_layout_matches_minibundle_v1() {
        let elf = fixture_elf();
        let bundle = build_bundle(MANIFEST, &elf).expect("fixture must fit the bundle limits");
        let bytes = bundle.bytes();

        let header = BootHeader::decode(&bytes[..BOOT_HEADER_LEN])
            .expect("ABI decoder must accept the header");
        assert_eq!(header.total_len, bytes.len() as u64);
        assert_eq!(header.manifest.offset, BOOT_HEADER_LEN as u64);
        assert_eq!(header.manifest.len, MANIFEST.len() as u64);
        let manifest_end = BOOT_HEADER_LEN + MANIFEST.len();
        let elf_offset = manifest_end + (8 - manifest_end % 8) % 8;
        assert_eq!(header.elf.offset, elf_offset as u64);
        assert_eq!(header.elf.len, elf.len() as u64);
        assert_eq!(&bytes[BOOT_HEADER_LEN..manifest_end], MANIFEST);
        assert!(
            bytes[manifest_end..elf_offset]
                .iter()
                .all(|byte| *byte == 0),
            "padding must be zero"
        );
        assert_eq!(&bytes[elf_offset..], elf.as_slice());

        let manifest = Manifest::parse(&bytes[BOOT_HEADER_LEN..manifest_end])
            .expect("ABI decoder must accept the manifest");
        assert_eq!(manifest.name(), "hello");
        assert_eq!(manifest.args().collect::<Vec<_>>(), vec!["alpha", "bravo"]);

        // digestを自力で再計算し、headerのdigest fieldと一致することを確認する。
        let mut digest_input = Vec::new();
        let mut zeroed = bytes[..BOOT_HEADER_LEN].to_vec();
        zeroed[56..88].fill(0);
        digest_input.extend_from_slice(&zeroed);
        digest_input.extend_from_slice(&bytes[BOOT_HEADER_LEN..]);
        assert_eq!(&header.digest, &sha256(&digest_input));
        assert_eq!(&bundle.digest(), &header.digest);
    }

    // Catches timestamps, random padding, or unordered writes that would make
    // the same guest produce different bundle bytes.
    #[test]
    fn same_input_produces_identical_bytes() {
        let elf = fixture_elf();
        let first = build_bundle(MANIFEST, &elf).expect("fixture must fit the bundle limits");
        let second = build_bundle(MANIFEST, &elf).expect("fixture must fit the bundle limits");

        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.digest(), second.digest());
    }

    // Catches silently dropping a manifest limit or reporting it without the
    // offending input and the ABI rejection reason.
    #[test]
    fn manifest_limits_are_reported_without_loss() {
        let cases: Vec<(String, Vec<String>, ManifestError)> = vec![
            (String::new(), vec![], ManifestError::EmptyName),
            ("hello world".to_owned(), vec![], ManifestError::InvalidName),
            ("a".repeat(129), vec![], ManifestError::NameTooLong),
            (
                "hello".to_owned(),
                vec!["x".to_owned(); 17],
                ManifestError::TooManyArgs,
            ),
            (
                "hello".to_owned(),
                vec!["a".repeat(257)],
                ManifestError::ArgumentTooLong,
            ),
            (
                "hello".to_owned(),
                vec!["before\0after".to_owned()],
                ManifestError::ArgumentContainsNul,
            ),
            (
                "hello".to_owned(),
                vec!["before\rafter".to_owned()],
                ManifestError::ArgumentContainsCarriageReturn,
            ),
            (
                "a".to_owned(),
                vec!["a".repeat(256); 16],
                ManifestError::TooLong,
            ),
        ];

        for (name, args, expected) in &cases {
            match render_manifest(name, args) {
                Err(BundleError::Manifest {
                    name: reported,
                    arguments,
                    source,
                }) => {
                    assert_eq!(&source, expected, "name={name:?}");
                    assert_eq!(&reported, name);
                    assert_eq!(arguments, args.len());
                }
                other => panic!("expected a manifest error for {name:?}, got {other:?}"),
            }
        }

        let diagnostic = render_manifest("hello", &vec!["x".to_owned(); 17])
            .expect_err("17 args must fail")
            .to_string();
        assert!(diagnostic.contains("TooManyArgs"), "{diagnostic}");
        assert!(diagnostic.contains("hello"), "{diagnostic}");
    }

    // Catches a bundle that passes the reserved window boundary, or wrapping
    // the length instead of rejecting it with both sizes.
    #[test]
    fn oversized_bundle_reports_total_and_limit() {
        let elf = vec![0u8; BUNDLE_MAX_LEN as usize];
        let manifest_end = BOOT_HEADER_LEN + MANIFEST.len();
        let elf_offset = manifest_end + (8 - manifest_end % 8) % 8;

        match build_bundle(MANIFEST, &elf) {
            Err(BundleError::TooLarge { total_len, max_len }) => {
                assert_eq!(total_len, elf_offset as u64 + BUNDLE_MAX_LEN);
                assert_eq!(max_len, BUNDLE_MAX_LEN);
            }
            other => panic!("expected a too-large error, got {other:?}"),
        }
    }

    // Catches a builder output that the kernel two-stage parser rejects, or a
    // manifest whose name and args do not survive the round trip.
    #[test]
    fn real_guest_bundle_passes_abi_and_kernel_decoders() {
        use minios_kernel::boot_payload::BootPayload;

        let manifest = render_manifest("minios-guest", &["alpha".to_owned(), "bravo".to_owned()])
            .expect("valid manifest must render");
        let bundle = build_bundle(&manifest, crate::guest::guest_bytes())
            .expect("real guest must fit the bundle limits");

        let header = BootHeader::decode(&bundle.bytes()[..BOOT_HEADER_LEN])
            .expect("ABI decoder must accept the header");
        assert_eq!(header.total_len, bundle.bytes().len() as u64);

        let payload =
            BootPayload::parse(bundle.bytes()).expect("kernel parser must accept the bundle");
        assert_eq!(payload.manifest().name(), "minios-guest");
        assert_eq!(
            payload.manifest().args().collect::<Vec<_>>(),
            vec!["alpha", "bravo"]
        );
        assert_eq!(payload.elf(), crate::guest::guest_bytes());
        assert_eq!(payload.total_len(), bundle.bytes().len() as u64);
    }

    // Catches a multi-image manifest whose layout (cumulative elf= offsets)
    // or image metadata the kernel parser would reject, and vice versa: the
    // bundle the kernel accepts must carry the per-image ranges it declares.
    #[test]
    fn multi_image_bundle_passes_the_kernel_decoder() {
        use minios_kernel::boot_payload::BootPayload;

        let first = crate::guest::guest_bytes();
        let second = crate::guest::guest_bytes();
        let images = [
            super::BundleImage {
                name: "first",
                args: &[String::from("alpha")],
                elf: first,
            },
            super::BundleImage {
                name: "second",
                args: &[],
                elf: second,
            },
        ];
        let manifest =
            super::render_manifest_multi(&images).expect("multi-image manifest must render");

        let parsed = Manifest::parse(&manifest).expect("manifest must parse");
        assert_eq!(parsed.version(), 2);
        let declared: Vec<_> = parsed.images().collect();
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].name(), "first");
        assert_eq!(declared[0].args().collect::<Vec<_>>(), vec!["alpha"]);
        assert_eq!(declared[0].elf(), Some(0..first.len() as u64));
        assert_eq!(declared[1].name(), "second");
        assert_eq!(
            declared[1].elf(),
            Some(first.len() as u64..(first.len() + second.len()) as u64)
        );

        let mut area = Vec::new();
        area.extend_from_slice(first);
        area.extend_from_slice(second);
        let bundle = build_bundle(&manifest, &area).expect("bundle must fit");
        let payload = BootPayload::parse(bundle.bytes()).expect("kernel must accept v2 bundle");
        let payload_images: Vec<_> = payload.images().collect();
        assert_eq!(payload_images.len(), 2);
        assert_eq!(payload.image_elf(&payload_images[0]), first);
        assert_eq!(payload.image_elf(&payload_images[1]), second);
    }

    // Catches a `bundle` command path that ignores the requested manifest,
    // misapplies the defaults, or writes bytes that fail the ABI decoder.
    // The guest ELF comes from the shared test-process build; the QEMU
    // payload-args run proves the cargo-spawning production path end to end.
    #[test]
    fn assemble_bundle_file_writes_a_decodable_bundle_with_defaults() {
        let output = std::env::temp_dir().join(format!(
            "minios-bundle-command-{}-{}.mcb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        let product = super::assemble_bundle_file(
            &BundleRequest {
                name: None,
                args: vec!["alpha".to_owned(), "bravo".to_owned()],
                images: vec![],
                output: Some(output.clone()),
            },
            crate::guest::guest_bytes(),
        )
        .expect("bundle command path must succeed");

        let bytes = std::fs::read(&output).expect("bundle file must be readable");
        let _ = std::fs::remove_file(&output);

        let header =
            BootHeader::decode(&bytes[..BOOT_HEADER_LEN]).expect("written bundle must decode");
        let manifest_end = BOOT_HEADER_LEN + header.manifest.len as usize;
        let expected: BundleProduct = BundleProduct {
            path: output,
            total_len: bytes.len() as u64,
            name: super::DEFAULT_BUNDLE_NAME.to_owned(),
            arguments: 2,
            images: 1,
            digest: header.digest,
        };
        assert_eq!(product, expected);
        let manifest = Manifest::parse(&bytes[BOOT_HEADER_LEN..manifest_end])
            .expect("written manifest must decode");
        assert_eq!(manifest.name(), "minios-guest");
        assert_eq!(manifest.args().collect::<Vec<_>>(), vec!["alpha", "bravo"]);
    }
}
