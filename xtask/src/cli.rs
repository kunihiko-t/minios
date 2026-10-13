use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
    Build,
    Run,
    Bundle(BundleOptions),
    Test(TestFilter),
    Check,
    Fuzz(FuzzOptions),
}

/// `cargo xtask bundle`の入力。`None`の項目はbundle側の既定値を使う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleOptions {
    pub name: Option<String>,
    pub args: Vec<String>,
    /// 複数image (manifest v2) bundleのguest bin名。空なら単一image v1を生成する。
    pub images: Vec<String>,
    pub output: Option<PathBuf>,
}

/// `cargo xtask fuzz`の入力。`None`の項目はfuzz側の既定値を使う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuzzOptions {
    /// 対象parser名。`fat32`、`elf`、`boot-payload`のいずれか。
    pub target: Option<String>,
    pub seed: Option<u64>,
    pub iters: Option<u64>,
    pub max_bytes: Option<usize>,
    /// 入力あたりtimeout(秒)。
    pub input_timeout: Option<u64>,
    /// 全体の制限時間(秒)。`None`は無制限。
    pub time_limit: Option<u64>,
    /// 入力あたりallocation上限(bytes)。
    pub alloc_cap: Option<usize>,
    pub corpus_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    /// 指定時はこの一つのfileを変異なしで再生する。
    pub input: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestFilter {
    All,
    Boot,
    Trap,
    Timer,
    Memory,
    Vm,
    Elf,
    UserEntry,
    UserTrap,
    UserSyscall,
    UserExit,
    KernelStack,
    Fdt,
    Heap,
    Virtio,
    Payload,
    PayloadArgs,
    PayloadStdin,
    File,
    FileFd,
    FileWrite,
    FileUnlink,
    FileSeek,
    FileRename,
    FileMkdir,
    FileSpawn,
    FileWaitpid,
    FileStat,
    FileReaddir,
    FileExec,
    FileFdinherit,
    FilePipe,
    UserHeap,
    UserSleep,
    UserDup,
    SpawnArgs,
    UserShell,
    Sched,
    SchedIo,
    SchedIoPartial,
    IrqUart,
    IrqBlk,
    NetArp,
    NetPing,
    NetUdp,
    NetSock,
    ProcFault,
    ProcKill,
    Shell,
    SmpBoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
    InvalidBundleOptions(String),
    InvalidFuzzOptions(String),
}

