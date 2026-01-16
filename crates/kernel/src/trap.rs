use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    imsic,
    kernelvec::kernelvec,
    memlayout::{STACK_PAGE_NUM, TRAMPOLINE, UART0_IRQ, VIRTIO0_IRQ},
    proc::{self, Cpus, ProcState},
    riscv::{
        registers::{scause::*, *},
        *,
    },
    spinlock::Mutex,
    syscall::syscall,
    task,
    trampoline::trampoline,
    uart::UART,
    virtio_disk::DISK,
    vm::{Addr, UVAddr},
};

unsafe extern "C" {
    fn uservec();
    fn userret();
}

#[derive(PartialEq)]
pub enum Intr {
    Timer,
    Device,
}

pub static TICKS: Mutex<usize> = Mutex::new(0, "time");
pub static EXT_IRQS: AtomicUsize = AtomicUsize::new(0);

// set up to take exceptions and traps while in the kernel.
#[unsafe(no_mangle)]
pub fn inithart() {
    unsafe {
        stvec::write(kernelvec as *const () as usize, stvec::TrapMode::Direct);
    }
    imsic::init_hart();
}

// handle an interrupt, exception, or system call from user space.
// called from trampoline.rs
//
#[unsafe(no_mangle)]
pub extern "C" fn usertrap() -> ! {
    assert!(
        sstatus::read().spp() == sstatus::SPP::User,
        "usertrap: not from user mode"
    );
    assert!(!intr_get(), "kerneltrap: interrupts enabled");

    // send interrupts and exceptions to kerneltrap().
    // since we're now in the kernel.
    unsafe {
        stvec::write(kernelvec as *const () as usize, stvec::TrapMode::Direct);
    }

    let p = Cpus::myproc().unwrap();
    let data = unsafe { &mut (*p.data.get()) };
    let tf = data.trapframe.as_mut().unwrap();

    // save user program counter
    tf.epc = sepc::read();

    let mut which_dev = None;
    match scause::read().cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            // system call

            if p.inner.lock().killed {
                proc::exit(-1)
            }

            // sepc points to the ecall instruction,
            // but we want to return to the next instruction.
            tf.epc += 4;

            // an interrupt will change sstatus &c registers,
            // so don't enable until done with those registers.
            intr_on();

            syscall();
        }
        Trap::Exception(Exception::StorePageFault) => {
            let fault = stval::read();
            let va = UVAddr::from(fault);

            // cow for regular user mem
            let mut did_cow = false;
            {
                let aspace = data.aspace.as_ref().unwrap();
                let mut as_inner = aspace.inner.lock();
                if va.into_usize() < as_inner.sz {
                    let uvm = as_inner.uvm.as_mut().unwrap();
                    if uvm.resolve_cow(va).is_err() {
                        p.inner.lock().killed = true;
                    }
                    did_cow = true;
                }
            }
            if !did_cow {
                // lazy mmap
                intr_on();
                if proc::handle_user_page_fault(fault, Exception::StorePageFault).is_err() {
                    p.inner.lock().killed = true;
                }
            }
        }
        Trap::Exception(e @ (Exception::InstructionPageFault | Exception::LoadPageFault)) => {
            // lazy mmap
            intr_on();
            let fault = stval::read();
            if proc::handle_user_page_fault(fault, e).is_err() {
                p.inner.lock().killed = true;
            }
        }
        Trap::Interrupt(intr)
            if {
                which_dev = devintr(intr);
                which_dev.is_some()
            } => {}
        _ => {
            let mut inner = p.inner.lock();
            println!(
                "usertrap(): unexpected scause {:?}, pid={:?}",
                scause::read().cause(),
                inner.pid
            );
            println!(
                "            sepc={:X}, stval={:X}",
                sepc::read(),
                stval::read()
            );
            inner.killed = true;
        }
    }

    proc::deliver_signals(&p);

    if p.inner.lock().killed {
        proc::exit(-1)
    }

    // give up the CPU if this is a timer interrupt.
    if Some(Intr::Timer) == which_dev {
        proc::yielding()
    }

    if Some(Intr::Device) == which_dev {
        let cpu = unsafe { Cpus::cpu_id() };
        if !task::ready_is_empty_cpu(cpu) {
            proc::yielding()
        }
    }

    unsafe { usertrap_ret() }
}

