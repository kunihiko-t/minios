//! FAT32・ELF・boot payload parserの決定性fuzz harness。
//!
//! 標準libraryのみで動作し、seedとcorpusが同じなら同じ入力列を生成する。
//! 検出対象はpanic、hang、過大allocationの三つである。parserが返す通常の
//! errorはfindingにしない。

use std::alloc::{GlobalAlloc, Layout, System};
use std::any::Any;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use minios_kernel::boot_payload::BootPayload;
use minios_kernel::elf::{ElfImage, LoadPlan};
use minios_kernel::storage::SectorReader;
use minios_kernel::storage::fat32::Fat32;

/// Fuzz対象のparser。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzTarget {
    /// `Fat32::mount`＋root走査＋root file読み出し。
    Fat32,
    /// `ElfImage::parse`＋program header走査＋`LoadPlan`。
    Elf,
    /// `BootPayload::parse`＋image走査。
    BootPayload,
}

impl FuzzTarget {
    /// CLI名から対象を解決する。
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "fat32" => Some(Self::Fat32),
            "elf" => Some(Self::Elf),
            "boot-payload" => Some(Self::BootPayload),
            _ => None,
        }
    }

    /// CLI名とcorpus directory名を返す。
    pub fn name(self) -> &'static str {
        match self {
            Self::Fat32 => "fat32",
            Self::Elf => "elf",
            Self::BootPayload => "boot-payload",
        }
    }
}

/// 既定の乱数seed。
pub const DEFAULT_SEED: u64 = 27;
/// 既定の入力数。
pub const DEFAULT_ITERS: u64 = 10_000;
/// 既定の入力上限(bytes)。FAT32 imageは複数sectorを要するため大きめに取る。
pub const DEFAULT_MAX_BYTES: usize = 128 * 1024;
/// 既定の入力あたりtimeout。
pub const DEFAULT_INPUT_TIMEOUT: Duration = Duration::from_secs(5);
/// 既定のallocation上限(bytes)。
pub const DEFAULT_ALLOC_CAP: usize = 16 * 1024 * 1024;

/// Fuzz実行の条件。
#[derive(Debug, Clone)]
pub struct FuzzConfig {
    /// 対象parser。
    pub target: FuzzTarget,
    /// 乱数seed。同じseedは同じ入力列を生成する。
    pub seed: u64,
    /// 生成する入力数。replay時は無視する。
    pub iters: u64,
    /// 生成入力の上限(bytes)。corpusのseedはこの長さへ切り詰める。
    pub max_bytes: usize,
    /// 入力あたりのtimeout。
    pub input_timeout: Duration,
    /// 全体の制限時間。`None`は無制限。
    pub time_limit: Option<Duration>,
    /// 入力あたりのallocation上限(bytes)。
    pub alloc_cap: usize,
    /// seed corpusのdirectory。`<corpus>/<target>/` を読む。
    pub corpus_dir: PathBuf,
    /// findingの入力を保存するdirectory。
    pub output_dir: PathBuf,
    /// 指定時はこの一つのfileをそのまま再生する。
    pub replay_input: Option<PathBuf>,
}

/// 一つの入力の実行結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// parserが復帰した。errorも正常終了に含める。
    Pass {
        /// 実行中に確保したpeak(bytes)。
        peak_alloc: usize,
    },
    /// parserがpanicした。
    Panic {
        /// panicのmessage。
        message: String,
    },
    /// timeoutを超過した。
    Timeout,
    /// allocation上限を超過した。
    OverAlloc {
        /// 観測したpeak(bytes)。
        peak: usize,
        /// 適用した上限(bytes)。
        cap: usize,
    },
}

/// findingのないfuzz実行の報告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzReport {
    /// 対象parser。
    pub target: FuzzTarget,
    /// 使用した乱数seed。
    pub seed: u64,
    /// 実行した入力数。
    pub iters_run: u64,
}