pub fn help() -> &'static str {
    "MiniOS development commands:\n\
  cargo xtask setup\n\
  cargo xtask build\n\
  cargo xtask run\n\
  cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]\n\
  cargo xtask bundle --image <guest-bin> [--image <guest-bin>]... [--output <path>]\n\
  cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|kernel-stack|fdt|heap|virtio|payload|payload-args|payload-stdin|file|file-fd|file-write|file-unlink|file-seek|file-rename|file-mkdir|file-spawn|file-waitpid|file-stat|file-readdir|file-exec|file-fdinherit|file-pipe|user-heap|user-sleep|user-dup|spawn-args|user-shell|sched|sched-io|sched-io-partial|irq-uart|irq-blk|net-arp|net-ping|net-udp|net-sock|proc-fault|proc-kill|shell|smp-boot]\n\
  cargo xtask check\n\
  cargo xtask fuzz --target <fat32|elf|boot-payload> [--seed <n>] [--iters <n>] [--max-bytes <n>] [--input-timeout <secs>] [--time-limit <secs>] [--alloc-cap <n>] [--corpus <dir>] [--output <dir>]\n\
  cargo xtask fuzz --target <fat32|elf|boot-payload> --input <file>"
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "setup" => Ok(Command::Setup),
        [command] if command == "build" => Ok(Command::Build),
        [command] if command == "run" => Ok(Command::Run),
        [command] if command == "test" => Ok(Command::Test(TestFilter::All)),
        [command, test] if command == "test" && test == "all" => Ok(Command::Test(TestFilter::All)),
        [command, test] if command == "test" && test == "boot" => {
            Ok(Command::Test(TestFilter::Boot))
        }
        [command, test] if command == "test" && test == "trap" => {
            Ok(Command::Test(TestFilter::Trap))
        }
        [command, test] if command == "test" && test == "timer" => {
            Ok(Command::Test(TestFilter::Timer))
        }
        [command, test] if command == "test" && test == "memory" => {
            Ok(Command::Test(TestFilter::Memory))
        }
        [command, test] if command == "test" && test == "vm" => Ok(Command::Test(TestFilter::Vm)),
        [command, test] if command == "test" && test == "elf" => Ok(Command::Test(TestFilter::Elf)),
        [command, test] if command == "test" && test == "user-entry" => {
            Ok(Command::Test(TestFilter::UserEntry))
        }
        [command, test] if command == "test" && test == "user-trap" => {
            Ok(Command::Test(TestFilter::UserTrap))
        }
        [command, test] if command == "test" && test == "user-syscall" => {
            Ok(Command::Test(TestFilter::UserSyscall))
        }
        [command, test] if command == "test" && test == "user-exit" => {
            Ok(Command::Test(TestFilter::UserExit))
        }
        [command, test] if command == "test" && test == "kernel-stack" => {
            Ok(Command::Test(TestFilter::KernelStack))
        }
        [command, test] if command == "test" && test == "fdt" => Ok(Command::Test(TestFilter::Fdt)),
        [command, test] if command == "test" && test == "heap" => {
            Ok(Command::Test(TestFilter::Heap))
        }
        [command, test] if command == "test" && test == "virtio" => {
            Ok(Command::Test(TestFilter::Virtio))
        }
        [command, test] if command == "test" && test == "payload" => {
            Ok(Command::Test(TestFilter::Payload))
        }
        [command, test] if command == "test" && test == "payload-args" => {
            Ok(Command::Test(TestFilter::PayloadArgs))
        }
        [command, test] if command == "test" && test == "payload-stdin" => {
            Ok(Command::Test(TestFilter::PayloadStdin))
        }
        [command, test] if command == "test" && test == "file" => {
            Ok(Command::Test(TestFilter::File))
        }
        [command, test] if command == "test" && test == "file-fd" => {
            Ok(Command::Test(TestFilter::FileFd))
        }
        [command, test] if command == "test" && test == "file-write" => {
            Ok(Command::Test(TestFilter::FileWrite))
        }
        [command, test] if command == "test" && test == "file-unlink" => {
            Ok(Command::Test(TestFilter::FileUnlink))
        }
        [command, test] if command == "test" && test == "file-seek" => {
            Ok(Command::Test(TestFilter::FileSeek))
        }
        [command, test] if command == "test" && test == "file-rename" => {
            Ok(Command::Test(TestFilter::FileRename))
        }
        [command, test] if command == "test" && test == "file-mkdir" => {
            Ok(Command::Test(TestFilter::FileMkdir))
        }
        [command, test] if command == "test" && test == "file-spawn" => {
            Ok(Command::Test(TestFilter::FileSpawn))
        }
        [command, test] if command == "test" && test == "file-waitpid" => {
            Ok(Command::Test(TestFilter::FileWaitpid))
        }
        [command, test] if command == "test" && test == "file-stat" => {
            Ok(Command::Test(TestFilter::FileStat))
        }
        [command, test] if command == "test" && test == "file-readdir" => {
            Ok(Command::Test(TestFilter::FileReaddir))
        }
        [command, test] if command == "test" && test == "file-exec" => {
            Ok(Command::Test(TestFilter::FileExec))
        }
        [command, test] if command == "test" && test == "file-fdinherit" => {
            Ok(Command::Test(TestFilter::FileFdinherit))
        }
        [command, test] if command == "test" && test == "file-pipe" => {
            Ok(Command::Test(TestFilter::FilePipe))
        }
        [command, test] if command == "test" && test == "user-heap" => {
            Ok(Command::Test(TestFilter::UserHeap))
        }
        [command, test] if command == "test" && test == "user-sleep" => {
            Ok(Command::Test(TestFilter::UserSleep))
        }
        [command, test] if command == "test" && test == "user-dup" => {
            Ok(Command::Test(TestFilter::UserDup))
        }
        [command, test] if command == "test" && test == "spawn-args" => {
            Ok(Command::Test(TestFilter::SpawnArgs))
        }
        [command, test] if command == "test" && test == "user-shell" => {
            Ok(Command::Test(TestFilter::UserShell))
        }
        [command, test] if command == "test" && test == "sched" => {
            Ok(Command::Test(TestFilter::Sched))
        }
        [command, test] if command == "test" && test == "sched-io" => {
            Ok(Command::Test(TestFilter::SchedIo))
        }
        [command, test] if command == "test" && test == "sched-io-partial" => {
            Ok(Command::Test(TestFilter::SchedIoPartial))
        }
        [command, test] if command == "test" && test == "irq-uart" => {
            Ok(Command::Test(TestFilter::IrqUart))
        }
        [command, test] if command == "test" && test == "irq-blk" => {
            Ok(Command::Test(TestFilter::IrqBlk))
        }
        [command, test] if command == "test" && test == "net-arp" => {
            Ok(Command::Test(TestFilter::NetArp))
        }
        [command, test] if command == "test" && test == "net-ping" => {
            Ok(Command::Test(TestFilter::NetPing))
        }
        [command, test] if command == "test" && test == "net-udp" => {
            Ok(Command::Test(TestFilter::NetUdp))
        }
        [command, test] if command == "test" && test == "net-sock" => {
            Ok(Command::Test(TestFilter::NetSock))
        }
        [command, test] if command == "test" && test == "proc-fault" => {
            Ok(Command::Test(TestFilter::ProcFault))
        }
        [command, test] if command == "test" && test == "proc-kill" => {
            Ok(Command::Test(TestFilter::ProcKill))
        }
        [command, test] if command == "test" && test == "smp-boot" => {
            Ok(Command::Test(TestFilter::SmpBoot))
        }
        [command, test] if command == "test" && test == "shell" => {
            Ok(Command::Test(TestFilter::Shell))
        }
        [command] if command == "check" => Ok(Command::Check),
        [command, options @ ..] if command == "bundle" => {
            parse_bundle_options(options).map(Command::Bundle)
        }
        [command, options @ ..] if command == "fuzz" => {
            parse_fuzz_options(options).map(Command::Fuzz)
        }
        [] => Err(CliError::MissingCommand),
        [command, ..] => Err(CliError::UnknownCommand(command.clone())),
    }
}

