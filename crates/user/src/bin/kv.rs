#![no_std]
extern crate alloc;

use alloc::{
    collections::btree_map::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use ulib::{
    ExitCode, env, eprintln,
    fs::File,
    io::{Read, Write},
    println, sys,
};

const DB_PATH: &str = "database.txt";

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let commands: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();

    if commands.is_empty() {
        return ExitCode::SUCCESS;
    }

    let mut store = match load_db(DB_PATH) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("kv: load error: {}", err);
            return ExitCode::FAILURE;
        }
    };

    let mut dirty = false;
    for command in commands {
        apply_command(&mut store, command, &mut dirty);
    }

    if dirty {
        if let Err(err) = save_db(DB_PATH, &store) {
            eprintln!("kv: save error: {}", err);
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}

fn load_db(path: &str) -> sys::Result<BTreeMap<i64, String>> {
    let mut map = BTreeMap::new();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(sys::Error::NotFound) => return Ok(map),
        Err(err) => return Err(err),
    };

    let mut data = String::new();
    file.read_to_string(&mut data)?;
    for raw_line in data.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (key_str, value) = line.split_once(',').ok_or(sys::Error::InvalidArgument)?;
        let key = key_str
            .trim()
            .parse::<i64>()
            .map_err(|_| sys::Error::InvalidArgument)?;
        map.insert(key, value.to_string());
    }
    Ok(map)
}

fn save_db(path: &str, store: &BTreeMap<i64, String>) -> sys::Result<()> {
    let mut file = File::create(path)?;
    for (key, value) in store.iter() {
        file.write_fmt(format_args!("{},{}\n", key, value))?;
    }
    Ok(())
}

fn apply_command(store: &mut BTreeMap<i64, String>, command: &str, dirty: &mut bool) {
    let mut parts = command.split(',');
    let op = parts.next().unwrap_or("");
    match op {
        "p" => {
            let Some(key_str) = parts.next() else {
                bad_command();
                return;
            };
            let Some(value) = parts.next() else {
                bad_command();
                return;
            };
            if parts.next().is_some() {
                bad_command();
                return;
            }
            let Some(key) = parse_key(key_str) else {
                bad_command();
                return;
            };
            store.insert(key, value.to_string());
            *dirty = true;
        }
        "g" => {
            let Some(key_str) = parts.next() else {
                bad_command();
                return;
            };
            if parts.next().is_some() {
                bad_command();
                return;
            }
            let Some(key) = parse_key(key_str) else {
                bad_command();
                return;
            };
            match store.get(&key) {
                Some(value) => println!("{},{}", key, value),
                None => println!("{} not found", key),
            }
        }
        "d" => {
            let Some(key_str) = parts.next() else {
                bad_command();
                return;
            };
            if parts.next().is_some() {
                bad_command();
                return;
            }
            let Some(key) = parse_key(key_str) else {
                bad_command();
                return;
            };
            if store.remove(&key).is_none() {
                println!("{} not found", key);
            } else {
                *dirty = true;
            }
        }
        "c" => {
            if parts.next().is_some() {
                bad_command();
                return;
            }
            store.clear();
            *dirty = true;
        }
        "a" => {
            if parts.next().is_some() {
                bad_command();
                return;
            }
            for (key, value) in store.iter() {
                println!("{},{}", key, value);
            }
        }
        _ => bad_command(),
    }
}

fn parse_key(key: &str) -> Option<i64> {
    key.trim().parse::<i64>().ok()
}

fn bad_command() {
    println!("bad command");
}
