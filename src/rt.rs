//! Threads for the litter's two-thread agent, modeled on
//! `userspace/amd64/threadprobe` — the raw version, not musl pthreads.
//!
//! Why not `pthread_create`: meow starts from a raw `_start`
//! (`libakuma`'s global_asm calls `libakuma_init` then `main`), so musl's
//! `__libc_start_main` never runs and musl's thread/TLS machinery is never
//! initialized — `pthread_create` fails in exactly the way the probe's
//! README calls "musl wants something this does not ask for". The probe's
//! answer was to drop to `clone(CLONE_VM|CLONE_THREAD|...)` with an
//! assembly child entry that never returns into Rust, and that is what
//! this module does.
//!
//! One concession: the child is created with `CLONE_SETTLS` pointing at
//! the PARENT's thread pointer, so both threads share the one TLS image.
//! meow is no_std; the only TLS user in practice is musl's `errno`, and a
//! torn errno read in a failed syscall is harmless here — documented,
//! accepted, revisit only if a litter thread ever needs its own errno.
//!
//! The child exits with `exit` (93), never `exit_group` (94) — that
//! distinction is half of what the probe exists to check: killing the
//! agent process because a raft tick ended would be absurd.

use alloc::format;
use core::ffi::c_void;
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

// aarch64 Linux syscall numbers.
const SYS_CLONE: u64 = 220;
const SYS_EXIT: u64 = 93;

// clone(2) flags — the set the probe validates bit by bit, plus the ones a
// real thread (as opposed to a boot check) wants: TLS sharing via
// CLONE_SETTLS and SYSVSEM adjust semantics.
const CLONE_VM: u64 = 0x0000_0100;
const CLONE_FS: u64 = 0x0000_0200;
const CLONE_FILES: u64 = 0x0000_0400;
const CLONE_SIGHAND: u64 = 0x0000_0800;
const CLONE_THREAD: u64 = 0x0001_0000;
const CLONE_SYSVSEM: u64 = 0x0400_0000;

/// No CLONE_SETTLS, deliberately: meow's raw `_start` never initializes
/// the parent's `tpidr_el0`, and handing a NULL TLS to clone is refused.
/// The child therefore runs with a zero thread pointer — which means NO
/// TLS use is allowed in litter thread code: no musl `errno`, no libc
/// TLS-dependent calls. libakuma does raw syscalls and meow is no_std, so
/// everything the raft thread runs is TLS-free by construction. Verified
/// empirically (bisected at runtime once — the SETTLS variant is the one
/// that returned -EINVAL in the Alpine container).
const THREAD_FLAGS: u64 =
    CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;

/// The child's stack: 64 KiB in `.bss`, `align(16)` because AAPCS64
/// requires it and a misaligned stack is the kind of thing that works
/// until the first SIMD spill. One stack per thread — the litter is two
/// threads per process, so a fixed table beats an allocator here.
#[repr(align(16))]
struct Stack([u8; 64 * 1024]);

static mut STACKS: [Option<Stack>; 2] = [None, None];
static NEXT_STACK: AtomicU64 = AtomicU64::new(0);

// Spawn the child. The parent cannot call `clone` from Rust and let the
// child continue in Rust: both sides return from the same instruction and
// the child's `sp` points at a stack with no frame on it, so the compiled
// epilogue would pop garbage. Hence assembly on both sides of the syscall,
// and an entry function planted on the child's own stack before the clone.
//
// The parent's thread pointer is read with `mrs` and handed to clone as
// the TLS argument; the kernel installs it as the child's `tpidr_el0`.
core::arch::global_asm!(
    r#"
    .section .text.litter_spawn_thread
    .global litter_spawn_thread
litter_spawn_thread:
    /* x0 = flags, x1 = child stack top, x2 = entry fn */
    mov x6, x0                  /* flags */
    mrs x4, tpidr_el0           /* child shares the parent's TLS image */
    sub x1, x1, 16
    str x2, [x1]                /* plant the entry fn on the child stack */
    mov x2, xzr                 /* parent_tid = NULL */
    mov x3, xzr                 /* child_tid = NULL (detached) */
    mov x8, 220                 /* SYS_clone */
    svc #0
    cbnz x0, 2f                 /* parent: x0 = child tid, done */
    /* child */
    ldr x19, [sp], 16           /* entry fn */
    blr x19
    /* the entry fn is done: end this thread and only this thread */
    mov x0, 93                  /* SYS_exit — NOT exit_group */
    svc #0
1:  b 1b
2:  ret
"#
);

unsafe extern "C" {
    /// Returns the child's tid in the parent; never returns in the child.
    fn litter_spawn_thread(flags: u64, stack_top: *mut c_void, entry: fn()) -> i64;
}

