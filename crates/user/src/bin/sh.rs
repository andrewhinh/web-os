#![no_std]
extern crate alloc;
use alloc::{format, string::String, vec::Vec};

use ulib::{
    env, eprintln,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader},
    path::Path,
    print, println,
    process::{Command, Stdio},
    signal,
    stdio::stdin,
    sys,
};

#[derive(Debug)]
struct Job {
    id: usize,
    pgid: usize,
    pids: Vec<usize>,
    last_pid: usize,
    cmd: String,
    stopped: bool,
}

fn init_job_control() -> usize {
    let _ = sys::setsid();
    let _ = sys::setpgid(0, 0);
    let shell_pgid = sys::getpgrp().unwrap_or_else(|_| sys::getpid().unwrap_or(0));
    let _ = sys::tcsetpgrp(0, shell_pgid);
    shell_pgid
}

fn status_stopped(status: i32) -> Option<usize> {
    if (status & 0xff) == 0x7f {
        Some(((status >> 8) & 0xff) as usize)
    } else {
        None
    }
}

fn exit_status_code(status: i32) -> i32 {
    if status_stopped(status).is_some() {
        return 1;
    }
    if (status & 0xff) == 0 {
        return (status >> 8) & 0xff;
    }
    1
}

fn parse_job_id(arg: Option<&str>, jobs: &[Job]) -> Option<usize> {
    match arg {
        Some(raw) => raw.trim_start_matches('%').parse().ok(),
        None => jobs.last().map(|job| job.id),
    }
}

fn reap_jobs(jobs: &mut Vec<Job>) {
    let mut i = 0;
    while i < jobs.len() {
        let mut stopped = false;
        let mut idx = 0;
        while idx < jobs[i].pids.len() {
            let pid = jobs[i].pids[idx];
            let mut status = 0;
            match sys::waitpid(
                pid as isize,
                &mut status,
                signal::WNOHANG | signal::WUNTRACED,
            ) {
                Ok(0) => {
                    idx += 1;
                }
                Ok(_) => {
                    if status_stopped(status).is_some() {
                        stopped = true;
                        idx += 1;
                    } else {
                        jobs[i].pids.remove(idx);
                    }
                }
                Err(sys::Error::Interrupted) => {
                    idx += 1;
                }
                Err(_) => {
                    idx += 1;
                }
            }
        }
        if stopped {
            jobs[i].stopped = true;
            println!("[{}] stopped {}", jobs[i].id, jobs[i].cmd);
        }
        if jobs[i].pids.is_empty() {
            println!("[{}] done {}", jobs[i].id, jobs[i].cmd);
            jobs.remove(i);
        } else {
            i += 1;
        }
    }
}

struct WaitResult {
    stopped: bool,
    status: i32,
}

