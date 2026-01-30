# Web OS

A Rust re-implementation of xv6 for RISC-V, available on the web.

## Development

### Installation

- [rustup](https://rustup.rs/)
- [qemu](https://www.qemu.org/download/)
- [npm](https://nodejs.org/en/download/)
- [prek](https://prek.j178.dev/installation/)
- [act](https://nektosact.com/installation/index.html)
- [flyctl](https://fly.io/docs/flyctl/install/)

Create an [Open Relay TURN server](https://www.metered.ca/tools/openrelay/)
account [here](https://dashboard.metered.ca/login?tool=turnserver) for TURN
server credentials.

```bash
npm i
prek install
fly auth login
```

### Commands

```bash
cargo run --target riscv64gc-unknown-none-elf    # run kernel
mprocs                                           # run server and frontend

prek run --all-files                             # run hooks
act push --bind                                  # test CI
docker build -t web-os .                         # test kernel build
docker run --rm -p 8080:8080 web-os              # test kernel run

fly launch
fly ips allocate-v4                              # allocate a dedicated IPv4 for WebRTC
fly secrets set ICE_PUBLIC_IPS=<dedicated-ipv4>
fly secrets set TURN_USERNAME=<username> TURN_CREDENTIAL=<credential>
fly redis create
fly secrets set UPSTASH_REDIS_REST_URL=<url> UPSTASH_REDIS_REST_TOKEN=<token>
fly deploy
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
- [x] [MSIs](https://blog.stephenmarz.com/2022/06/30/msi/) +
      [APLIC](https://blog.stephenmarz.com/2022/07/26/aplic/)
- [x] [framebuffer](https://blog.stephenmarz.com/2020/11/11/risc-v-os-using-rust-graphics/)
      and
      [keyboard/mouse input](https://blog.stephenmarz.com/2020/08/03/risc-v-os-using-rust-input-devices/)

### User-Space

- [x] [pzip/punzip](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-pzip)
- [x] [psort](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-sort)
- [x] [reverse](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/initial-reverse)
- [x] [kv store](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/initial-kv)
- [x] [concurrent web server](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-webserver)
- [x] [distributed fs](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/filesystems-distributed-ufs)
- [x] [mapreduce](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/concurrency-mapreduce)
- [x] [fsck](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/filesystems-checker)
- [x] [memcached](https://github.com/remzi-arpacidusseau/ostep-projects/tree/master/initial-memcached)

### App

- [x] stream kernel video to browser via VNC and WebRTC
- [x] add live metrics

## Credit

- [blog_os](https://github.com/phil-opp/blog_os)
- [octox](https://github.com/o8vm/octox)
- [adventures of os](https://blog.stephenmarz.com/)
- [ostep](https://pages.cs.wisc.edu/~remzi/OSTEP/)
