//! MiniOS user shell（`SH.ELF`）。
//!
//! stdinを1行ずつ読み、`|`で区切った最大3個のcommandをpipelineとして
//! 起動する。最初のcommandには`< file`、最後のcommandには`> file`を付け
//! られる。`/`を含まない名前`NAME`は`BIN/NAME.ELF`（大文字）へ、`/`を
//! 含む語はそのままpathとして解決する。built-inは`exit [code]`だけで、
//! cwdは持たない。stdinのEOFで0で終了する。`-i`を渡すと`$ `を出す。
//!
//! 各commandは「dup2してからspawn」で起動する。kernelは`spawn`時に
//! fd tableを複製するため、shell自身のfd 0/1をpipe端やredirect先へ
//! 付け替えてからspawnし、退避しておいたconsoleで戻す。pipeのwrite端は
//! 書き手のchildだけが持つため、書き手の終了で読み手がEOFを得る。
//! childは起動順に`waitpid`し、最後のcommandの終了codeを覚える。
//!
//! process tableは`MAX_PROCS`=4（shellを含む）なので、3段のpipelineで
//! 満杯になる。spawnの失敗（`ENOENT`、満杯やframe不足の`ENOMEM`など）は
//! stderrへ1行出して次の行へ進む。

#![no_std]
#![no_main]

use minios_abi::syscall::{
    EINVAL, ENOENT, ENOMEM, FD_TABLE_LEN, MAX_PATH_LEN, SPAWN_MAX_ARGC, STDIN, STDOUT,
};
use minios_guest::{
    Args, Errno, eprintln,
    fs::File,
    io, print,
    process::{self, Pid, dup2, pipe, spawn, wait},
};

/// 1行の上限。超えた行はerrorを出して捨てる。
const LINE_MAX: usize = 256;
/// pipelineの段数上限。shellと合わせてkernelの`MAX_PROCS`=4に収まる。
const MAX_STAGES: usize = 3;
/// consoleのstdin/stdoutを退避しておくfd。tableの末尾2個を使う。
const SAVED_STDIN: usize = FD_TABLE_LEN - 2;
const SAVED_STDOUT: usize = FD_TABLE_LEN - 1;
/// 構文errorの終了status。
const SYNTAX_STATUS: i32 = 2;
/// 起動できなかったcommandの終了status。
const SPAWN_STATUS: i32 = 127;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

/// 1個のcommand。`argv[0]`がcommand名である。
#[derive(Clone, Copy)]
struct Stage<'a> {
    argv: [&'a [u8]; SPAWN_MAX_ARGC],
    argc: usize,
}

struct Pipeline<'a> {
    stages: [Stage<'a>; MAX_STAGES],
    count: usize,
    input: Option<&'a [u8]>,
    output: Option<&'a [u8]>,
}

/// 空白で区切った語と、1文字の演算子`|`、`<`、`>`を順に返す。
fn tokens(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = line;
    core::iter::from_fn(move || {
        while let [b' ' | b'\t' | b'\r', tail @ ..] = rest {
            rest = tail;
        }
        let first = *rest.first()?;
        let len = if matches!(first, b'|' | b'<' | b'>') {
            1
        } else {
            rest.iter()
                .position(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'|' | b'<' | b'>'))
                .unwrap_or(rest.len())
        };
        let (token, tail) = rest.split_at(len);
        rest = tail;
        Some(token)
    })
}