fn wait_foreground(job: &mut Job) -> sys::Result<WaitResult> {
    let mut last_status = 1;
    while !job.pids.is_empty() {
        let pid = job.pids[0];
        let mut status = 0;
        match sys::waitpid(pid as isize, &mut status, signal::WUNTRACED) {
            Ok(_) => {
                if status_stopped(status).is_some() {
                    job.stopped = true;
                    return Ok(WaitResult {
                        stopped: true,
                        status: 1,
                    });
                }
                if pid == job.last_pid {
                    last_status = exit_status_code(status);
                }
                job.pids.remove(0);
            }
            Err(sys::Error::Interrupted) => {
                let _ = sys::sleep(1);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(WaitResult {
        stopped: false,
        status: last_status,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Op(Op),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Semi,
    Pipe,
    AndAnd,
    OrOr,
    Amp,
    RedirOut,
    RedirOutAppend,
    RedirIn,
}

#[derive(Debug, Clone)]
struct OutputRedir {
    path: String,
    append: bool,
}

#[derive(Debug, Clone, Default)]
struct Redir {
    input: Option<String>,
    output: Option<OutputRedir>,
}

#[derive(Debug, Clone)]
struct SimpleCmd {
    argv: Vec<String>,
    redir: Redir,
}

#[derive(Debug, Clone)]
struct Pipeline {
    cmds: Vec<SimpleCmd>,
    background: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondOp {
    And,
    Or,
}

#[derive(Debug, Clone)]
struct CondItem {
    op: Option<CondOp>,
    pipeline: Pipeline,
}

#[derive(Debug, Clone)]
struct ForLoop {
    var: String,
    items: Vec<String>,
    body: String,
}

#[derive(Debug)]
enum BuiltinResult {
    Status(i32),
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Single,
    Double,
}

fn apply_escape(next: char) -> char {
    match next {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '\\' => '\\',
        '"' => '"',
        '\'' => '\'',
        _ => next,
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut chars = input.chars().peekable();
    let mut mode = Mode::Normal;

    while let Some(ch) = chars.next() {
        match mode {
            Mode::Normal => match ch {
                c if c.is_whitespace() => {
                    if !buf.is_empty() {
                        tokens.push(Token::Word(core::mem::take(&mut buf)));
                    }
                }
                '\'' => {
                    mode = Mode::Single;
                }
                '"' => {
                    mode = Mode::Double;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        buf.push(apply_escape(next));
                    } else {
                        buf.push('\\');
                    }
                }
                ';' | '|' | '&' | '>' | '<' => {
                    if !buf.is_empty() {
                        tokens.push(Token::Word(core::mem::take(&mut buf)));
                    }
                    match ch {
                        ';' => tokens.push(Token::Op(Op::Semi)),
                        '|' => {
                            if matches!(chars.peek(), Some('|')) {
                                chars.next();
                                tokens.push(Token::Op(Op::OrOr));
                            } else {
                                tokens.push(Token::Op(Op::Pipe));
                            }
                        }
                        '&' => {
                            if matches!(chars.peek(), Some('&')) {
                                chars.next();
                                tokens.push(Token::Op(Op::AndAnd));
                            } else {
                                tokens.push(Token::Op(Op::Amp));
                            }
                        }
                        '>' => {
                            if matches!(chars.peek(), Some('>')) {
                                chars.next();
                                tokens.push(Token::Op(Op::RedirOutAppend));
                            } else {
                                tokens.push(Token::Op(Op::RedirOut));
                            }
                        }
                        '<' => tokens.push(Token::Op(Op::RedirIn)),
                        _ => {}
                    }
                }
                _ => buf.push(ch),
            },
            Mode::Single => {
                if ch == '\'' {
                    mode = Mode::Normal;
                } else {
                    buf.push(ch);
                }
            }
            Mode::Double => match ch {
                '"' => mode = Mode::Normal,
                '\\' => {
                    if let Some(next) = chars.next() {
                        buf.push(apply_escape(next));
                    } else {
                        buf.push('\\');
                    }
                }
                _ => buf.push(ch),
            },
        }
    }

    match mode {
        Mode::Normal => {}
        Mode::Single => return Err("unterminated single quote".into()),
        Mode::Double => return Err("unterminated double quote".into()),
    }

    if !buf.is_empty() {
        tokens.push(Token::Word(buf));
    }
    Ok(tokens)
}

fn parse_simple(tokens: &[Token], idx: &mut usize) -> Result<SimpleCmd, String> {
    let mut argv = Vec::new();
    let mut redir = Redir::default();

    while *idx < tokens.len() {
        match &tokens[*idx] {
            Token::Word(word) => {
                argv.push(word.clone());
                *idx += 1;
            }
            Token::Op(op) => match op {
                Op::RedirIn => {
                    *idx += 1;
                    let Some(Token::Word(path)) = tokens.get(*idx) else {
                        return Err("expected input file after '<'".into());
                    };
                    *idx += 1;
                    if redir.input.is_some() {
                        return Err("multiple input redirections".into());
                    }
                    redir.input = Some(path.clone());
                }
                Op::RedirOut | Op::RedirOutAppend => {
                    let append = *op == Op::RedirOutAppend;
                    *idx += 1;
                    let Some(Token::Word(path)) = tokens.get(*idx) else {
                        return Err("expected output file after '>'".into());
                    };
                    *idx += 1;
                    if redir.output.is_some() {
                        return Err("multiple output redirections".into());
                    }
                    redir.output = Some(OutputRedir {
                        path: path.clone(),
                        append,
                    });
                }
                _ => break,
            },
        }
    }

    if argv.is_empty() {
        return Err("empty command".into());
    }

    Ok(SimpleCmd { argv, redir })
}

fn parse_pipeline(tokens: &[Token], idx: &mut usize) -> Result<Pipeline, String> {
    let mut cmds = Vec::new();

    loop {
        let cmd = parse_simple(tokens, idx)?;
        cmds.push(cmd);
        if *idx >= tokens.len() {
            break;
        }
        match tokens[*idx] {
            Token::Op(Op::Pipe) => {
                *idx += 1;
                continue;
            }
            _ => break,
        }
    }

    Ok(Pipeline {
        cmds,
        background: false,
    })
}

fn parse_line(tokens: &[Token]) -> Result<Vec<Vec<CondItem>>, String> {
    let mut idx = 0;
    let mut sequences: Vec<Vec<CondItem>> = Vec::new();
    let mut chain: Vec<CondItem> = Vec::new();
    let mut pending_op: Option<CondOp> = None;

    while idx < tokens.len() {
        let mut pipeline = parse_pipeline(tokens, &mut idx)?;
        let mut background = false;
        if matches!(tokens.get(idx), Some(Token::Op(Op::Amp))) {
            idx += 1;
            background = true;
        }
        pipeline.background = background;

        chain.push(CondItem {
            op: pending_op.take(),
            pipeline,
        });

        if background {
            sequences.push(chain);
            chain = Vec::new();
            pending_op = None;
            continue;
        }

        if idx >= tokens.len() {
            break;
        }
        match tokens[idx] {
            Token::Op(Op::Semi) => {
                idx += 1;
                sequences.push(chain);
                chain = Vec::new();
                pending_op = None;
            }
            Token::Op(Op::AndAnd) => {
                idx += 1;
                pending_op = Some(CondOp::And);
            }
            Token::Op(Op::OrOr) => {
                idx += 1;
                pending_op = Some(CondOp::Or);
            }
            Token::Op(_) => return Err("unexpected operator".into()),
            Token::Word(_) => return Err("missing operator between commands".into()),
        }
    }

    if !chain.is_empty() {
        sequences.push(chain);
    }

    Ok(sequences)
}

fn pipeline_display(pipeline: &Pipeline) -> String {
    let mut out = String::new();
    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        if i > 0 {
            out.push_str(" | ");
        }
        for (j, arg) in cmd.argv.iter().enumerate() {
            if j > 0 {
                out.push(' ');
            }
            out.push_str(arg);
        }
    }
    if pipeline.background {
        out.push_str(" &");
    }
    out
}

fn parse_for_loop(line: &str) -> Result<Option<ForLoop>, String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("for ") {
        return Ok(None);
    }

    let do_marker = "; do ";
    let done_marker = "; done";
    let Some(do_pos) = trimmed.find(do_marker) else {
        return Err("for: missing '; do'".into());
    };
    let Some(done_pos) = trimmed.rfind(done_marker) else {
        return Err("for: missing '; done'".into());
    };
    if done_pos < do_pos {
        return Err("for: bad order".into());
    }

    let header = &trimmed[4..do_pos];
    let body = trimmed[do_pos + do_marker.len()..done_pos].trim();
    if body.is_empty() {
        return Err("for: empty body".into());
    }

    let mut parts = header.split_whitespace();
    let var = parts
        .next()
        .ok_or_else(|| String::from("for: missing var"))?;
    let in_kw = parts
        .next()
        .ok_or_else(|| String::from("for: missing in"))?;
    if in_kw != "in" {
        return Err("for: expected 'in'".into());
    }
    let mut items = Vec::new();
    for part in parts {
        items.push(String::from(part));
    }
    if items.is_empty() {
        return Err("for: missing items".into());
    }

    Ok(Some(ForLoop {
        var: String::from(var),
        items,
        body: String::from(body),
    }))
}

fn run_builtin(cmd: &SimpleCmd, jobs: &mut Vec<Job>, shell_pgid: usize) -> Option<BuiltinResult> {
    let command = cmd.argv.first()?.as_str();
    match command {
        "true" => Some(BuiltinResult::Status(0)),
        "false" => Some(BuiltinResult::Status(1)),
        "cd" => {
            let new_dir = cmd.argv.get(1).map(|s| s.as_str()).unwrap_or("/");
            let status = match env::set_current_dir(new_dir) {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("{}", e);
                    1
                }
            };
            Some(BuiltinResult::Status(status))
        }
        "export" => {
            if cmd.argv.get(1).map_or(true, |x| x.as_str() == "-p") {
                for (key, value) in env::vars() {
                    println!("{}: {}", key, value);
                }
                return Some(BuiltinResult::Status(0));
            }
            let mut status = 0;
            for arg in cmd.argv.iter().skip(1) {
                if let Some((key, value)) = arg.split_once('=') {
                    if let Err(e) = env::set_var(key, value) {
                        eprintln!("{}", e);
                        status = 1;
                    }
                } else {
                    eprintln!("export: invalid argument: {}", arg);
                    status = 1;
                }
            }
            Some(BuiltinResult::Status(status))
        }
        "jobs" => {
            for job in jobs.iter() {
                let state = if job.stopped { "stopped" } else { "running" };
                println!("[{}] {} {}", job.id, state, job.cmd);
            }
            Some(BuiltinResult::Status(0))
        }
        "fg" => {
            let job_id = parse_job_id(cmd.argv.get(1).map(|s| s.as_str()), jobs);
            let Some(job_id) = job_id else {
                eprintln!("fg: no job");
                return Some(BuiltinResult::Status(1));
            };
            let Some(pos) = jobs.iter().position(|job| job.id == job_id) else {
                eprintln!("fg: no such job");
                return Some(BuiltinResult::Status(1));
            };
            let mut job = jobs.remove(pos);
            if job.stopped {
                for pid in &job.pids {
                    let _ = sys::kill(*pid, signal::SIGCONT);
                }
                job.stopped = false;
            }
            let _ = sys::tcsetpgrp(0, job.pgid);
            let res = wait_foreground(&mut job);
            let _ = sys::tcsetpgrp(0, shell_pgid);
            match res {
                Ok(WaitResult { stopped: true, .. }) => {
                    println!("[{}] stopped {}", job.id, job.cmd);
                    jobs.push(job);
                    Some(BuiltinResult::Status(1))
                }
                Ok(WaitResult { status, .. }) => Some(BuiltinResult::Status(status)),
                Err(e) => {
                    eprintln!("{}", e);
                    Some(BuiltinResult::Status(1))
                }
            }
        }
        "bg" => {
            let job_id = parse_job_id(cmd.argv.get(1).map(|s| s.as_str()), jobs);
            let Some(job_id) = job_id else {
                eprintln!("bg: no job");
                return Some(BuiltinResult::Status(1));
            };
            let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else {
                eprintln!("bg: no such job");
                return Some(BuiltinResult::Status(1));
            };
            if job.stopped {
                for pid in &job.pids {
                    let _ = sys::kill(*pid, signal::SIGCONT);
                }
                job.stopped = false;
            }
            println!("[{}] running {}", job.id, job.cmd);
            Some(BuiltinResult::Status(0))
        }
        "exit" => Some(BuiltinResult::Exit),
        _ => None,
    }
}

fn run_pipeline(
    pipeline: &Pipeline,
    jobs: &mut Vec<Job>,
    next_job_id: &mut usize,
    shell_pgid: usize,
) -> sys::Result<i32> {
    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        if cmd.redir.input.is_some() && i != 0 {
            eprintln!("redirect: input only allowed on first command");
            return Ok(1);
        }
        if cmd.redir.output.is_some() && i + 1 < pipeline.cmds.len() {
            eprintln!("redirect: output only allowed on last command");
            return Ok(1);
        }
    }

    let job_cmd = pipeline_display(pipeline);
    let mut prev_stdout: Option<Stdio> = None;
    let mut pgid: Option<usize> = None;
    let mut pids: Vec<usize> = Vec::new();
    let mut last_pid = 0usize;

    for (i, cmd) in pipeline.cmds.iter().enumerate() {
        let mut argv_refs = Vec::with_capacity(cmd.argv.len());
        for s in &cmd.argv {
            argv_refs.push(s.as_str());
        }
        let Some(program) = argv_refs.get(0).copied() else {
            return Ok(1);
        };
        let args = &argv_refs[1..];

        let stdin = if i == 0 {
            if let Some(path) = cmd.redir.input.as_deref() {
                Stdio::Fd(OpenOptions::new().read(true).open(path)?)
            } else {
                prev_stdout.take().unwrap_or(Stdio::Inherit)
            }
        } else {
            prev_stdout.take().unwrap_or(Stdio::Inherit)
        };

        let mut stdout = if i + 1 < pipeline.cmds.len() {
            Stdio::MakePipe
        } else {
            Stdio::Inherit
        };

        if i + 1 == pipeline.cmds.len() {
            if let Some(out) = cmd.redir.output.as_ref() {
                stdout = Stdio::Fd(
                    OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(!out.append)
                        .append(out.append)
                        .open(out.path.as_str())?,
                );
            }
        }

        let is_first = pgid.is_none();
        let target_pgid = pgid.unwrap_or(0);
        match Command::new(program)
            .pgrp(target_pgid)
            .foreground(is_first && !pipeline.background)
            .args(args)
            .stdin(stdin)
            .stdout(stdout)
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.pid();
                let job_pgid = pgid.unwrap_or(pid);
                let _ = sys::setpgid(pid, job_pgid);
                pgid = Some(job_pgid);
                if is_first && !pipeline.background {
                    let _ = sys::tcsetpgrp(0, job_pgid);
                }
                if i + 1 < pipeline.cmds.len() {
                    prev_stdout = child.stdout.take().map(Stdio::from);
                }
                pids.push(pid);
                if i + 1 == pipeline.cmds.len() {
                    last_pid = pid;
                }
            }
            Err(e) => {
                pids.clear();
                eprintln!("{}", e);
                return Ok(1);
            }
        }
    }

    if pids.is_empty() {
        return Ok(1);
    }

    let pgid = pgid.unwrap_or(0);
    if pipeline.background {
        let job_id = *next_job_id;
        *next_job_id += 1;
        jobs.push(Job {
            id: job_id,
            pgid,
            pids,
            last_pid,
            cmd: job_cmd,
            stopped: false,
        });
        println!("[{}] {}", job_id, pgid);
        Ok(0)
    } else {
        let mut job = Job {
            id: 0,
            pgid,
            pids,
            last_pid,
            cmd: job_cmd,
            stopped: false,
        };
        let _ = sys::tcsetpgrp(0, pgid);
        let res = wait_foreground(&mut job);
        let _ = sys::tcsetpgrp(0, shell_pgid);
        match res {
            Ok(WaitResult { stopped: true, .. }) => {
                let job_id = *next_job_id;
                *next_job_id += 1;
                job.id = job_id;
                println!("[{}] stopped {}", job.id, job.cmd);
                jobs.push(job);
                Ok(1)
            }
            Ok(WaitResult { status, .. }) => Ok(status),
            Err(e) => {
                eprintln!("{}", e);
                Ok(1)
            }
        }
    }
}