/// Fuzz実行の失敗。
#[derive(Debug, PartialEq, Eq)]
pub enum FuzzError {
    /// 設定値が不正である。
    InvalidConfig(&'static str),
    /// fileの読み書きに失敗した。
    Io {
        /// 操作の内容。
        context: &'static str,
        /// 対象のpath。
        path: PathBuf,
        /// OSのmessage。
        message: String,
    },
    /// corpus directoryが存在しない。
    MissingCorpus {
        /// 期待したpath。
        path: PathBuf,
    },
    /// failing入力を検出した。詳細と再現commandを含む。
    FindingFound {
        /// 対象parser名。
        target: &'static str,
        /// 検出時の詳細。
        detail: String,
        /// 再現用の入力file。生成入力は保存先、replayは指定file。
        repro_path: PathBuf,
    },
}

impl fmt::Display for FuzzError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => {
                write!(formatter, "invalid fuzz configuration: {reason}")
            }
            Self::Io {
                context,
                path,
                message,
            } => write!(formatter, "{} {}: {message}", context, path.display()),
            Self::MissingCorpus { path } => write!(
                formatter,
                "fuzz corpus directory not found: {}",
                path.display()
            ),
            Self::FindingFound {
                target,
                detail,
                repro_path,
            } => write!(
                formatter,
                "fuzz {target} found a failing input ({detail}); reproduce with: cargo xtask fuzz --target {target} --input '{}'",
                repro_path.display()
            ),
        }
    }
}

impl std::error::Error for FuzzError {}

/// Fuzzを実行する。findingがあれば保存して`FindingFound`を返す。
pub fn run(config: &FuzzConfig) -> Result<FuzzReport, FuzzError> {
    validate(config)?;
    if let Some(input) = &config.replay_input {
        return replay(config, input);
    }
    let seeds = load_corpus(&config.corpus_dir, config.target, config.max_bytes)?;
    fs::create_dir_all(&config.output_dir)
        .map_err(|error| io_error("could not create", &config.output_dir, &error))?;
    let mut inputs = InputStream::new(seeds, config.seed, config.max_bytes);
    let started = Instant::now();
    let mut iters_run = 0;
    while iters_run < config.iters {
        if config
            .time_limit
            .is_some_and(|limit| started.elapsed() >= limit)
        {
            break;
        }
        let Some(input) = inputs.next() else {
            break;
        };
        match run_input(
            config.target,
            &input,
            config.input_timeout,
            config.alloc_cap,
        ) {
            InputOutcome::Pass { .. } => {}
            outcome => {
                let artifact = config.output_dir.join(format!(
                    "{}-{:016x}-{iters_run}.bin",
                    config.target.name(),
                    config.seed
                ));
                fs::write(&artifact, &input)
                    .map_err(|error| io_error("could not write", &artifact, &error))?;
                return Err(FuzzError::FindingFound {
                    target: config.target.name(),
                    detail: describe(&outcome, config),
                    repro_path: artifact,
                });
            }
        }
        iters_run += 1;
    }
    Ok(FuzzReport {
        target: config.target,
        seed: config.seed,
        iters_run,
    })
}

/// 一つのfileを変異なしで再生する。replayは入力を切り詰めない。
fn replay(config: &FuzzConfig, path: &Path) -> Result<FuzzReport, FuzzError> {
    let bytes = fs::read(path).map_err(|error| io_error("could not read", path, &error))?;
    match run_input(
        config.target,
        &bytes,
        config.input_timeout,
        config.alloc_cap,
    ) {
        InputOutcome::Pass { .. } => Ok(FuzzReport {
            target: config.target,
            seed: config.seed,
            iters_run: 1,
        }),
        outcome => Err(FuzzError::FindingFound {
            target: config.target.name(),
            detail: describe(&outcome, config),
            repro_path: path.to_owned(),
        }),
    }
}

fn validate(config: &FuzzConfig) -> Result<(), FuzzError> {
    if config.replay_input.is_none() && config.iters == 0 {
        return Err(FuzzError::InvalidConfig("iters must be at least 1"));
    }
    if config.max_bytes == 0 {
        return Err(FuzzError::InvalidConfig("max-bytes must be at least 1"));
    }
    if config.input_timeout.is_zero() {
        return Err(FuzzError::InvalidConfig("input-timeout must be positive"));
    }
    if config.alloc_cap == 0 {
        return Err(FuzzError::InvalidConfig("alloc-cap must be at least 1"));
    }
    Ok(())
}

fn describe(outcome: &InputOutcome, config: &FuzzConfig) -> String {
    match outcome {
        InputOutcome::Pass { .. } => "passed".to_owned(),
        InputOutcome::Panic { message } => format!("panic: {message}"),
        InputOutcome::Timeout => format!("timeout after {:.1?}", config.input_timeout),
        InputOutcome::OverAlloc { peak, cap } => {
            format!("peak allocation {peak} exceeds cap {cap}")
        }
    }
}

