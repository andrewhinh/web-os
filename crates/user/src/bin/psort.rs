#![no_std]
extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::cmp::min;

use ulib::{
    ExitCode, env, eprintln,
    fs::File,
    io::{Read, Write},
    stdio::stdout,
    sys, sysinfo, thread,
};

const RECORD_SIZE: usize = 100;

struct Record {
    data: [u8; RECORD_SIZE],
}

impl Record {
    fn key(&self) -> u32 {
        u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
    }
}

struct Task {
    records: *mut Record,
    start: usize,
    end: usize,
}

extern "C" fn worker(task_ptr: usize, _unused: usize) {
    let task = unsafe { Box::from_raw(task_ptr as *mut Task) };
    unsafe {
        sort_range(task.records, task.start, task.end);
    }
}

unsafe fn sort_range(ptr: *mut Record, start: usize, end: usize) {
    if end <= start {
        return;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(ptr.add(start), end - start) };
    slice.sort_unstable_by_key(|record| record.key());
}

fn parse_records(data: &[u8]) -> Vec<Record> {
    let count = data.len() / RECORD_SIZE;
    let mut records = Vec::with_capacity(count);
    for chunk in data.chunks_exact(RECORD_SIZE) {
        let mut rec = [0u8; RECORD_SIZE];
        rec.copy_from_slice(chunk);
        records.push(Record { data: rec });
    }
    records
}

fn parallel_sort(records: &mut [Record]) -> sys::Result<()> {
    let count = records.len();
    if count <= 1 {
        return Ok(());
    }

    let nprocs = sysinfo::get_nprocs_conf().max(1);
    let chunk_count = min(nprocs, count);
    let chunk_size = (count + chunk_count - 1) / chunk_count;
    let ptr = records.as_mut_ptr();

    let mut spawned = 0usize;
    for i in 0..chunk_count {
        let start = i * chunk_size;
        if start >= count {
            break;
        }
        let end = min(start + chunk_size, count);
        let task = Box::new(Task {
            records: ptr,
            start,
            end,
        });
        let task_ptr = Box::into_raw(task) as usize;
        match thread::thread_create(worker, task_ptr, 0) {
            Ok(_) => spawned += 1,
            Err(_) => unsafe {
                let task = Box::from_raw(task_ptr as *mut Task);
                sort_range(task.records, task.start, task.end);
            },
        }
    }

    for _ in 0..spawned {
        thread::thread_join()?;
    }

    Ok(())
}

struct Cursor {
    chunk: usize,
    index: usize,
    end: usize,
}

fn merge_write<W: Write>(
    records: &[Record],
    chunk_count: usize,
    chunk_size: usize,
    out: &mut W,
) -> sys::Result<()> {
    let mut cursors = Vec::with_capacity(chunk_count);
    for i in 0..chunk_count {
        let start = i * chunk_size;
        if start >= records.len() {
            break;
        }
        let end = min(start + chunk_size, records.len());
        cursors.push(Cursor {
            chunk: i,
            index: start,
            end,
        });
    }

    loop {
        let mut best_idx: Option<usize> = None;
        let mut best_key = 0u32;
        let mut best_chunk = 0usize;

        for (i, cursor) in cursors.iter().enumerate() {
            if cursor.index >= cursor.end {
                continue;
            }
            let key = records[cursor.index].key();
            let pick = match best_idx {
                None => true,
                Some(_) => key < best_key || (key == best_key && cursor.chunk < best_chunk),
            };
            if pick {
                best_idx = Some(i);
                best_key = key;
                best_chunk = cursor.chunk;
            }
        }

        let Some(choice) = best_idx else {
            break;
        };
        let record_index = cursors[choice].index;
        out.write_all(&records[record_index].data)?;
        cursors[choice].index += 1;
    }

    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let rest: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();
    if rest.len() != 1 {
        eprintln!("usage: psort <file>");
        return ExitCode::FAILURE;
    }

    let path = rest[0];
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => {
            eprintln!("error: cannot open file '{}'", path);
            return ExitCode::FAILURE;
        }
    };

    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        eprintln!("psort: read error");
        return ExitCode::FAILURE;
    }
    if data.len() % RECORD_SIZE != 0 {
        eprintln!("psort: file format error");
        return ExitCode::FAILURE;
    }

    let mut records = parse_records(&data);
    if parallel_sort(&mut records).is_err() {
        eprintln!("psort: thread error");
        return ExitCode::FAILURE;
    }

    let mut out = stdout();
    let chunk_count = min(sysinfo::get_nprocs_conf().max(1), records.len().max(1));
    let chunk_size = if records.is_empty() {
        0
    } else {
        (records.len() + chunk_count - 1) / chunk_count
    };
    if merge_write(&records, chunk_count, chunk_size, &mut out).is_err() {
        eprintln!("psort: write error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
