#![no_std]
extern crate alloc;
use alloc::{string::String, vec::Vec};

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

fn wait_foreground(job: &mut Job) -> sys::Result<bool> {
    while !job.pids.is_empty() {
        let pid = job.pids[0];
        let mut status = 0;
        match sys::waitpid(pid as isize, &mut status, signal::WUNTRACED) {
            Ok(_) => {
                if status_stopped(status).is_some() {
                    job.stopped = true;
                    return Ok(true);
                }
                job.pids.remove(0);
            }
            Err(sys::Error::Interrupted) => {
                let _ = sys::sleep(1);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(false)
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
        stdin().read_line(&mut input).unwrap();

        let mut line = input.trim();
        if line.is_empty() {
            continue;
        }
        let mut background = false;
        if line.ends_with('&') {
            background = true;
            line = line.trim_end_matches('&').trim();
        }
        if line.is_empty() {
            continue;
        }

        let job_cmd = String::from(line);
        let mut commands = line.split('|').enumerate().peekable();
        let mut prev_stdout: Option<Stdio> = None;
        let mut pgid: Option<usize> = None;
        let mut pids: Vec<usize> = Vec::new();

        while let Some((num, command)) = commands.next() {
            let mut parts = command.split_whitespace();
            let Some(command) = parts.next() else {
                continue 'main;
            };
            let mut args = parts.peekable();

            match command {
                "cd" => {
                    if num == 0 {
                        // chdir must be called by the parent. if in child do nothing any more.
                        let new_dir = args.peek().map_or("/", |x| *x);
                        if let Err(e) = env::set_current_dir(new_dir) {
                            eprintln!("{}", e);
                        }
                    }
                    continue 'main;
                }
                "export" => {
                    if args.peek().map_or(true, |x| *x == "-p") {
                        for (key, value) in env::vars() {
                            println!("{}: {}", key, value);
                        }
                    } else {
                        for arg in args {
                            if let Some((key, value)) = arg.split_once('=') {
                                if let Err(e) = env::set_var(key, value) {
                                    eprintln!("{}", e);
                                }
                            } else {
                                eprintln!("export: invalid argument: {}", arg);
                            }
                        }
                    }
                    continue 'main;
                }
                "jobs" => {
                    if num == 0 && commands.peek().is_none() {
                        for job in &jobs {
                            let state = if job.stopped { "stopped" } else { "running" };
                            println!("[{}] {} {}", job.id, state, job.cmd);
                        }
                    }
                    continue 'main;
                }
                "fg" => {
                    if num == 0 && commands.peek().is_none() {
                        let job_id = parse_job_id(args.next(), &jobs);
                        let Some(job_id) = job_id else {
                            eprintln!("fg: no job");
                            continue 'main;
                        };
                        let Some(pos) = jobs.iter().position(|job| job.id == job_id) else {
                            eprintln!("fg: no such job");
                            continue 'main;
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
                            Ok(true) => {
                                println!("[{}] stopped {}", job.id, job.cmd);
                                jobs.push(job);
                            }
                            Ok(false) => {}
                            Err(e) => eprintln!("{}", e),
                        }
                    }
                    continue 'main;
                }
                "bg" => {
                    if num == 0 && commands.peek().is_none() {
                        let job_id = parse_job_id(args.next(), &jobs);
                        let Some(job_id) = job_id else {
                            eprintln!("bg: no job");
                            continue 'main;
                        };
                        let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else {
                            eprintln!("bg: no such job");
                            continue 'main;
                        };
                        if job.stopped {
                            for pid in &job.pids {
                                let _ = sys::kill(*pid, signal::SIGCONT);
                            }
                            job.stopped = false;
                        }
                        println!("[{}] running {}", job.id, job.cmd);
                    }
                    continue 'main;
                }
                "exit" => return,
                command => {
                    let stdin = prev_stdout.take().unwrap_or(Stdio::Inherit);
                    let mut stdout = if commands.peek().is_some() {
                        Stdio::MakePipe
                    } else {
                        Stdio::Inherit
                    };

                    let rawstring;
                    let mut file_name = "";
                    let mut overwrite = true;
                    let mut append = false;
                    let mut arg_vec = Vec::new();
                    while let Some(arg) = args.next_if(|s| !s.contains('>')) {
                        arg_vec.push(arg);
                    }
                    if let Some(redir) = args.peek() {
                        if redir.contains(">>") {
                            overwrite = false;
                            append = true;
                        }
                        rawstring = args.collect::<Vec<&str>>().concat();
                        let split = rawstring.split('>');
                        for (i, e) in split.enumerate() {
                            if e.is_empty() {
                                continue;
                            }
                            if i == 0 {
                                arg_vec.push(e);
                            } else {
                                file_name = e;
                            }
                        }
                        assert!(!file_name.is_empty(), "redirect");
                        stdout = Stdio::Fd(
                            OpenOptions::new()
                                .create(true)
                                .write(true)
                                .truncate(overwrite)
                                .append(append)
                                .open(file_name)
                                .unwrap(),
                        );
                    }

                    let target_pgid = pgid.unwrap_or(0);
                    match Command::new(command)
                        .pgrp(target_pgid)
                        .args(arg_vec)
                        .stdin(stdin)
                        .stdout(stdout)
                        .spawn()
                    {
                        Ok(mut child) => {
                            let pid = child.pid();
                            let job_pgid = pgid.unwrap_or(pid);
                            let _ = sys::setpgid(pid, job_pgid);
                            pgid = Some(job_pgid);
                            if commands.peek().is_some() {
                                prev_stdout = child.stdout.take().map(Stdio::from);
                            }
                            pids.push(pid);
                        }
                        Err(e) => {
                            prev_stdout = None;
                            pids.clear();
                            eprintln!("{}", e);
                        }
                    }
                }
            }
        }

        if pids.is_empty() {
            continue;
        }
        let pgid = pgid.unwrap_or(0);
        if background {
            let job_id = next_job_id;
            next_job_id += 1;
            jobs.push(Job {
                id: job_id,
                pgid,
                pids,
                cmd: job_cmd,
                stopped: false,
            });
            println!("[{}] {}", job_id, pgid);
        } else {
            let mut job = Job {
                id: 0,
                pgid,
                pids,
                cmd: job_cmd,
                stopped: false,
            };
            let _ = sys::tcsetpgrp(0, pgid);
            let res = wait_foreground(&mut job);
            let _ = sys::tcsetpgrp(0, shell_pgid);
            match res {
                Ok(true) => {
                    let job_id = next_job_id;
                    next_job_id += 1;
                    job.id = job_id;
                    println!("[{}] stopped {}", job.id, job.cmd);
                    jobs.push(job);
                }
                Ok(false) => {}
                Err(e) => eprintln!("{}", e),
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