fn io_error(context: &'static str, path: &Path, error: &std::io::Error) -> FuzzError {
    FuzzError::Io {
        context,
        path: path.to_owned(),
        message: error.to_string(),
    }
}

/// 組み込みseedにcorpus directoryのfileを足した一覧を返す。
/// file名順に読むため、corpusが同じなら順序も同じである。
fn load_corpus(
    corpus_dir: &Path,
    target: FuzzTarget,
    max_bytes: usize,
) -> Result<Vec<Vec<u8>>, FuzzError> {
    let mut seeds = builtin_seeds(target);
    let dir = corpus_dir.join(target.name());
    if !dir.is_dir() {
        return Err(FuzzError::MissingCorpus { path: dir });
    }
    let mut entries: Vec<PathBuf> = Vec::new();
    let listing = fs::read_dir(&dir).map_err(|error| io_error("could not list", &dir, &error))?;
    for entry in listing {
        let path = entry
            .map_err(|error| io_error("could not list", &dir, &error))?
            .path();
        if path.is_file() {
            entries.push(path);
        }
    }
    entries.sort();
    for path in entries {
        let mut bytes =
            fs::read(&path).map_err(|error| io_error("could not read", &path, &error))?;
        bytes.truncate(max_bytes);
        seeds.push(bytes);
    }
    Ok(seeds)
}

/// 一つの入力をguard付きで実行する。
///
/// peakはparser実行中の確保だけを測り、入力buffer自体は含めない。
/// 計測はprocess全体を見るため、呼び出し側は計測中にほかのthreadが
/// 大量確保しないことを保証する。CLIは単threadで動き、corpusのreplay
/// testは直列化する。
pub fn run_input(
    target: FuzzTarget,
    bytes: &[u8],
    timeout: Duration,
    alloc_cap: usize,
) -> InputOutcome {
    let owned = bytes.to_vec();
    let outcome = run_guarded(
        move || match target {
            FuzzTarget::Fat32 => execute_fat32(&owned),
            FuzzTarget::Elf => execute_elf(&owned),
            FuzzTarget::BootPayload => execute_boot_payload(&owned),
        },
        timeout,
    );
    match outcome {
        GuardOutcome::Done { peak } if peak > alloc_cap => InputOutcome::OverAlloc {
            peak,
            cap: alloc_cap,
        },
        GuardOutcome::Done { peak } => InputOutcome::Pass { peak_alloc: peak },
        GuardOutcome::Panic { message } => InputOutcome::Panic { message },
        GuardOutcome::Timeout => InputOutcome::Timeout,
    }
}

/// fuzz入力をそのままvolume imageとして読ませるsector backend。
/// 512 byteに満たない末尾や存在しないlbaはMissingSectorとして報告する。
struct SliceReader<'a> {
    bytes: &'a [u8],
}

impl SectorReader for SliceReader<'_> {
    type Error = ();

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error> {
        let start = (lba as usize).checked_mul(512).ok_or(())?;
        let end = start.checked_add(512).ok_or(())?;
        destination.copy_from_slice(self.bytes.get(start..end).ok_or(())?);
        Ok(())
    }
}

/// mount、root走査、root fileの読み出しまでを一連で試す。mountが通った
/// imageだけがFAT走査とdir entryのparserを踏むため、単にparse失敗を
/// 重ねるより深い経路を検査できる。
fn execute_fat32(bytes: &[u8]) {
    let mut fs = match Fat32::mount(SliceReader { bytes }) {
        Ok(fs) => fs,
        Err(_) => return,
    };
    let mut names = std::vec::Vec::new();
    let _ = fs.for_each_root_entry(|entry| {
        if names.len() < 16 {
            names.push(entry.name().to_owned());
        }
    });
    for name in names {
        let _ = fs.read_root_file(&name, |_| {});
    }
}

fn execute_elf(bytes: &[u8]) {
    let Ok(image) = ElfImage::parse(bytes) else {
        return;
    };
    for header in image.program_headers() {
        let _ = header;
    }
    let _ = LoadPlan::new(&image);
}

fn execute_boot_payload(bytes: &[u8]) {
    let Ok(payload) = BootPayload::parse(bytes) else {
        return;
    };
    for image in payload.images() {
        let _ = payload.image_elf(&image);
    }
}

