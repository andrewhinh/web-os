#![no_std]

use ulib::{
    eprintln,
    fs::OpenOptions,
    io::Read,
    pipe, print, println,
    sys::{
        self, Error,
        fcntl::{FcntlCmd, fd, omode},
    },
};

fn main() -> sys::Result<()> {
    println!("test_fcntl: start");

    let path = "/t_fcntl.txt";
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("test_fcntl: open err={}", e);
            return Err(e);
        }
    };

    let fd_num = file.get_fd();
    let fl0 = sys::fcntl(fd_num, FcntlCmd::GetFl, 0)?;
    let fd0 = sys::fcntl(fd_num, FcntlCmd::GetFd, 0)?;
    print_fl("file", fl0);
    print_fd("file", fd0);

    let new_fl = fl0 | omode::APPEND | omode::NONBLOCK;
    sys::fcntl(fd_num, FcntlCmd::SetFl, new_fl)?;
    let fl1 = sys::fcntl(fd_num, FcntlCmd::GetFl, 0)?;
    print_fl("file", fl1);

    sys::fcntl(fd_num, FcntlCmd::SetFd, fd::CLOEXEC)?;
    let fd1 = sys::fcntl(fd_num, FcntlCmd::GetFd, 0)?;
    print_fd("file", fd1);

    let (mut reader, _writer) = pipe::pipe()?;
    let rfd = reader.get_fd();
    let pfl0 = sys::fcntl(rfd, FcntlCmd::GetFl, 0)?;
    sys::fcntl(rfd, FcntlCmd::SetFl, pfl0 | omode::NONBLOCK)?;
    let pfl1 = sys::fcntl(rfd, FcntlCmd::GetFl, 0)?;
    print_fl("pipe", pfl1);

    let mut buf = [0u8; 1];
    match reader.read(&mut buf) {
        Err(Error::WouldBlock) => println!("test_fcntl: nonblock read -> would block"),
        Ok(n) => println!("test_fcntl: pipe read n={}", n),
        Err(e) => eprintln!("test_fcntl: pipe read err={}", e),
    }

    Ok(())
}

fn print_fl(label: &str, flags: usize) {
    let acc = flags & (omode::RDWR | omode::WRONLY);
    let acc_str = if acc == omode::RDWR {
        "rdwr"
    } else if acc == omode::WRONLY {
        "wronly"
    } else {
        "rdonly"
    };
    print!("test_fcntl: {} fl={} {}", label, flags, acc_str);
    if flags & omode::APPEND != 0 {
        print!(" append");
    }
    if flags & omode::NONBLOCK != 0 {
        print!(" nonblock");
    }
    println!("");
}

fn print_fd(label: &str, flags: usize) {
    print!("test_fcntl: {} fd={} ", label, flags);
    if flags & fd::CLOEXEC != 0 {
        println!("cloexec");
    } else {
        println!("none");
    }
}