/// 1行をpipelineへ分解する。空行は`Ok(None)`。
fn parse(line: &[u8]) -> Result<Option<Pipeline<'_>>, &'static str> {
    let empty = Stage {
        argv: [&[]; SPAWN_MAX_ARGC],
        argc: 0,
    };
    let mut pipeline = Pipeline {
        stages: [empty; MAX_STAGES],
        count: 1,
        input: None,
        output: None,
    };
    let mut redirect: Option<u8> = None;
    for token in tokens(line) {
        let stage = &mut pipeline.stages[pipeline.count - 1];
        match token {
            b"|" => {
                if redirect.is_some() || stage.argc == 0 {
                    return Err("syntax error near '|'");
                }
                if pipeline.output.is_some() {
                    return Err("'>' is only allowed on the last command");
                }
                if pipeline.count == MAX_STAGES {
                    return Err("too many commands in a pipeline (max 3)");
                }
                pipeline.count += 1;
            }
            b"<" | b">" => {
                if redirect.is_some() {
                    return Err("syntax error near redirection");
                }
                redirect = Some(token[0]);
            }
            word => match redirect.take() {
                Some(b'<') => {
                    if pipeline.count != 1 || pipeline.input.is_some() {
                        return Err("'<' is only allowed once on the first command");
                    }
                    pipeline.input = Some(word);
                }
                Some(_) => {
                    if pipeline.output.is_some() {
                        return Err("'>' is only allowed once");
                    }
                    pipeline.output = Some(word);
                }
                None => {
                    if stage.argc == SPAWN_MAX_ARGC {
                        return Err("too many arguments");
                    }
                    stage.argv[stage.argc] = word;
                    stage.argc += 1;
                }
            },
        }
    }
    if redirect.is_some() {
        return Err("missing file name after redirection");
    }
    let last = &pipeline.stages[pipeline.count - 1];
    if last.argc == 0 {
        if pipeline.count == 1 && pipeline.input.is_none() && pipeline.output.is_none() {
            return Ok(None);
        }
        return Err("missing command");
    }
    Ok(Some(pipeline))
}

/// command名をpathへ解決する。`/`を含めばそのまま、含まなければ
/// `BIN/NAME.ELF`（大文字）にする。
fn resolve<'a>(name: &'a [u8], buffer: &'a mut [u8; MAX_PATH_LEN]) -> &'a [u8] {
    if name.contains(&b'/') {
        return name;
    }
    let mut len = 0;
    for &byte in b"BIN/".iter().chain(name).chain(b".ELF") {
        if len == MAX_PATH_LEN {
            // kernelが`EINVAL`で拒否する長さのままspawnへ渡す。
            break;
        }
        buffer[len] = byte.to_ascii_uppercase();
        len += 1;
    }
    &buffer[..len]
}

fn report_spawn(name: &[u8], Errno(errno): Errno) {
    let reason = match errno {
        ENOENT => "not found",
        EINVAL => "not an executable or bad arguments",
        ENOMEM => "no free process slot (max 4 including sh) or out of memory",
        _ => "cannot spawn",
    };
    eprintln!("sh: {}: {reason} (errno {errno})", text(name));
}

/// 読み手のいないpipeへ書くchildがbufferを埋めて止まらないよう、`fd`を
/// EOFまで読み捨てる。後段のspawnに失敗したときに使う。
fn drain(file: &mut File) {
    let mut chunk = [0u8; 256];
    while let Ok(1..) = file.read(&mut chunk) {}
}

