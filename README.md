# Web OS

A Rust re-implementation of xv6 for RISC-V, available on the web.

## Development

### Installation

- [prek installation docs](https://prek.j178.dev/installation/)
- [act installation docs](https://nektosact.com/installation/index.html)

```bash
prek install
```

### Commands

```bash
cargo build --target riscv64gc-unknown-none-elf  # build kernel
cargo run --target riscv64gc-unknown-none-elf    # run kernel

prek run --all-files                             # run hooks
act push --bind                                  # test CI
```

## Roadmap

### Kernel

- [x] dual stream for pipes (kernel-space)
- [x] [COW mappings](https://pages.cs.wisc.edu/~remzi/OSTEP/lab-projects-xv6.pdf)
- [x] [mmap](https://pages.cs.wisc.edu/~remzi/OSTEP/lab-projects-xv6.pdf)
- [x] IPC: shared memory + semaphores
- [x] [kernel threads](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-xv6-threads)
- [x] proc control: signals + waitpid opts + interval timers
- [x] tty/job control: pgrp + sessions + controlling TTY + fg/bg
- [x] adv I/O: nonblock + poll/select
- [x] fcntl: F_GETFL/F_SETFL + FD_CLOEXEC
- [x] file locks: fcntl F_SETLK/F_GETLK
- [x] raw block device file: user fsck reads disk
- [x] fs names/attrs: rename + symlinks + permissions + umask
- [x] fs durability: fsync + timestamps
- [x] [journaling/crash-consistency](https://pages.cs.wisc.edu/~remzi/OSTEP/lab-projects-xv6.pdf)
- [x] sockets: AF_UNIX
- [x] net: virtio-net + IPv4/ARP + sockets (TCP/UDP)
- [x] cooperative, event-based scheduler for trusted kernel tasks
  - [blog_os](https://os.phil-opp.com/async-await/)
  - [multi-processor multi-queue scheduler](https://pages.cs.wisc.edu/~remzi/OSTEP/cpu-sched-multi.pdf)
  - [event-based concurrency](https://pages.cs.wisc.edu/~remzi/OSTEP/threads-events.pdf)
- [x] [MSIs](https://blog.stephenmarz.com/2022/06/30/msi/) + [APLIC](https://blog.stephenmarz.com/2022/07/26/aplic/)
- [ ] [framebuffer](https://blog.stephenmarz.com/2020/11/11/risc-v-os-using-rust-graphics/) and [keyboard/mouse input](https://blog.stephenmarz.com/2020/08/03/risc-v-os-using-rust-input-devices/)

### App

- [ ] stream kernel video to browser via VNC and WebRTC

### User-Space

- [ ] [zip/unzip](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/initial-utilities#wzip-and-wunzip) and [pzip](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-pzip)
- [ ] [reverse](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/initial-reverse)
- [ ] [concurrent web server](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-webserver)
- [ ] [mapreduce](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-mapreduce)
- [ ] [fsck](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/filesystems-checker)

## Credit

- [blog_os](https://github.com/phil-opp/blog_os)
- [octox](https://github.com/o8vm/octox)
- [adventures of os](https://blog.stephenmarz.com/)
- [ostep](https://pages.cs.wisc.edu/~remzi/OSTEP/)