fn execute_line(
    line: &str,
    jobs: &mut Vec<Job>,
    next_job_id: &mut usize,
    shell_pgid: usize,
) -> bool {
    let tokens = match tokenize(line) {
        Ok(tokens) => tokens,
        Err(e) => {
            eprintln!("parse: {}", e);
            return false;
        }
    };
    let sequences = match parse_line(&tokens) {
        Ok(sequences) => sequences,
        Err(e) => {
            eprintln!("parse: {}", e);
            return false;
        }
    };

    for chain in sequences {
        let mut last_status = 0;
        for item in chain {
            if let Some(op) = item.op {
                match op {
                    CondOp::And if last_status != 0 => continue,
                    CondOp::Or if last_status == 0 => continue,
                    _ => {}
                }
            }

            if item.pipeline.cmds.len() == 1 && !item.pipeline.background {
                if let Some(res) = run_builtin(&item.pipeline.cmds[0], jobs, shell_pgid) {
                    match res {
                        BuiltinResult::Exit => return true,
                        BuiltinResult::Status(status) => {
                            last_status = status;
                            continue;
                        }
                    }
                }
            } else if let Some(cmd) = item.pipeline.cmds.first() {
                if run_builtin(cmd, jobs, shell_pgid).is_some() {
                    eprintln!("builtin in pipeline or background not supported");
                    last_status = 1;
                    continue;
                }
            }

            match run_pipeline(&item.pipeline, jobs, next_job_id, shell_pgid) {
                Ok(status) => last_status = status,
                Err(e) => {
                    eprintln!("{}", e);
                    last_status = 1;
                }
            }
        }
    }

    false
}

