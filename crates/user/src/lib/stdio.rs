use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::fs::File;
use crate::io::{self, Read, Write};
use crate::mutex::Mutex;
use crate::sys::{self, Error::AlreadyExists, Error::Utf8Error, sync::OnceLock};

pub const STDIN_FILENO: usize = 0;
pub const STDOUT_FILENO: usize = 1;
pub const STDERR_FILENO: usize = 2;

static mut STDIN: OnceLock<Mutex<File>> = OnceLock::new();
static mut STDOUT: OnceLock<Mutex<File>> = OnceLock::new();
static mut STDERR: OnceLock<Mutex<File>> = OnceLock::new();

pub struct Stdin {
    inner: *mut OnceLock<Mutex<File>>,
}

pub fn stdin() -> Stdin {
    Stdin {
        inner: unsafe { &raw mut STDIN },
    }
}

impl Stdin {
    fn get_inner(&self) -> &mut OnceLock<Mutex<File>> {
        unsafe { &mut *self.inner }
    }

    pub fn set(&self, file: File) -> sys::Result<()> {
        self.get_inner()
            .set(Mutex::new(file))
            .or(Err(AlreadyExists))
    }

    pub fn replace(&mut self, src: &File) -> sys::Result<()> {
        let inner = self
            .get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDIN_FILENO) }));
        let mut dst = inner.lock();
        File::dup2(src, &mut dst)
    }

    pub fn read_line(&mut self, buf: &mut String) -> sys::Result<usize> {
        let mut char: [u8; 1] = [0];
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            let cc = self.read(&mut char)?;
            if cc < 1 {
                break;
            }
            bytes.extend_from_slice(&char);
            if char[0] == b'\n' || char[0] == b'\r' {
                break;
            }
        }
        buf.push_str(core::str::from_utf8(&bytes).or(Err(Utf8Error))?);
        Ok(buf.len())
    }
}

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> sys::Result<usize> {
        self.get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDIN_FILENO) }))
            .lock()
            .read(buf)
    }
}

pub struct Stdout {
    inner: *mut OnceLock<Mutex<File>>,
}

pub fn stdout() -> Stdout {
    Stdout {
        inner: unsafe { &raw mut STDOUT },
    }
}

impl Stdout {
    fn get_inner(&self) -> &mut OnceLock<Mutex<File>> {
        unsafe { &mut *self.inner }
    }

    pub fn set(&self, file: File) -> sys::Result<()> {
        self.get_inner()
            .set(Mutex::new(file))
            .or(Err(AlreadyExists))
    }

    pub fn replace(&mut self, src: &File) -> sys::Result<()> {
        let inner = self
            .get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDOUT_FILENO) }));
        let mut dst = inner.lock();
        File::dup2(src, &mut dst)
    }
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> sys::Result<usize> {
        self.get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDOUT_FILENO) }))
            .lock()
            .write(buf)
    }
}

pub struct Stderr {
    inner: *mut OnceLock<Mutex<File>>,
}

pub fn stderr() -> Stderr {
    Stderr {
        inner: unsafe { &raw mut STDERR },
    }
}

impl Stderr {
    fn get_inner(&self) -> &mut OnceLock<Mutex<File>> {
        unsafe { &mut *self.inner }
    }

    pub fn set(&self, file: File) -> sys::Result<()> {
        self.get_inner()
            .set(Mutex::new(file))
            .or(Err(AlreadyExists))
    }

    pub fn replace(&mut self, src: &File) -> sys::Result<()> {
        let inner = self
            .get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDERR_FILENO) }));
        let mut dst = inner.lock();
        File::dup2(src, &mut dst)
    }
}

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> sys::Result<usize> {
        self.get_inner()
            .get_or_init(|| Mutex::new(unsafe { File::from_raw_fd(STDERR_FILENO) }))
            .lock()
            .write(buf)
    }
}

fn print_to<T>(args: fmt::Arguments<'_>, global_s: fn() -> T)
where
    T: Write,
{
    if let Err(e) = global_s().write_fmt(args) {
        panic!("failed printing {e}");
    }
}

pub fn _print(args: fmt::Arguments<'_>) {
    print_to(args, stdout);
}

pub fn _eprint(args: fmt::Arguments<'_>) {
    print_to(args, stderr);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::stdio::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    ($fmt:expr) => {
        $crate::print!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::print!(concat!($fmt, "\n"), $($arg)*)
    };
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::stdio::_eprint(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! eprintln {
    ($fmt:expr) => {
        $crate::eprint!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::eprint!(concat!($fmt, "\n"), $($arg)*)
    };
}

pub fn panic_output() -> Option<impl io::Write> {
    Some(stderr())
}