/// 対象の最小seed。corpus fileがなくてもharnessは動作する。
pub fn builtin_seeds(target: FuzzTarget) -> Vec<Vec<u8>> {
    match target {
        FuzzTarget::Fat32 => vec![fat32_image_seed()],
        FuzzTarget::Elf => vec![elf_seed()],
        FuzzTarget::BootPayload => vec![
            crate::bundle::build_bundle(
                &crate::bundle::render_manifest("fuzz", &[])
                    .expect("static manifest seed must build"),
                b"fuzz-elf",
            )
            .expect("static bundle seed must build")
            .bytes()
            .to_vec(),
        ],
    }
}

/// BPBが矛盾しない最小のFAT32 image。boot+FAT+root dirの3 sectorで、
/// mountからroot走査まで到達する。実際のcluster数要件(65,525)を満たす
/// volume sizeをBPBに宣言する。
fn fat32_image_seed() -> Vec<u8> {
    let mut boot = [0u8; 512];
    boot[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSWIN4.1");
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&32u16.to_le_bytes());
    boot[16] = 1;
    boot[21] = 0xf8;
    boot[32..36].copy_from_slice(&66_000u32.to_le_bytes());
    boot[36..40].copy_from_slice(&1u32.to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[510..512].copy_from_slice(&[0x55, 0xaa]);

    let mut fat = [0u8; 512];
    fat[0..4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
    fat[4..8].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    fat[8..12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());

    let mut image = Vec::with_capacity(512 * 34);
    image.extend_from_slice(&boot);
    image.extend_from_slice(&[0; 512 * 31]);
    image.extend_from_slice(&fat);
    image.extend_from_slice(&[0; 512]);
    image
}

/// program headerが1本の最小ELF64 (little-endian, RISC-V, EXEC)。
fn elf_seed() -> Vec<u8> {
    let mut bytes = vec![0u8; 64 + 56];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // ELFCLASS64
    bytes[5] = 1; // little-endian
    bytes[6] = 1; // version
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes()); // RISC-V
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0010_0000u64.to_le_bytes()); // entry
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes()); // ehsize
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes()); // phnum
    bytes[64..68].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    bytes[68..72].copy_from_slice(&5u32.to_le_bytes()); // PF_R|PF_X
    bytes[80..88].copy_from_slice(&0x0010_0000u64.to_le_bytes()); // vaddr
    bytes[96..104].copy_from_slice(&0x100u64.to_le_bytes()); // filesz
    bytes[104..112].copy_from_slice(&0x100u64.to_le_bytes()); // memsz
    bytes[112..120].copy_from_slice(&0x1000u64.to_le_bytes()); // align
    bytes
}

/// seedと設定から決定性の入力列を生成するiterator。
#[derive(Debug)]
pub struct InputStream {
    seeds: Vec<Vec<u8>>,
    rng: FuzzRng,
    max_bytes: usize,
}

impl InputStream {
    /// 入力列を作る。同じ引数は同じ列を生成する。
    pub fn new(seeds: Vec<Vec<u8>>, seed: u64, max_bytes: usize) -> Self {
        Self {
            seeds,
            rng: FuzzRng::new(seed),
            max_bytes,
        }
    }
}

impl Iterator for InputStream {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        if self.seeds.is_empty() {
            return None;
        }
        let seed = &self.seeds[self.rng.below(self.seeds.len())];
        Some(mutate(&mut self.rng, seed, self.max_bytes))
    }
}