/// pipelineを起動して全childを待ち、最後のcommandの終了codeを返す。
fn run(pipeline: &Pipeline<'_>) -> i32 {
    // redirect先はspawn前に開き、失敗したら何も起動しない。
    let mut upstream = match pipeline.input.map(File::open).transpose() {
        Ok(file) => file,
        Err(Errno(errno)) => {
            let name = text(pipeline.input.unwrap_or_default());
            eprintln!("sh: {name}: cannot open (errno {errno})");
            return 1;
        }
    };
    let output = match pipeline.output.map(File::create).transpose() {
        Ok(file) => file,
        Err(Errno(errno)) => {
            let name = text(pipeline.output.unwrap_or_default());
            eprintln!("sh: {name}: cannot create (errno {errno})");
            return 1;
        }
    };

    let mut pids: [Pid; MAX_STAGES] = [0; MAX_STAGES];
    let mut spawned = 0;
    let mut status = 0;
    for (index, stage) in pipeline.stages[..pipeline.count].iter().enumerate() {
        // このcommandのstdinは前段のpipeのread端か`< file`、なければconsole。
        let source = upstream.take();
        if let Some(file) = &source {
            dup2(file.as_raw_fd(), STDIN).unwrap();
        }
        // stdoutは次段へのpipeのwrite端か`> file`、なければconsole。
        let mut downstream = None;
        if index + 1 < pipeline.count {
            match pipe() {
                Ok((reader, writer)) => {
                    dup2(writer.as_raw_fd(), STDOUT).unwrap();
                    downstream = Some(reader);
                }
                Err(Errno(errno)) => {
                    dup2(SAVED_STDIN, STDIN).unwrap();
                    eprintln!("sh: cannot create pipe (errno {errno})");
                    status = 1;
                    break;
                }
            }
        } else if let Some(file) = &output {
            dup2(file.as_raw_fd(), STDOUT).unwrap();
        }

        let argv = &stage.argv[..stage.argc];
        let mut path = [0u8; MAX_PATH_LEN];
        let result = spawn(resolve(argv[0], &mut path), argv);

        // childはfd tableの複製を持つので、shell側のfd 0/1をconsoleへ戻す。
        // これでshellが持っていたpipeのwrite端は消え、書き手はchildだけになる。
        dup2(SAVED_STDIN, STDIN).unwrap();
        dup2(SAVED_STDOUT, STDOUT).unwrap();
        match result {
            Ok(pid) => {
                pids[spawned] = pid;
                spawned += 1;
                upstream = downstream;
            }
            Err(errno) => {
                report_spawn(argv[0], errno);
                status = SPAWN_STATUS;
                if let (Some(mut file), true) = (source, index > 0) {
                    drain(&mut file);
                }
                break;
            }
        }
    }

    // 起動順に待つ。前段は後段より先に終わるとは限らないが、順に待てば
    // 全員を回収でき、終了codeは最後のcommandのものが残る。
    for (index, &pid) in pids[..spawned].iter().enumerate() {
        match wait(pid) {
            Ok(code) if index + 1 == pipeline.count => status = code,
            Ok(_) => {}
            Err(Errno(errno)) => {
                eprintln!("sh: wait {pid}: errno {errno}");
                status = 1;
            }
        }
    }
    status
}

/// `exit [code]`の引数を10進数として読む。
fn parse_code(word: &[u8]) -> Option<i32> {
    let (negative, digits) = match word {
        [b'-', rest @ ..] => (true, rest),
        _ => (false, word),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i32 = 0;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(i32::from(byte - b'0'))?;
    }
    Some(if negative { -value } else { value })
}

/// 1行を実行し、新しい終了statusを返す。
fn execute(line: &[u8], status: i32) -> i32 {
    let pipeline = match parse(line) {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return status,
        Err(message) => {
            eprintln!("sh: {message}");
            return SYNTAX_STATUS;
        }
    };
    let first = &pipeline.stages[0];
    if pipeline.count == 1 && first.argv[0] == b"exit" {
        match first.argv[1..first.argc] {
            [] => process::exit(status),
            [word] => match parse_code(word) {
                Some(code) => process::exit(code),
                None => eprintln!("sh: exit: bad code {}", text(word)),
            },
            _ => eprintln!("sh: exit: too many arguments"),
        }
        return SYNTAX_STATUS;
    }
    run(&pipeline)
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let prompt = args.skip(1).any(|arg| arg == b"-i");
    dup2(STDIN, SAVED_STDIN).unwrap();
    dup2(STDOUT, SAVED_STDOUT).unwrap();

    let mut line = [0u8; LINE_MAX];
    let mut len = 0;
    let mut overflow = false;
    let mut chunk = [0u8; LINE_MAX];
    let mut status = 0;
    loop {
        if prompt && len == 0 {
            print!("$ ");
        }
        let read = io::read(STDIN, &mut chunk).unwrap();
        if read == 0 {
            // 改行のない最終行も1行として実行する。
            if len > 0 && !overflow {
                execute(&line[..len], status);
            }
            return 0;
        }
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if overflow {
                    eprintln!("sh: line too long (max {LINE_MAX} bytes)");
                } else {
                    status = execute(&line[..len], status);
                }
                len = 0;
                overflow = false;
            } else if len == LINE_MAX {
                overflow = true;
            } else {
                line[len] = byte;
                len += 1;
            }
        }
    }
}