// return to user space
//
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usertrap_ret() -> ! {
    let p = Cpus::myproc().unwrap();

    // we're about to switch the destination of traps from
    // kerneltrap() to usertrap(), so turn off interrupts until
    // we're back in user space, where usertrap() is correct.
    intr_off();

    // send syscalls, interrupts, and exceptions to trampoline.rs
    unsafe {
        stvec::write(
            TRAMPOLINE + (uservec as *const () as usize - trampoline as *const () as usize),
            stvec::TrapMode::Direct,
        );
    }

    let data = p.data_mut(); //&mut *p.data.get();

    // set up trapframe values that uservec will need when
    // the process next re-enters the kernel.
    let tf = data.trapframe.as_mut().unwrap();
    tf.kernel_satp = unsafe { satp::read() }.bits();
    tf.kernel_sp = data.kstack.into_usize() + PGSIZE * STACK_PAGE_NUM;
    tf.kernel_trap = usertrap as *const () as usize;
    tf.kernel_hartid = unsafe { Cpus::cpu_id() };

    // tell trampoline where this thread's trapframe lives
    unsafe {
        asm!("csrw sscratch, {}", in(reg) data.trapframe_va.into_usize());
    }

    // set up the registers that trampoline.rs's sret will use
    // to get to user space.

    // set S Previous Privilege mode to User.
    unsafe {
        sstatus::set_spp(sstatus::SPP::User); // clear SPP to 0 for user mode.
        sstatus::set_spie(); // enable interrupts in user mode.
    }

    // set S Exception Program Counter to the saved user pc.
    sepc::write(tf.epc);

    // tell trampoline.rs the user page table to switch to.
    let satp = data
        .aspace
        .as_ref()
        .unwrap()
        .inner
        .lock()
        .uvm
        .as_ref()
        .unwrap()
        .as_satp();

    // jump to trampoline.rs at the top of memory, which
    // switches to the user page table, restores user registers,
    // and switches to user mode with sret.

    let fn_0: usize =
        TRAMPOLINE + (userret as *const () as usize - trampoline as *const () as usize);
    let fn_0: extern "C" fn(usize) -> ! = unsafe { core::mem::transmute(fn_0) };
    fn_0(satp)
}

// interrupts and exceptions from kernel code go here via kernelvec,
// on whatever the current kernel stack is.
#[unsafe(no_mangle)]
pub extern "C" fn kerneltrap() {
    let which_dev;
    let sepc = sepc::read();
    let sstatus = sstatus::read();
    let scause = scause::read();

    assert!(
        sstatus.spp() == sstatus::SPP::Supervisor,
        "not from supervisor mode"
    );
    assert!(!intr_get(), "kerneltrap: interrupts enabled");

    match scause.cause() {
        Trap::Interrupt(intr)
            if {
                which_dev = devintr(intr);
                which_dev.is_some()
            } => {}
        _ => {
            panic!(
                "kerneltrap: scause = {:?}, sepc = {:x}, stval = {:x}",
                scause.cause(),
                sepc::read(),
                stval::read()
            );
        }
    }

    // give up the CPU if this is a timer interrupt.
    let should_yield = if Some(Intr::Timer) != which_dev {
        false
    } else if let Some(p) = Cpus::myproc() {
        p.inner.lock().state == ProcState::RUNNING
    } else {
        false
    };
    if should_yield {
        proc::yielding()
    }

    // the yielding() may have caused some traps to occur.
    // so restore trap registers for use by kernelvec.rs's sepc instruction.
    sepc::write(sepc);
    sstatus.restore();
}

fn clockintr() {
    let cpu = unsafe { Cpus::cpu_id() };
    task::on_tick_cpu(cpu);
    if cpu == 0 {
        let mut ticks = TICKS.lock();
        *ticks += 1;
        proc::wakeup(&(*ticks) as *const _ as usize);
        proc::on_tick(*ticks);
    }
}

// check if it's an external interrupt or software interrupt,
// and handle it.
// returns Option<Intr>
// devintr() is safe because it is only called in the non-interruptable
// part of trap.rs.
fn devintr(intr: Interrupt) -> Option<Intr> {
    match intr {
        Interrupt::SupervisorExternal => {
            // Supervisor external interrupt delivered via IMSIC.
            // Drain all pending messages. Each pop() claims and clears one message.
            loop {
                let msg = imsic::pop();
                if msg == 0 {
                    break;
                }
                EXT_IRQS.fetch_add(1, Ordering::Relaxed);
                match msg {
                    UART0_IRQ => UART.intr(),
                    VIRTIO0_IRQ => DISK.intr(),
                    _ => println!("unexpected msi msg={}", msg),
                }
            }

            Some(Intr::Device)
        }
        Interrupt::SupervisorSoft => {
            // software interrupt from a machine-mode timer interrupt,
            // forwarded by timervec in kernelvec.rs.
            clockintr();

            // acknowledge the software interrupt by clearing
            // the SSIP bit in sip.
            unsafe {
                sip::clear_ssoft();
            }

            Some(Intr::Timer)
        }
        _ => None,
    }
}
