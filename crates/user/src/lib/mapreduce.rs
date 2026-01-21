#![allow(non_snake_case)]

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::cmp::min;
use core::mem;

use crate::mutex::Mutex;
use crate::thread;

pub type Mapper = fn(&str);
pub type Reducer = fn(&str, Getter, usize);
pub type Getter = fn(&str, usize) -> Option<String>;
pub type Partitioner = fn(&str, usize) -> usize;

struct KvPair {
    key: String,
    value: String,
}

struct PartitionMap {
    entries: Mutex<Vec<KvPair>>,
}

struct MrShared {
    partitions: Vec<PartitionMap>,
    partitioner: Partitioner,
    num_partitions: usize,
}

struct KeyRange {
    key: String,
    end: usize,
    next: usize,
}

struct PartitionReduce {
    data: Vec<KvPair>,
    ranges: Vec<KeyRange>,
}

static MR_SHARED: Mutex<Option<Arc<MrShared>>> = Mutex::new(None);
static MR_REDUCE: Mutex<Option<Arc<Vec<Mutex<PartitionReduce>>>>> = Mutex::new(None);

pub fn MR_Emit(key: &str, value: &str) {
    let shared = {
        let guard = MR_SHARED.lock();
        guard.as_ref().cloned()
    };
    let Some(shared) = shared else {
        return;
    };
    if shared.num_partitions == 0 {
        return;
    }
    let idx = (shared.partitioner)(key, shared.num_partitions) % shared.num_partitions;
    let mut entries = shared.partitions[idx].entries.lock();
    entries.push(KvPair {
        key: key.to_string(),
        value: value.to_string(),
    });
}

pub fn MR_DefaultHashPartition(key: &str, num_partitions: usize) -> usize {
    if num_partitions == 0 {
        return 0;
    }
    let mut hash: u64 = 5381;
    for byte in key.as_bytes() {
        hash = ((hash << 5).wrapping_add(hash)).wrapping_add(*byte as u64);
    }
    (hash % num_partitions as u64) as usize
}

pub fn MR_GetNext(key: &str, partition_number: usize) -> Option<String> {
    let parts = {
        let guard = MR_REDUCE.lock();
        guard.as_ref().cloned()
    };
    let Some(parts) = parts else {
        return None;
    };
    let part = parts.get(partition_number)?;
    let mut part = part.lock();
    let idx = match part
        .ranges
        .binary_search_by(|range| range.key.as_str().cmp(key))
    {
        Ok(idx) => idx,
        Err(_) => return None,
    };
    let next = {
        let range = &mut part.ranges[idx];
        if range.next >= range.end {
            return None;
        }
        let next = range.next;
        range.next += 1;
        next
    };
    let value = part.data[next].value.clone();
    Some(value)
}

struct MapTask {
    map: Mapper,
    files: Arc<Vec<String>>,
    next: Arc<Mutex<usize>>,
}

extern "C" fn map_worker(task_ptr: usize, _unused: usize) {
    let task = unsafe { Box::from_raw(task_ptr as *mut MapTask) };
    loop {
        let idx = {
            let mut guard = task.next.lock();
            if *guard >= task.files.len() {
                return;
            }
            let idx = *guard;
            *guard += 1;
            idx
        };
        let path = &task.files[idx];
        (task.map)(path);
    }
}

struct ReduceTask {
    reduce: Reducer,
    partition: usize,
    parts: Arc<Vec<Mutex<PartitionReduce>>>,
}

extern "C" fn reduce_worker(task_ptr: usize, _unused: usize) {
    let task = unsafe { Box::from_raw(task_ptr as *mut ReduceTask) };
    let keys: Vec<String> = {
        let part = task.parts[task.partition].lock();
        part.ranges.iter().map(|range| range.key.clone()).collect()
    };
    for key in keys {
        (task.reduce)(&key, MR_GetNext, task.partition);
    }
}

fn build_ranges(data: &[KvPair]) -> Vec<KeyRange> {
    let mut ranges = Vec::new();
    let mut idx = 0usize;
    while idx < data.len() {
        let start = idx;
        let key = data[idx].key.clone();
        idx += 1;
        while idx < data.len() && data[idx].key == key {
            idx += 1;
        }
        ranges.push(KeyRange {
            key,
            end: idx,
            next: start,
        });
    }
    ranges
}

pub fn MR_Run(
    files: &[&str],
    map: Mapper,
    num_mappers: usize,
    reduce: Reducer,
    num_reducers: usize,
    partitioner: Partitioner,
) {
    let num_partitions = num_reducers.max(1);
    let mut partitions = Vec::with_capacity(num_partitions);
    for _ in 0..num_partitions {
        partitions.push(PartitionMap {
            entries: Mutex::new(Vec::new()),
        });
    }
    let shared = Arc::new(MrShared {
        partitions,
        partitioner,
        num_partitions,
    });
    *MR_SHARED.lock() = Some(Arc::clone(&shared));

    let files = Arc::new(
        files
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
    );
    let next = Arc::new(Mutex::new(0usize));
    let mut map_threads = num_mappers.max(1);
    if files.is_empty() {
        map_threads = 0;
    } else {
        map_threads = min(map_threads, files.len());
    }

    let mut spawned = 0usize;
    for _ in 0..map_threads {
        let task = Box::new(MapTask {
            map,
            files: Arc::clone(&files),
            next: Arc::clone(&next),
        });
        let task_ptr = Box::into_raw(task) as usize;
        match thread::thread_create(map_worker, task_ptr, 0) {
            Ok(_) => spawned += 1,
            Err(_) => map_worker(task_ptr, 0),
        }
    }

    for _ in 0..spawned {
        let _ = thread::thread_join();
    }

    let mut reduce_parts = Vec::with_capacity(num_partitions);
    for part in &shared.partitions {
        let mut entries = part.entries.lock();
        let mut data = mem::take(&mut *entries);
        data.sort_by(|a, b| a.key.cmp(&b.key));
        let ranges = build_ranges(&data);
        reduce_parts.push(Mutex::new(PartitionReduce { data, ranges }));
    }
    let reduce_parts = Arc::new(reduce_parts);
    *MR_REDUCE.lock() = Some(Arc::clone(&reduce_parts));
    *MR_SHARED.lock() = None;

    let mut reduce_threads = num_reducers.max(1);
    reduce_threads = min(reduce_threads, num_partitions);

    let mut spawned = 0usize;
    for partition in 0..reduce_threads {
        let task = Box::new(ReduceTask {
            reduce,
            partition,
            parts: Arc::clone(&reduce_parts),
        });
        let task_ptr = Box::into_raw(task) as usize;
        match thread::thread_create(reduce_worker, task_ptr, 0) {
            Ok(_) => spawned += 1,
            Err(_) => reduce_worker(task_ptr, 0),
        }
    }

    for _ in 0..spawned {
        let _ = thread::thread_join();
    }
    *MR_REDUCE.lock() = None;
}
