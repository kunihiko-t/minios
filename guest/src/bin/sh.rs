//! MiniOS user shell（`SH.ELF`）。
//!
//! stdinを1行ずつ読み、`|`で区切った最大7個のcommandをpipelineとして
//! 起動する。最初のcommandには`< file`、最後のcommandには`> file`を付け
//! られる。`"…"`か`'…'`で囲んだ語は空白や演算子を含められる。
//! 1行には`;`、`&&`、`||`でpipelineを並べられる。`;`は前の結果に関係なく、
//! `&&`は直前の終了statusが0のときだけ、`||`は0以外のときだけ次を実行する
//! （3つは同じ優先度で左から順に評価する）。末尾か区切りの`&`はその
//! pipelineをbackgroundで起動して待たずに次へ進み、built-inの`wait`が
//! まとめて待つ。backgroundのcommandのstdinは、write端を閉じたpipeの
//! read端（読めばすぐEOF）にして、shellとconsoleの入力を取り合わせない。
//! 引用符で囲まない語`$?`は直前の終了statusの10進数へ置き換わる。
//! `/`を含まない名前`NAME`は`/BIN/NAME.ELF`（大文字）へ、`/`を
//! 含む語はそのままpathとして解決する。built-inは`exit [code]`、`cd [dir]`、
//! `pwd`、`true`、`false`、`wait`で、作業directoryはkernelがprocessごとに持ち、
//! spawnしたchildへ引き継がれる。stdinのEOFで0で終了する。`-i`を渡すと
//! `$ `を出す。
//!
//! 各commandは「dup2してからspawn」で起動する。kernelは`spawn`時に
//! fd tableを複製するため、shell自身のfd 0/1をpipe端やredirect先へ
//! 付け替えてからspawnし、退避しておいたconsoleで戻す。shellが開くfd
//! （退避したconsole、pipeの両端、redirect先）にはclose-on-execを立てる
//! ため、childが継承するのは`dup2`で複製したfd 0/1/2だけである。
//! pipeのwrite端は書き手のchildだけが持つため書き手の終了で読み手がEOFを、
//! read端は読み手のchildだけが持つため読み手の終了で書き手が`EPIPE`を得る。
//! childは起動順に`waitpid`し、最後のcommandの終了codeを覚える。
//!
//! process tableは`MAX_PROCESSES`=8（shellを含む）なので、7段のpipelineで
//! 満杯になる。spawnの失敗（`ENOENT`、満杯やframe不足の`ENOMEM`など）は
//! stderrへ1行出して次の行へ進む。

#![no_std]
#![no_main]

use minios_abi::syscall::{
    EINVAL, ENOENT, ENOMEM, FD_TABLE_LEN, MAX_PATH_LEN, MAX_PROCESSES, SPAWN_MAX_ARGC, STDIN,
    STDOUT,
};
use minios_guest::{
    Args, Errno, eprintln,
    fs::{self, File},
    io, print, println,
    process::{self, Pid, dup2, pipe, set_cloexec, spawn, wait},
};

/// 1行の上限。超えた行はerrorを出して捨てる。
const LINE_MAX: usize = 256;
/// pipelineの段数上限。shellと合わせてkernelの`MAX_PROCESSES`に収まる。
const MAX_STAGES: usize = MAX_PROCESSES - 1;
// errorの文言は段数とprocess数を数字で書くため、上限が変わったら直す。
const _: () = assert!(MAX_STAGES == 7);
/// consoleのstdin/stdoutを退避しておくfd。tableの末尾2個を使う。
const SAVED_STDIN: usize = FD_TABLE_LEN - 2;
const SAVED_STDOUT: usize = FD_TABLE_LEN - 1;
/// 構文errorの終了status。
const SYNTAX_STATUS: i32 = 2;
/// 起動できなかったcommandの終了status。
const SPAWN_STATUS: i32 = 127;
/// backgroundで起動して、まだ`wait`していないprocessの上限。
const MAX_JOBS: usize = 16;

/// backgroundで起動したprocessの記録。
struct Jobs {
    pids: [Pid; MAX_JOBS],
    count: usize,
    /// `-i`で起動した対話shellなら、background起動のpidを`[pid]`で知らせる。
    interactive: bool,
}

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

/// 字句1個。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    /// 1文字の演算子`|`、`<`、`>`。
    Op(u8),
    /// 語。引用符で囲んだ語は引用符を除いた中身で、演算子や空白も含められる。
    /// `quoted`は引用符で囲んだ語か。囲んだ`"$?"`は置き換えない。
    Word { text: &'a [u8], quoted: bool },
}

