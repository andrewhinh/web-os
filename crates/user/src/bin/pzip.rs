#![no_std]
extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::cmp::min;

use ulib::{
    ExitCode, env, eprintln,
    fs::File,
    io::{Read, Write},
    stdio::stdout,
    sys, sysinfo, thread,
};

#[derive(Clone, Copy)]
struct Run {
    count: u32,
    byte: u8,
}

struct Task {
    data: Arc<Vec<u8>>,
    start: usize,
    end: usize,
    results: *mut Option<Vec<Run>>,
    index: usize,
}

extern "C" fn worker(task_ptr: usize, _unused: usize) {
    let task = unsafe { Box::from_raw(task_ptr as *mut Task) };
    let runs = rle_encode(&task.data[task.start..task.end]);
    unsafe {
        *task.results.add(task.index) = Some(runs);
    }
}

fn usage() {
    eprintln!("usage: pzip <file1> [file2 ...]");
}

fn rle_encode(buf: &[u8]) -> Vec<Run> {
    let mut out = Vec::new();
    if buf.is_empty() {
        return out;
    }

    let mut current = buf[0];
    let mut count: u32 = 1;
    for &byte in &buf[1..] {
        if byte == current && count < u32::MAX {
            count += 1;
        } else {
            out.push(Run {
                count,
                byte: current,
            });
            current = byte;
            count = 1;
        }
    }
    out.push(Run {
        count,
        byte: current,
    });
    out
}

fn write_run<W: Write>(writer: &mut W, run: Run) -> sys::Result<()> {
    let mut record = [0u8; 5];
    record[..4].copy_from_slice(&run.count.to_le_bytes());
    record[4] = run.byte;
    writer.write_all(&record)
}

fn flush_prev<W: Write>(writer: &mut W, prev: &mut Option<Run>) -> sys::Result<()> {
    if let Some(run) = prev.take() {
        write_run(writer, run)?;
    }
    Ok(())
}

fn merge_run<W: Write>(writer: &mut W, prev: &mut Option<Run>, run: Run) -> sys::Result<()> {
    if let Some(prev_run) = prev.as_mut() {
        if prev_run.byte == run.byte {
            let mut total = prev_run.count as u64 + run.count as u64;
            while total > u32::MAX as u64 {
                write_run(
                    writer,
                    Run {
                        count: u32::MAX,
                        byte: prev_run.byte,
                    },
                )?;
                total -= u32::MAX as u64;
            }
            prev_run.count = total as u32;
            return Ok(());
        }
    }

    flush_prev(writer, prev)?;
    *prev = Some(run);
    Ok(())
}

fn compress_data<W: Write>(data: Vec<u8>, prev: &mut Option<Run>, out: &mut W) -> sys::Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let nprocs = sysinfo::get_nprocs_conf().max(1);
    let chunk_count = min(nprocs, data.len());
    let chunk_size = (data.len() + chunk_count - 1) / chunk_count;
    let shared = Arc::new(data);

    let mut results: Vec<Option<Vec<Run>>> = vec![None; chunk_count];
    let results_ptr = results.as_mut_ptr();

    let mut spawned = 0usize;
    for i in 0..chunk_count {
        let start = i * chunk_size;
        if start >= shared.len() {
            break;
        }
        let end = min(start + chunk_size, shared.len());
        let task = Box::new(Task {
            data: Arc::clone(&shared),
            start,
            end,
            results: results_ptr,
            index: i,
        });
        let task_ptr = Box::into_raw(task) as usize;
        match thread::thread_create(worker, task_ptr, 0) {
            Ok(_) => spawned += 1,
            Err(_) => unsafe {
                let task = Box::from_raw(task_ptr as *mut Task);
                let runs = rle_encode(&task.data[task.start..task.end]);
                *task.results.add(task.index) = Some(runs);
            },
        }
    }

    for _ in 0..spawned {
        thread::thread_join()?;
    }

    for i in 0..chunk_count {
        if let Some(runs) = results[i].take() {
            for run in runs {
                merge_run(out, prev, run)?;
            }
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let files: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();

    if files.is_empty() {
        usage();
        return ExitCode::FAILURE;
    }

    let mut out = stdout();
    let mut prev: Option<Run> = None;

    for path in files {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                eprintln!("error: cannot open file '{}'", path);
                return ExitCode::FAILURE;
            }
        };
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_err() {
            eprintln!("pzip: read error");
            return ExitCode::FAILURE;
        }
        if compress_data(data, &mut prev, &mut out).is_err() {
            eprintln!("pzip: write error");
            return ExitCode::FAILURE;
        }
    }

    if flush_prev(&mut out, &mut prev).is_err() {
        eprintln!("pzip: write error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