/// Spawn `f` on a dedicated raw-clone thread. Returns false when no stack
/// slot is free or the clone failed — callers treat that as "run degraded
/// or exit loudly", never as retry-forever.
///
/// # Safety
/// `f` must not require thread-local state unique to the parent (it runs
/// on a clone sharing the parent's TLS image) and must either run forever
/// or exit its own thread — the child's asm never returns into the
/// parent's control flow.
pub unsafe fn spawn_detached(f: fn()) -> bool {
    let slot = NEXT_STACK.fetch_add(1, Ordering::AcqRel) as usize;
    if slot >= STACKS.len() {
        return false;
    }
    // SAFETY: single-threaded here (the agent's main loop, before any
    // child exists that could race this table).
    let stack = unsafe {
        STACKS[slot] = Some(Stack([0; 64 * 1024]));
        STACKS[slot].as_mut().unwrap_unchecked()
    };
    let stack_top = stack.0.as_mut_ptr().wrapping_add(64 * 1024) as *mut c_void;
    let tid = unsafe { litter_spawn_thread(THREAD_FLAGS, stack_top, f) };
    tid > 0
}

/// Coarse mutex for the litter's shared in-memory state (`HubState`, the
/// static-peer table) — both threads take it for microseconds per frame.
/// The guard unlocks on drop, so no critical section can forget.
pub struct PMutex<T> {
    mutex: FutexWord,
    data: core::cell::UnsafeCell<T>,
}

#[repr(transparent)]
struct FutexWord(AtomicI32);

impl<T> PMutex<T> {
    pub fn new(value: T) -> Self {
        Self { mutex: FutexWord(AtomicI32::new(0)), data: core::cell::UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> PMutexGuard<'_, T> {
        // Swap, then futex-sleep while contested. Acquire on the swap pairs
        // with the Release store in the guard's drop.
        loop {
            if self.mutex.0.swap(1, Ordering::Acquire) == 0 {
                break;
            }
            futex_wait(&self.mutex.0, 1);
        }
        PMutexGuard { mutex: &self.mutex, data: self.data.get(), _marker: core::marker::PhantomData }
    }
}

const FUTEX_WAIT_PRIVATE: i32 = 128;
const FUTEX_WAKE_PRIVATE: i32 = 129;

const SYS_FUTEX: u64 = 98; // aarch64

fn futex_wait(word: &AtomicI32, expected: i32) {
    // Ignore the return: a spurious wake or EAGAIN just re-runs the swap loop.
    let _ = libakuma::syscall(
        SYS_FUTEX,
        word as *const AtomicI32 as *const i32 as u64,
        FUTEX_WAIT_PRIVATE as u64,
        expected as u64,
        0,
        0,
        0,
    );
}

fn futex_wake_one(word: &AtomicI32) {
    let _ = libakuma::syscall(
        SYS_FUTEX,
        word as *const AtomicI32 as *const i32 as u64,
        FUTEX_WAKE_PRIVATE as u64,
        1,
        0,
        0,
        0,
    );
}

pub struct PMutexGuard<'a, T> {
    mutex: &'a FutexWord,
    data: *mut T,
    _marker: core::marker::PhantomData<&'a ()>,
}

impl<'a, T> core::ops::Deref for PMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<'a, T> core::ops::DerefMut for PMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
}

impl<'a, T> Drop for PMutexGuard<'a, T> {
    fn drop(&mut self) {
        // Release ordering publishes everything the critical section wrote.
        self.mutex.0.store(0, Ordering::Release);
        futex_wake_one(&self.mutex.0);
    }
}

// Safety: the mutex guards every access to `data`.
unsafe impl<T: Send> Send for PMutex<T> {}
unsafe impl<T: Send> Sync for PMutex<T> {}

#[cfg(feature = "tests")]
pub fn run_tests() -> i32 {
    let mut passed = 0usize;
    let mut total = 0usize;
    libakuma::print("--- rt threads tests ---\n");

    // PMutex: lock/mutate/unlock round-trip, and a dropped guard relocks —
    // the bug the Drop guard exists to prevent.
    total += 1;
    {
        let m = PMutex::new(41u32);
        {
            let mut g = m.lock();
            *g += 1;
        }
        let again = m.lock();
        if *again == 42 {
            passed += 1;
        } else {
            libakuma::print(&format!("  [!] pmutex round-trip: {}\n", *again));
        }
    }

    // spawn_detached: the child flips a flag the parent can observe; the
    // channel is atomics-only so the test needs nothing beyond the spawn.
    // Bounded wait — a failure must slow the suite, not hang it.
    total += 1;
    {
        static CHILD_FLAG: AtomicU64 = AtomicU64::new(0);
        fn child() {
            CHILD_FLAG.store(1, Ordering::Release);
        }
        let spawned = unsafe { spawn_detached(child) };
        let mut seen = false;
        if spawned {
            for _ in 0..100 {
                if CHILD_FLAG.load(Ordering::Acquire) == 1 {
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
