use crate::{Error, Result};
use std::sync::atomic::{AtomicI32, Ordering};

static RECEIVED: AtomicI32 = AtomicI32::new(0);

pub fn received() -> i32 {
    RECEIVED.load(Ordering::Relaxed)
}

pub fn check() -> Result<()> {
    if received() == 0 {
        Ok(())
    } else {
        Err(Error::Invalid("operation interrupted"))
    }
}

#[cfg(unix)]
extern "C" fn handle(signal: libc::c_int) {
    RECEIVED.store(signal, Ordering::Relaxed);
}

#[cfg(unix)]
pub struct Handlers(Vec<(libc::c_int, libc::sigaction)>);

#[cfg(unix)]
pub fn install() -> Result<Handlers> {
    let mut handlers = Handlers(Vec::new());
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: zero is a valid initial representation; fields are set before use.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: storage is initialized by sigaction before its value is used.
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handle as *const () as usize;
        // Do not restart blocking operations after termination is requested.
        action.sa_flags = 0;
        // SAFETY: pointers refer to live sigaction storage; handler only stores an atomic integer.
        if unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, &mut previous)
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        handlers.0.push((signal, previous));
    }
    Ok(handlers)
}

#[cfg(unix)]
impl Drop for Handlers {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            // SAFETY: previous is the initialized handler returned by sigaction.
            if unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) } != 0 {
                crate::log("could not restore signal handler");
            }
        }
    }
}

#[cfg(not(unix))]
pub fn install() -> Result<()> {
    Ok(())
}
