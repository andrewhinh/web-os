#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

use ulib::{println, signal, sys};

static ALARMS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn alarm_handler(_sig: usize) {
    ALARMS.fetch_add(1, Ordering::SeqCst);
}

fn main() -> sys::Result<()> {
    println!("test_signal: start");

    signal::signal(signal::SIGALRM, alarm_handler as *const () as usize)?;
    signal::setitimer(2, 2)?;

    let mut last = 0;
    loop {
        let alarms = ALARMS.load(Ordering::SeqCst);
        if alarms != last {
            println!("test_signal: alarms={}", alarms);
            last = alarms;
            if alarms >= 3 {
                break;
            }
        }
        let _ = sys::sleep(1);
    }

    let _ = signal::setitimer(0, 0)?;
    Ok(())
}