/// `bundle`以降のoption列をparseする。`--arg`だけが繰り返し可能で、
/// 値の意味検査 (文字種や上限) はbundle生成側の責務である。
fn parse_bundle_options(options: &[String]) -> Result<BundleOptions, CliError> {
    let mut parsed = BundleOptions::default();
    let mut index = 0;
    while index < options.len() {
        let flag = options[index].as_str();
        match flag {
            "--name" | "--arg" | "--output" | "--image" => {
                let Some(value) = options.get(index + 1) else {
                    return Err(CliError::InvalidBundleOptions(format!(
                        "bundle option {flag} requires a value"
                    )));
                };
                match flag {
                    "--name" => {
                        if parsed.name.replace(value.clone()).is_some() {
                            return Err(CliError::InvalidBundleOptions(
                                "duplicate bundle option: --name".to_owned(),
                            ));
                        }
                    }
                    "--arg" => parsed.args.push(value.clone()),
                    "--image" => parsed.images.push(value.clone()),
                    _ => {
                        if parsed.output.replace(PathBuf::from(value)).is_some() {
                            return Err(CliError::InvalidBundleOptions(
                                "duplicate bundle option: --output".to_owned(),
                            ));
                        }
                    }
                }
                index += 2;
            }
            other => {
                return Err(CliError::InvalidBundleOptions(format!(
                    "unknown bundle option: {other}"
                )));
            }
        }
    }
    // 複数imageモードでは`--name`/`--arg`は単一image専用のため併用不可にし、
    // image数はmanifest format上限と同じ4までに制限する。
    if !parsed.images.is_empty() && (parsed.name.is_some() || !parsed.args.is_empty()) {
        return Err(CliError::InvalidBundleOptions(
            "--image cannot be combined with --name or --arg".to_owned(),
        ));
    }
    if parsed.images.len() > minios_abi::manifest::IMAGE_MAX_COUNT {
        return Err(CliError::InvalidBundleOptions(format!(
            "too many --image options (max {})",
            minios_abi::manifest::IMAGE_MAX_COUNT
        )));
    }
    Ok(parsed)
}