/// 語を区切るbyte。
const fn is_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'|' | b'<' | b'>')
}

/// 空白で区切った語と、1文字の演算子`|`、`<`、`>`を順に返す。
///
/// `"…"`か`'…'`で囲んだ部分は1個の語になり、中の空白と演算子は文字として
/// 扱う（`cat "my notes.txt"`）。escapeはなく、引用符は語の全体を囲む必要が
/// ある。閉じていない引用符や、語の途中の引用符は構文errorである。
fn tokens(line: &[u8]) -> impl Iterator<Item = Result<Token<'_>, &'static str>> {
    let mut rest = line;
    core::iter::from_fn(move || {
        while let [b' ' | b'\t' | b'\r', tail @ ..] = rest {
            rest = tail;
        }
        let first = *rest.first()?;
        if matches!(first, b'|' | b'<' | b'>') {
            rest = &rest[1..];
            return Some(Ok(Token::Op(first)));
        }
        if matches!(first, b'"' | b'\'') {
            let body = &rest[1..];
            let Some(close) = body.iter().position(|&byte| byte == first) else {
                rest = &[];
                return Some(Err("unterminated quote"));
            };
            let word = &body[..close];
            rest = &body[close + 1..];
            if rest.first().is_some_and(|&byte| !is_separator(byte)) {
                rest = &[];
                return Some(Err("quotes must enclose a whole word"));
            }
            return Some(Ok(Token::Word {
                text: word,
                quoted: true,
            }));
        }
        let len = rest
            .iter()
            .position(|&byte| is_separator(byte))
            .unwrap_or(rest.len());
        let (word, tail) = rest.split_at(len);
        rest = tail;
        if word.iter().any(|&byte| matches!(byte, b'"' | b'\'')) {
            rest = &[];
            return Some(Err("quotes must enclose a whole word"));
        }
        Some(Ok(Token::Word {
            text: word,
            quoted: false,
        }))
    })
}

/// 1行をpipelineへ分解する。空行は`Ok(None)`。引用符で囲まない語`$?`は
/// `last_status`（直前の終了statusの10進数）へ置き換える。
fn parse<'a>(line: &'a [u8], last_status: &'a [u8]) -> Result<Option<Pipeline<'a>>, &'static str> {
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
        match token? {
            Token::Op(b'|') => {
                if redirect.is_some() || stage.argc == 0 {
                    return Err("syntax error near '|'");
                }
                if pipeline.output.is_some() {
                    return Err("'>' is only allowed on the last command");
                }
                if pipeline.count == MAX_STAGES {
                    return Err("too many commands in a pipeline (max 7)");
                }
                pipeline.count += 1;
            }
            Token::Op(op) => {
                if redirect.is_some() {
                    return Err("syntax error near redirection");
                }
                redirect = Some(op);
            }
            Token::Word { text, quoted } => {
                let word = if !quoted && text == b"$?" {
                    last_status
                } else {
                    text
                };
                match redirect.take() {
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
                }
            }
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
    // `cd`の後でも見つかるよう、root基準の絶対pathにする。
    for &byte in b"/BIN/".iter().chain(name).chain(b".ELF") {
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
        ENOMEM => "no free process slot (max 8 including sh) or out of memory",
        _ => "cannot spawn",
    };
    eprintln!("sh: {}: {reason} (errno {errno})", text(name));
}

/// shell自身だけが使うfdとして、childへ継承させない。
fn private(file: File) -> File {
    file.set_cloexec(true).unwrap();
    file
}

