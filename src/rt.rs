//! Raw pthread plumbing for the litter's two-thread agent — the one place
//! meow touches the OS thread API. The `linux-net` build links musl, which
//! provides `pthread_*` directly (no separate libpthread); the bare-metal
//! Akuma build hits the kernel's own thread runtime through the same C
//! ABI (`akuma-threading` implements the clone/pthread surface), so this
//! module compiles unchanged for both.
//!
//! Deliberately minimal: one spawned function, one coarse mutex. No
//! channels, no condvars, no name mangling beyond what pthread requires —
//! the litter is two threads per agent (raft/serve + agent loop; see
//! `docs/LITTER_STATE_MACHINE.md`), and a coarse `PMutex<HubState>` held
//! for microseconds per frame is the whole synchronization story.

use core::ffi::c_void;

const PTHREAD_MUTEX_NORMAL: i32 = 0;

#[repr(C)]
struct PthreadMutexT {
    /// musl's pthread_mutex_t is 40 bytes on aarch64 (4 x u32 + 2 x u64
    /// rounds up); size it generously and only ever touch it through
    /// pthread calls. Over-size is harmless, under-size is UB.
    opaque: [u64; 8],
}

extern "C" {
    fn pthread_create(
        thread: *mut u64,
        attr: *const c_void,
        start_routine: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> i32;
    fn pthread_detach(thread: u64) -> i32;
    fn pthread_mutex_init(mutex: *mut PthreadMutexT, attr: *const i32) -> i32;
    fn pthread_mutex_lock(mutex: *mut PthreadMutexT) -> i32;
    fn pthread_mutex_unlock(mutex: *mut PthreadMutexT) -> i32;
}

extern "C" fn trampoline(arg: *mut c_void) -> *mut c_void {
    // Take ownership of the boxed closure and run it. Detached: the raft
    // thread lives exactly as long as the process does — nobody joins it.
    let func = unsafe { Box::from_raw(arg as *mut Box<dyn FnOnce()>) };
    func();
    core::ptr::null_mut()
}

/// Spawn a detached thread running `f`. Returns false if pthread_create
/// failed (thread table full) — callers treat that as "run degraded or
/// exit loudly", never as retry-forever.
pub fn spawn_detached(f: impl FnOnce() + 'static) -> bool {
    let boxed: Box<dyn FnOnce()> = Box::new(f);
    let arg = Box::into_raw(Box::new(boxed)) as *mut c_void;
    let mut tid: u64 = 0;
    let rc = unsafe { pthread_create(&mut tid, core::ptr::null(), trampoline, arg) };
    if rc != 0 {
        // Give the boxed closure back so the failure is at least a leak-free one.
        unsafe {
            drop(Box::from_raw(arg as *mut Box<dyn FnOnce()>));
        }
        return false;
    }
    unsafe { pthread_detach(tid) };
    true
}

/// Coarse mutex over shared litter state. `lock()` blocks; every critical
/// section in the litter is a handful of Vec operations, so there is no
/// try-lock, no poisoning, no fair-queue pretense.
pub struct PMutex<T> {
    mutex: PthreadMutexT,
    data: core::cell::UnsafeCell<T>,
}

impl<T> PMutex<T> {
    pub fn new(value: T) -> Self {
        let mut mutex = PthreadMutexT { opaque: [0; 8] };
        unsafe { pthread_mutex_init(&mut mutex, &PTHREAD_MUTEX_NORMAL) };
        Self { mutex, data: core::cell::UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> &mut T {
        unsafe {
            pthread_mutex_lock(&self.mutex);
            &mut *self.data.get()
        }
    }
}

// Safety: the mutex guards every access to `data`; a litter agent's threads
// only ever touch the same PMutex from these two threads.
unsafe impl<T: Send> Send for PMutex<T> {}
unsafe impl<T: Send> Sync for PMutex<T> {}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- rt threads tests ---\n");

    // PMutex: lock/mutate/unlock round-trip on the calling thread.
    total += 1;
    {
        let m = PMutex::new(41u32);
        *m.lock() += 1;
        let v = *m.lock();
        if v == 42 {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] pmutex round-trip: {}\n", v));
        }
    }

    // spawn_detached: the child flips a shared flag; the parent polls for it
    // briefly. Bounded wait — a failure here must hang the test suite, not
    // the agent.
    total += 1;
    {
        let flag = alloc::sync::Arc::new(PMutex::new(false));
        let child_flag = flag.clone();
        let spawned = spawn_detached(move || {
            *child_flag.lock() = true;
        });
        let mut seen = false;
        if spawned {
            for _ in 0..100 {
                if *flag.lock() {
                    seen = true;
                    break;
                }
                libakuma::sleep_ms(10);
            }
        }
        if spawned && seen {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] spawn_detached: spawned={} seen={}\n", spawned, seen));
        }
    }

    libakuma::print(&format!("  result: {}/{}\n", passed, total));
    if passed == total { 0 } else { 1 }
}