/// `fuzz`以降のoption列をparseする。数値とpathの意味検査 (上限や存在確認)
/// はfuzz実行側の責務である。`--target`の必須化もここで行う。
fn parse_fuzz_options(options: &[String]) -> Result<FuzzOptions, CliError> {
    let mut parsed = FuzzOptions::default();
    let mut index = 0;
    while index < options.len() {
        let flag = options[index].as_str();
        let Some(value) = options.get(index + 1) else {
            return Err(CliError::InvalidFuzzOptions(format!(
                "fuzz option {flag} requires a value"
            )));
        };
        match flag {
            "--target" => set_once(&mut parsed.target, value.clone(), flag)?,
            "--seed" => set_once(&mut parsed.seed, parse_num(value, flag)?, flag)?,
            "--iters" => set_once(&mut parsed.iters, parse_num(value, flag)?, flag)?,
            "--max-bytes" => set_once(&mut parsed.max_bytes, parse_num(value, flag)?, flag)?,
            "--input-timeout" => {
                set_once(&mut parsed.input_timeout, parse_num(value, flag)?, flag)?
            }
            "--time-limit" => set_once(&mut parsed.time_limit, parse_num(value, flag)?, flag)?,
            "--alloc-cap" => set_once(&mut parsed.alloc_cap, parse_num(value, flag)?, flag)?,
            "--corpus" => set_once(&mut parsed.corpus_dir, PathBuf::from(value), flag)?,
            "--output" => set_once(&mut parsed.output_dir, PathBuf::from(value), flag)?,
            "--input" => set_once(&mut parsed.input, PathBuf::from(value), flag)?,
            other => {
                return Err(CliError::InvalidFuzzOptions(format!(
                    "unknown fuzz option: {other}"
                )));
            }
        }
        index += 2;
    }
    if parsed.target.is_none() {
        return Err(CliError::InvalidFuzzOptions(
            "fuzz requires --target <fat32|elf|boot-payload>".to_owned(),
        ));
    }
    Ok(parsed)
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), CliError> {
    if slot.replace(value).is_some() {
        return Err(CliError::InvalidFuzzOptions(format!(
            "duplicate fuzz option: {flag}"
        )));
    }
    Ok(())
}