/// pipelineを起動して全childを待ち、最後のcommandの終了codeを返す。
/// `background`なら待たずに全childのpidを`jobs`へ記録し、起動できれば0を返す。
fn run(pipeline: &Pipeline<'_>, jobs: &mut Jobs, background: bool) -> i32 {
    if background && jobs.count + pipeline.count > MAX_JOBS {
        eprintln!("sh: too many background jobs (max {MAX_JOBS}); run wait");
        return 1;
    }
    // redirect先はspawn前に開き、失敗したら何も起動しない。
    let mut upstream = match pipeline.input.map(File::open).transpose() {
        Ok(file) => file.map(private),
        Err(Errno(errno)) => {
            let name = text(pipeline.input.unwrap_or_default());
            eprintln!("sh: {name}: cannot open (errno {errno})");
            return 1;
        }
    };
    let output = match pipeline.output.map(File::create).transpose() {
        Ok(file) => file.map(private),
        Err(Errno(errno)) => {
            let name = text(pipeline.output.unwrap_or_default());
            eprintln!("sh: {name}: cannot create (errno {errno})");
            return 1;
        }
    };

    // backgroundのcommandがconsoleの入力をshellと取り合わないよう、`< file`が
    // 無ければwrite端を閉じたpipeのread端をstdinにする。読めばすぐEOFになる。
    if background && upstream.is_none() {
        match pipe() {
            Ok((reader, writer)) => {
                drop(writer);
                upstream = Some(private(reader));
            }
            Err(Errno(errno)) => {
                eprintln!("sh: cannot create pipe (errno {errno})");
                return 1;
            }
        }
    }

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
                    dup2(private(writer).as_raw_fd(), STDOUT).unwrap();
                    downstream = Some(private(reader));
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
                // 前段のpipeのread端はshellだけが持つので、ここで閉じれば
                // 前段の書き手は満杯で止まらずに`EPIPE`を受け取る。
                drop(source);
                break;
            }
        }
    }

    if background {
        jobs.pids[jobs.count..jobs.count + spawned].copy_from_slice(&pids[..spawned]);
        jobs.count += spawned;
        if jobs.interactive && spawned > 0 {
            eprintln!("[{}]", pids[spawned - 1]);
        }
        return status;
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

/// 1行に並べたpipelineの区切り。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Connector {
    /// `;`：前の結果に関係なく次を実行する。
    Then,
    /// `&&`：直前の終了statusが0なら次を実行する。
    And,
    /// `||`：直前の終了statusが0以外なら次を実行する。
    Or,
    /// `&`：前のpipelineをbackgroundで起動し、待たずに次を実行する。
    Background,
}

/// 1行を`;`、`&`、`&&`、`||`で区切り、各pipelineの部分と、その後ろの区切りを
/// 順に返す。最後の部分の区切りは`None`である。引用符の中の`;`、`&`、`|`は
/// 区切らない。閉じていない引用符は行末まで1つの部分として返し、`parse`が
/// 構文errorにする。
fn split_list(
    line: &[u8],
) -> impl Iterator<Item = Result<(&[u8], Option<Connector>), &'static str>> {
    let mut rest = Some(line);
    core::iter::from_fn(move || {
        let current = rest?;
        let mut quote = None;
        let mut index = 0;
        while index < current.len() {
            let byte = current[index];
            match (quote, byte) {
                (Some(open), _) if byte == open => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(byte),
                (None, b';') => {
                    rest = Some(&current[index + 1..]);
                    return Some(Ok((&current[..index], Some(Connector::Then))));
                }
                (None, b'&') if current.get(index + 1) == Some(&b'&') => {
                    rest = Some(&current[index + 2..]);
                    return Some(Ok((&current[..index], Some(Connector::And))));
                }
                (None, b'&') => {
                    rest = Some(&current[index + 1..]);
                    return Some(Ok((&current[..index], Some(Connector::Background))));
                }
                (None, b'|') if current.get(index + 1) == Some(&b'|') => {
                    rest = Some(&current[index + 2..]);
                    return Some(Ok((&current[..index], Some(Connector::Or))));
                }
                (None, _) => {}
            }
            index += 1;
        }
        rest = None;
        Some(Ok((current, None)))
    })
}

/// `line`の全pipelineを実行前に検査する。`;`と`&`の後ろの空は許すが、
/// 区切りの前の空と、`&&`・`||`の後ろの空は構文errorである。
fn check_list(line: &[u8]) -> Result<(), &'static str> {
    let mut previous: Option<Connector> = None;
    for item in split_list(line) {
        let (segment, connector) = item?;
        let empty = parse(segment, b"0")?.is_none();
        match (empty, connector, previous) {
            // 空行と、末尾の`;`・`&`の後ろの空。
            (true, None, None | Some(Connector::Then | Connector::Background)) => {}
            (true, None, Some(_)) => return Err("missing command after '&&' or '||'"),
            (true, Some(_), _) => {
                return Err("missing command before ';', '&', '&&' or '||'");
            }
            (false, _, _) => {}
        }
        previous = connector;
    }
    Ok(())
}

/// 1行を実行し、新しい終了statusを返す。構文errorのある行は何も実行しない。
fn execute_line(line: &[u8], status: i32, jobs: &mut Jobs) -> i32 {
    if let Err(message) = check_list(line) {
        eprintln!("sh: {message}");
        return SYNTAX_STATUS;
    }
    let mut status = status;
    let mut run_next = true;
    for item in split_list(line) {
        let Ok((segment, connector)) = item else {
            unreachable!("check_list accepted the line");
        };
        if run_next {
            let background = connector == Some(Connector::Background);
            status = execute(segment, status, jobs, background);
        }
        run_next = match connector {
            None | Some(Connector::Then | Connector::Background) => true,
            Some(Connector::And) => status == 0,
            Some(Connector::Or) => status != 0,
        };
    }
    status
}