/// 決定性の乱数生成器。xorshift64*。
#[derive(Debug, Clone)]
struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
        // xorshiftは0へ吸着するため、0だけを固定の奇数へ写す。
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// `bound`未満を返す。呼び出し側は1以上を渡す。
    /// 剰余の偏りはfuzz用途で無視する。
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < len {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

/// seedを変異させて一つの入力を作る。結果は`max_bytes`以下である。
fn mutate(rng: &mut FuzzRng, seed: &[u8], max_bytes: usize) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    bytes.truncate(max_bytes);
    // 空seedに有効な操作は挿入だけである。
    let op = if bytes.is_empty() { 2 } else { rng.below(8) };
    match op {
        0 => {
            let index = rng.below(bytes.len());
            bytes[index] ^= 1 << rng.below(8);
        }
        1 => {
            let index = rng.below(bytes.len());
            bytes[index] = rng.bytes(1)[0];
        }
        2 => {
            let count = 1 + rng.below(8);
            let position = rng.below(bytes.len() + 1);
            bytes.splice(position..position, rng.bytes(count));
        }
        3 => {
            let start = rng.below(bytes.len());
            let end = start + 1 + rng.below(bytes.len() - start);
            bytes.drain(start..end);
        }
        4 => {
            bytes.truncate(rng.below(bytes.len() + 1));
        }
        5 => {
            let start = rng.below(bytes.len());
            let width = 1 + rng.below((bytes.len() - start).min(32));
            let chunk = bytes[start..start + width].to_vec();
            let position = rng.below(bytes.len() + 1);
            bytes.splice(position..position, chunk);
        }
        6 => {
            let first = rng.below(bytes.len());
            let second = rng.below(bytes.len());
            bytes.swap(first, second);
        }
        _ => {
            const INTERESTING: [u32; 8] = [
                0,
                1,
                0xFF,
                0xFFFF,
                64 * 1024 - 1,
                64 * 1024,
                64 * 1024 + 1,
                u32::MAX,
            ];
            let value = INTERESTING[rng.below(INTERESTING.len())];
            let width = if rng.below(2) == 0 { 2 } else { 4 };
            if bytes.len() >= width {
                let position = rng.below(bytes.len() - width + 1);
                let encoded = value.to_le_bytes();
                bytes[position..position + width].copy_from_slice(&encoded[..width]);
            } else {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    bytes.truncate(max_bytes);
    bytes
}

/// 割り当ての現在量とpeakを数える計測器。
#[derive(Debug)]
struct PeakTracker {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl PeakTracker {
    const fn new() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    fn alloc(&self, size: usize) {
        let current = self.current.fetch_add(size, Ordering::SeqCst);
        self.peak
            .fetch_max(current.saturating_add(size), Ordering::SeqCst);
    }

    /// 計測開始前の確保の解放に対応するため飽和減算する。
    fn free(&self, size: usize) {
        let mut current = self.current.load(Ordering::SeqCst);
        while let Err(actual) = self.current.compare_exchange_weak(
            current,
            current.saturating_sub(size),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            current = actual;
        }
    }

    fn reset(&self) {
        self.current.store(0, Ordering::SeqCst);
        self.peak.store(0, Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

struct CountingAlloc;

static TRACKER: PeakTracker = PeakTracker::new();

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Systemへ委譲し、成功時のlayout sizeだけを数える。
        unsafe {
            let pointer = System.alloc(layout);
            if !pointer.is_null() {
                TRACKER.alloc(layout.size());
            }
            pointer
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: Systemへ委譲し、確保と同じsizeを戻す。
        unsafe {
            System.dealloc(pointer, layout);
            TRACKER.free(layout.size());
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Systemへ委譲し、成功時に差分だけを数える。
        unsafe {
            let grown = System.realloc(pointer, layout, new_size);
            if !grown.is_null() {
                if new_size >= layout.size() {
                    TRACKER.alloc(new_size - layout.size());
                } else {
                    TRACKER.free(layout.size() - new_size);
                }
            }
            grown
        }
    }
}

/// guard付きtaskの結果。
#[derive(Debug)]
enum GuardOutcome {
    Done { peak: usize },
    Panic { message: String },
    Timeout,
}

/// taskを分離threadで実行し、panicとtimeoutを検出する。
/// timeout時はworker threadを残したまま復帰する。CLIはprocess終了で回収し、
/// testは失敗後にbinary終了で回収する。
#[cfg(test)]
static GUARD_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_guarded(task: impl FnOnce() + Send + 'static, timeout: Duration) -> GuardOutcome {
    // test binaryは並列実行のため、peak計測だけを直列化する。本番は単threadである。
    #[cfg(test)]
    let _serial = GUARD_SERIAL.lock().expect("fuzz guard lock must be free");
    TRACKER.reset();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(())) => GuardOutcome::Done {
            peak: TRACKER.peak(),
        },
        Ok(Err(payload)) => GuardOutcome::Panic {
            message: panic_message(&payload),
        },
        Err(_) => GuardOutcome::Timeout,
    }
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "unknown panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_all_targets_by_cli_name() {
        assert_eq!(FuzzTarget::parse("fat32"), Some(FuzzTarget::Fat32));
        assert_eq!(FuzzTarget::parse("elf"), Some(FuzzTarget::Elf));
        assert_eq!(
            FuzzTarget::parse("boot-payload"),
            Some(FuzzTarget::BootPayload)
        );
        assert_eq!(FuzzTarget::parse("qemu"), None);
        assert_eq!(FuzzTarget::parse(""), None);
        assert_eq!(FuzzTarget::Fat32.name(), "fat32");
        assert_eq!(FuzzTarget::Elf.name(), "elf");
        assert_eq!(FuzzTarget::BootPayload.name(), "boot-payload");
    }

    #[test]
    fn rng_replays_the_same_stream_for_the_same_seed() {
        let first = FuzzRng::new(27);
        let second = FuzzRng::new(27);
        let third = FuzzRng::new(28);

        let first_stream = stream(first, 64);
        let second_stream = stream(second, 64);
        let third_stream = stream(third, 64);

        assert_eq!(first_stream, second_stream);
        assert_ne!(first_stream, third_stream);
        assert!(first_stream.iter().any(|value| *value != 0));
    }

    #[test]
    fn rng_seed_zero_does_not_stick_at_zero() {
        let values = stream(FuzzRng::new(0), 8);

        assert!(values.iter().all(|value| *value != 0));
    }

    #[test]
    fn mutate_stays_within_max_bytes() {
        let mut rng = FuzzRng::new(11);
        let seed = vec![7u8; 100];
        for _ in 0..500 {
            let output = mutate(&mut rng, &seed, 64);
            assert!(output.len() <= 64);
        }
        let empty = mutate(&mut rng, &[], 64);
        assert!(empty.len() <= 64);
    }

    #[test]
    fn mutate_replays_identically_for_the_same_seed() {
        let seed = vec![3u8; 80];
        let mut first = FuzzRng::new(9);
        let mut second = FuzzRng::new(9);
        for _ in 0..20 {
            assert_eq!(
                mutate(&mut first, &seed, 128),
                mutate(&mut second, &seed, 128)
            );
        }
    }

    #[test]
    fn run_input_passes_and_reports_alloc_peak() {
        for target in [FuzzTarget::Fat32, FuzzTarget::Elf, FuzzTarget::BootPayload] {
            let seeds = builtin_seeds(target);
            let outcome = run_input(target, &seeds[0], Duration::from_secs(5), DEFAULT_ALLOC_CAP);
            assert!(
                matches!(outcome, InputOutcome::Pass { .. }),
                "{target:?}: {outcome:?}"
            );
        }
    }

    #[test]
    fn run_input_treats_rejected_input_as_pass() {
        let outcome = run_input(
            FuzzTarget::Elf,
            b"not an elf",
            Duration::from_secs(5),
            DEFAULT_ALLOC_CAP,
        );
        assert!(matches!(outcome, InputOutcome::Pass { .. }));
    }

    #[test]
    fn panic_messages_extract_text() {
        let text: Box<dyn Any + Send> = Box::new("boom".to_owned());
        assert_eq!(panic_message(&text), "boom");
        let raw: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_message(&raw), "boom");
    }

    #[test]
    fn run_rejects_missing_corpus_and_zero_iters() {
        let config = FuzzConfig {
            target: FuzzTarget::Elf,
            seed: 1,
            iters: 0,
            max_bytes: 128,
            input_timeout: Duration::from_secs(1),
            time_limit: None,
            alloc_cap: DEFAULT_ALLOC_CAP,
            corpus_dir: PathBuf::from("nonexistent-corpus-dir"),
            output_dir: PathBuf::from("unused"),
            replay_input: None,
        };
        assert_eq!(
            run(&config),
            Err(FuzzError::InvalidConfig("iters must be at least 1"))
        );

        let config = FuzzConfig { iters: 1, ..config };
        assert!(
            matches!(run(&config), Err(FuzzError::MissingCorpus { .. })),
            "missing corpus must fail before generating inputs"
        );
    }

    #[test]
    fn run_replays_one_input_from_a_file() {
        let path = std::env::temp_dir().join(format!("minios-fuzz-seed-{}", std::process::id()));
        std::fs::write(&path, elf_seed()).unwrap();
        let config = FuzzConfig {
            target: FuzzTarget::Elf,
            seed: 1,
            iters: 1,
            max_bytes: 128,
            input_timeout: Duration::from_secs(5),
            time_limit: None,
            alloc_cap: DEFAULT_ALLOC_CAP,
            corpus_dir: PathBuf::from("unused"),
            output_dir: PathBuf::from("unused"),
            replay_input: Some(path.clone()),
        };
        let report = run(&config).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(report.iters_run, 1);
    }

    fn stream(mut rng: FuzzRng, count: usize) -> Vec<u64> {
        (0..count).map(|_| rng.next_u64()).collect()
    }
}