fn main() {
    // Ensure that three file descriptors are open
    while let Ok(fd) = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/console")
    {
        if fd.get_fd() > 2 {
            drop(fd);
            break;
        }
    }
    set_path_from_etc_paths().unwrap();
    let shell_pgid = init_job_control();
    let _ = signal::signal(signal::SIGINT, signal::SIG_IGN);
    let _ = signal::signal(signal::SIGTSTP, signal::SIG_IGN);
    let _ = signal::signal(signal::SIGTTIN, signal::SIG_IGN);
    let _ = signal::signal(signal::SIGTTOU, signal::SIG_IGN);
    let mut jobs: Vec<Job> = Vec::new();
    let mut next_job_id = 1usize;

    // read and run input commands.
    'main: loop {
        reap_jobs(&mut jobs);
        print!("$ ");

        let mut input = String::new();
        loop {
            match stdin().read_line(&mut input) {
                Ok(0) => return,
                Ok(_) => break,
                Err(sys::Error::Interrupted) => continue,
                Err(e) => {
                    eprintln!("{}", e);
                    continue 'main;
                }
            }
        }

        let input = normalize_input(&input);
        let line = input.trim();
        if line.is_empty() {
            continue;
        }

        match parse_for_loop(line) {
            Ok(Some(loop_spec)) => {
                let prev = env::var(&loop_spec.var).ok().map(String::from);
                for item in loop_spec.items {
                    let _ = env::set_var(&loop_spec.var, &item);
                    let mut body = loop_spec.body.clone();
                    let pat_braced = format!("${{{}}}", loop_spec.var);
                    let pat_plain = format!("${}", loop_spec.var);
                    body = body.replace(&pat_braced, &item);
                    body = body.replace(&pat_plain, &item);
                    if execute_line(&body, &mut jobs, &mut next_job_id, shell_pgid) {
                        return;
                    }
                }
                match prev {
                    Some(value) => {
                        let _ = env::set_var(&loop_spec.var, &value);
                    }
                    None => {
                        let _ = env::remove_var(&loop_spec.var);
                    }
                }
            }
            Ok(None) => {
                if execute_line(line, &mut jobs, &mut next_job_id, shell_pgid) {
                    return;
                }
            }
            Err(e) => {
                eprintln!("parse: {}", e);
                continue 'main;
            }
        }
    }
}

fn set_path_from_etc_paths() -> sys::Result<()> {
    let path_file = "/etc/paths";
    if Path::new(path_file).exists() {
        let file = BufReader::new(File::open(path_file)?);
        let mut paths: Vec<String> = env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        for line in file.lines() {
            if let Ok(path) = line {
                paths.push(path);
            }
        }
        let new_path = paths.join(":");
        let _ = env::set_var("PATH", &new_path);
    }
    Ok(())
}

fn normalize_input(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '\x08' | '\x7f' => {
                out.pop();
            }
            _ if ch.is_ascii_control() => {}
            _ => out.push(ch),
        }
    }
    out
}