/// `status`を10進数で`buffer`へ書き、書いた部分を返す。
fn format_status(status: i32, buffer: &mut [u8; 11]) -> &[u8] {
    let mut digits = [0u8; 10];
    let mut len = 0;
    let mut value = status.unsigned_abs();
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let mut out = 0;
    if status < 0 {
        buffer[0] = b'-';
        out = 1;
    }
    for &digit in digits[..len].iter().rev() {
        buffer[out] = digit;
        out += 1;
    }
    &buffer[..out]
}

/// built-inとして扱うcommand名。backgroundでは起動できない。
const BUILTINS: [&[u8]; 6] = [b"exit", b"cd", b"pwd", b"true", b"false", b"wait"];

/// 1個のpipelineを実行し、新しい終了statusを返す。`background`なら待たない。
fn execute(line: &[u8], status: i32, jobs: &mut Jobs, background: bool) -> i32 {
    let mut digits = [0u8; 11];
    let last_status = format_status(status, &mut digits);
    let pipeline = match parse(line, last_status) {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return status,
        Err(message) => {
            eprintln!("sh: {message}");
            return SYNTAX_STATUS;
        }
    };
    let first = &pipeline.stages[0];
    if background && pipeline.count == 1 && BUILTINS.contains(&first.argv[0]) {
        eprintln!(
            "sh: {}: built-in commands cannot run in the background",
            text(first.argv[0])
        );
        return 1;
    }
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
    let simple = pipeline.count == 1 && pipeline.input.is_none() && pipeline.output.is_none();
    if simple && first.argv[0] == b"cd" {
        let target: &[u8] = match first.argv[1..first.argc] {
            [] => b"/",
            [dir] => dir,
            _ => {
                eprintln!("sh: cd: too many arguments");
                return SYNTAX_STATUS;
            }
        };
        return match fs::chdir(target) {
            Ok(()) => 0,
            Err(Errno(errno)) => {
                eprintln!(
                    "sh: cd: {}: cannot change directory (errno {errno})",
                    text(target)
                );
                1
            }
        };
    }
    if simple && first.argc == 1 && first.argv[0] == b"wait" {
        // 起動順に待つ。既に回収されたpidや、kernelの台帳から落ちた古い
        // 終了codeは`ECHILD`になるが、終わっていることに変わりはない。
        for &pid in &jobs.pids[..jobs.count] {
            let _ = wait(pid);
        }
        jobs.count = 0;
        return 0;
    }
    if simple && first.argc == 1 && first.argv[0] == b"true" {
        return 0;
    }
    if simple && first.argc == 1 && first.argv[0] == b"false" {
        return 1;
    }
    if simple && first.argv[0] == b"pwd" && first.argc == 1 {
        let mut buffer = [0u8; MAX_PATH_LEN + 1];
        return match fs::getcwd(&mut buffer) {
            Ok(cwd) => {
                println!("{}", text(cwd));
                0
            }
            Err(Errno(errno)) => {
                eprintln!("sh: pwd: errno {errno}");
                1
            }
        };
    }
    run(&pipeline, jobs, background)
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let prompt = args.skip(1).any(|arg| arg == b"-i");
    dup2(STDIN, SAVED_STDIN).unwrap();
    dup2(STDOUT, SAVED_STDOUT).unwrap();
    set_cloexec(SAVED_STDIN, true).unwrap();
    set_cloexec(SAVED_STDOUT, true).unwrap();

    let mut line = [0u8; LINE_MAX];
    let mut len = 0;
    let mut overflow = false;
    let mut chunk = [0u8; LINE_MAX];
    let mut status = 0;
    let mut jobs = Jobs {
        pids: [0; MAX_JOBS],
        count: 0,
        interactive: prompt,
    };
    loop {
        if prompt && len == 0 {
            print!("$ ");
        }
        let read = io::read(STDIN, &mut chunk).unwrap();
        if read == 0 {
            // 改行のない最終行も1行として実行する。
            if len > 0 && !overflow {
                execute_line(&line[..len], status, &mut jobs);
            }
            return 0;
        }
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if overflow {
                    eprintln!("sh: line too long (max {LINE_MAX} bytes)");
                } else {
                    status = execute_line(&line[..len], status, &mut jobs);
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