fn parse_num<T: std::str::FromStr>(value: &str, flag: &str) -> Result<T, CliError> {
    value.parse::<T>().map_err(|_| {
        CliError::InvalidFuzzOptions(format!("fuzz option {flag} expects a number: {value}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_all_public_commands() {
        assert_eq!(parse(&owned(&["setup"])), Ok(Command::Setup));
        assert_eq!(parse(&owned(&["build"])), Ok(Command::Build));
        assert_eq!(parse(&owned(&["run"])), Ok(Command::Run));
        assert_eq!(parse(&owned(&["test"])), Ok(Command::Test(TestFilter::All)));
        assert_eq!(
            parse(&owned(&["test", "timer"])),
            Ok(Command::Test(TestFilter::Timer))
        );
        assert_eq!(parse(&owned(&["check"])), Ok(Command::Check));
        assert_eq!(
            parse(&owned(&["fuzz", "--target", "elf"])),
            Ok(Command::Fuzz(FuzzOptions {
                target: Some("elf".to_owned()),
                ..FuzzOptions::default()
            }))
        );
    }

    #[test]
    fn parses_fuzz_options_and_rejects_bad_ones() {
        assert_eq!(
            parse(&owned(&[
                "fuzz",
                "--target",
                "fat32",
                "--seed",
                "9",
                "--iters",
                "250",
                "--max-bytes",
                "4096",
                "--input-timeout",
                "2",
                "--time-limit",
                "60",
                "--alloc-cap",
                "1048576",
                "--corpus",
                "corpus",
                "--output",
                "out",
            ])),
            Ok(Command::Fuzz(FuzzOptions {
                target: Some("fat32".to_owned()),
                seed: Some(9),
                iters: Some(250),
                max_bytes: Some(4096),
                input_timeout: Some(2),
                time_limit: Some(60),
                alloc_cap: Some(1048576),
                corpus_dir: Some(PathBuf::from("corpus")),
                output_dir: Some(PathBuf::from("out")),
                input: None,
            }))
        );
        assert_eq!(
            parse(&owned(&[
                "fuzz",
                "--target",
                "boot-payload",
                "--input",
                "finding.bin",
            ])),
            Ok(Command::Fuzz(FuzzOptions {
                target: Some("boot-payload".to_owned()),
                input: Some(PathBuf::from("finding.bin")),
                ..FuzzOptions::default()
            }))
        );
        assert!(matches!(
            parse(&owned(&["fuzz"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
        assert!(matches!(
            parse(&owned(&["fuzz", "--iters", "1"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
        assert!(matches!(
            parse(&owned(&["fuzz", "--target", "elf", "--seed"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
        assert!(matches!(
            parse(&owned(&["fuzz", "--target", "elf", "--seed", "abc"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
        assert!(matches!(
            parse(&owned(&["fuzz", "--target", "elf", "--target", "fat32"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
        assert!(matches!(
            parse(&owned(&["fuzz", "--target", "elf", "--bogus", "1"])),
            Err(CliError::InvalidFuzzOptions(_))
        ));
    }

    #[test]
    fn parses_explicit_all_and_every_individual_test_filter() {
        for (args, expected) in [
            (vec!["test", "all"], TestFilter::All),
            (vec!["test", "boot"], TestFilter::Boot),
            (vec!["test", "trap"], TestFilter::Trap),
            (vec!["test", "timer"], TestFilter::Timer),
            (vec!["test", "memory"], TestFilter::Memory),
            (vec!["test", "vm"], TestFilter::Vm),
            (vec!["test", "elf"], TestFilter::Elf),
            (vec!["test", "user-entry"], TestFilter::UserEntry),
            (vec!["test", "user-trap"], TestFilter::UserTrap),
            (vec!["test", "user-syscall"], TestFilter::UserSyscall),
            (vec!["test", "user-exit"], TestFilter::UserExit),
            (vec!["test", "kernel-stack"], TestFilter::KernelStack),
            (vec!["test", "fdt"], TestFilter::Fdt),
            (vec!["test", "heap"], TestFilter::Heap),
            (vec!["test", "virtio"], TestFilter::Virtio),
            (vec!["test", "payload"], TestFilter::Payload),
            (vec!["test", "payload-args"], TestFilter::PayloadArgs),
            (vec!["test", "payload-stdin"], TestFilter::PayloadStdin),
            (vec!["test", "file"], TestFilter::File),
            (vec!["test", "file-fd"], TestFilter::FileFd),
            (vec!["test", "file-write"], TestFilter::FileWrite),
            (vec!["test", "file-unlink"], TestFilter::FileUnlink),
            (vec!["test", "file-seek"], TestFilter::FileSeek),
            (vec!["test", "file-rename"], TestFilter::FileRename),
            (vec!["test", "file-mkdir"], TestFilter::FileMkdir),
            (vec!["test", "file-spawn"], TestFilter::FileSpawn),
            (vec!["test", "file-waitpid"], TestFilter::FileWaitpid),
            (vec!["test", "file-stat"], TestFilter::FileStat),
            (vec!["test", "file-readdir"], TestFilter::FileReaddir),
            (vec!["test", "file-exec"], TestFilter::FileExec),
            (vec!["test", "file-fdinherit"], TestFilter::FileFdinherit),
            (vec!["test", "file-pipe"], TestFilter::FilePipe),
            (vec!["test", "user-heap"], TestFilter::UserHeap),
            (vec!["test", "user-sleep"], TestFilter::UserSleep),
            (vec!["test", "user-dup"], TestFilter::UserDup),
            (vec!["test", "spawn-args"], TestFilter::SpawnArgs),
            (vec!["test", "user-shell"], TestFilter::UserShell),
            (vec!["test", "sched"], TestFilter::Sched),
            (vec!["test", "sched-io"], TestFilter::SchedIo),
            (vec!["test", "sched-io-partial"], TestFilter::SchedIoPartial),
            (vec!["test", "irq-uart"], TestFilter::IrqUart),
            (vec!["test", "irq-blk"], TestFilter::IrqBlk),
            (vec!["test", "net-arp"], TestFilter::NetArp),
            (vec!["test", "net-ping"], TestFilter::NetPing),
            (vec!["test", "net-udp"], TestFilter::NetUdp),
            (vec!["test", "net-sock"], TestFilter::NetSock),
            (vec!["test", "proc-fault"], TestFilter::ProcFault),
            (vec!["test", "proc-kill"], TestFilter::ProcKill),
            (vec!["test", "shell"], TestFilter::Shell),
            (vec!["test", "smp-boot"], TestFilter::SmpBoot),
        ] {
            assert_eq!(parse(&owned(&args)), Ok(Command::Test(expected)));
        }
    }

    #[test]
    fn rejects_unknown_command_with_helpful_name() {
        let args = vec!["unknown".to_owned()];
        assert_eq!(
            parse(&args),
            Err(CliError::UnknownCommand("unknown".to_owned()))
        );
    }

    #[test]
    fn help_advertises_every_public_command_and_test_filter() {
        let help = help();

        for command in [
            "cargo xtask setup",
            "cargo xtask build",
            "cargo xtask run",
            "cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]",
            "cargo xtask bundle --image <guest-bin> [--image <guest-bin>]... [--output <path>]",
            "cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|kernel-stack|fdt|heap|virtio|payload|payload-args|payload-stdin|file|file-fd|file-write|file-unlink|file-seek|file-rename|file-mkdir|file-spawn|file-waitpid|file-stat|file-readdir|file-exec|file-fdinherit|file-pipe|user-heap|user-sleep|user-dup|spawn-args|user-shell|sched|sched-io|sched-io-partial|irq-uart|irq-blk|net-arp|net-ping|net-udp|net-sock|proc-fault|proc-kill|shell|smp-boot]",
            "cargo xtask check",
        ] {
            assert!(help.contains(command), "missing help entry: {command}");
        }
    }

    #[test]
    fn parses_bundle_with_defaults() {
        assert_eq!(
            parse(&owned(&["bundle"])),
            Ok(Command::Bundle(BundleOptions::default()))
        );
    }

    #[test]
    fn parses_bundle_with_name_repeated_args_and_output() {
        assert_eq!(
            parse(&owned(&[
                "bundle",
                "--name",
                "hello",
                "--arg",
                "alpha",
                "--arg",
                "bravo",
                "--output",
                "target/hello.mcb",
            ])),
            Ok(Command::Bundle(BundleOptions {
                name: Some("hello".to_owned()),
                args: vec!["alpha".to_owned(), "bravo".to_owned()],
                images: vec![],
                output: Some(PathBuf::from("target/hello.mcb")),
            }))
        );
    }

    #[test]
    fn parses_bundle_with_multiple_images() {
        assert_eq!(
            parse(&owned(&[
                "bundle",
                "--image",
                "sched-a",
                "--image",
                "sched-b",
                "--output",
                "target/multi.mcb",
            ])),
            Ok(Command::Bundle(BundleOptions {
                name: None,
                args: vec![],
                images: vec!["sched-a".to_owned(), "sched-b".to_owned()],
                output: Some(PathBuf::from("target/multi.mcb")),
            }))
        );
    }

    #[test]
    fn rejects_mixing_images_with_single_image_options() {
        for extra in ["--name", "--arg"] {
            assert_eq!(
                parse(&owned(&["bundle", "--image", "a", extra, "x"])),
                Err(CliError::InvalidBundleOptions(
                    "--image cannot be combined with --name or --arg".to_owned()
                )),
                "extra={extra}"
            );
        }
    }

    #[test]
    fn rejects_more_images_than_the_manifest_limit() {
        let mut args = vec!["bundle"];
        for name in ["a", "b", "c", "d", "e"] {
            args.push("--image");
            args.push(name);
        }
        assert_eq!(
            parse(&owned(&args)),
            Err(CliError::InvalidBundleOptions(
                "too many --image options (max 4)".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_bundle_option_without_a_value() {
        assert_eq!(
            parse(&owned(&["bundle", "--name"])),
            Err(CliError::InvalidBundleOptions(
                "bundle option --name requires a value".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_unknown_bundle_option_by_name() {
        assert_eq!(
            parse(&owned(&["bundle", "--compress"])),
            Err(CliError::InvalidBundleOptions(
                "unknown bundle option: --compress".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_duplicate_bundle_name_and_output() {
        assert_eq!(
            parse(&owned(&["bundle", "--name", "a", "--name", "b"])),
            Err(CliError::InvalidBundleOptions(
                "duplicate bundle option: --name".to_owned()
            ))
        );
        assert_eq!(
            parse(&owned(&["bundle", "--output", "a", "--output", "b"])),
            Err(CliError::InvalidBundleOptions(
                "duplicate bundle option: --output".to_owned()
            ))
        );
    }
}
