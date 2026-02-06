use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};
use core::cell::UnsafeCell;

pub static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub static STREAMING: AtomicBool = AtomicBool::new(false);

pub struct AppState {
    pub global_input: String,
    pub message_queue: VecDeque<String>,
    pub command_history: Vec<String>,
    pub history_index: usize,
    pub saved_input: String,
    pub model_name: String,
    pub provider_name: String,
    pub last_history_kb: usize,
}

struct AtomicAppState {
    initialized: AtomicBool,
    state: UnsafeCell<Option<AppState>>,
}

// Safety: Akuma userspace is single-threaded, atomic flag ensures safe init
unsafe impl Sync for AtomicAppState {}

impl AtomicAppState {
    const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            state: UnsafeCell::new(None),
        }
    }

    fn ensure_init(&self) {
        if !self.initialized.load(Ordering::Acquire) {
            unsafe {
                *self.state.get() = Some(AppState {
                    global_input: String::new(),
                    message_queue: VecDeque::new(),
                    command_history: Vec::new(),
                    history_index: 0,
                    saved_input: String::new(),
                    model_name: String::from("unknown"),
                    provider_name: String::from("unknown"),
                    last_history_kb: 0,
                });
            }
            self.initialized.store(true, Ordering::Release);
        }
    }

    fn get_mut(&self) -> &mut AppState {
        self.ensure_init();
        unsafe { (*self.state.get()).as_mut().unwrap() }
    }
}

static STATE: AtomicAppState = AtomicAppState::new();

pub fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut AppState) -> R,
{
    f(STATE.get_mut())
}

// Helper accessors to keep the rest of the code clean
pub fn with_global_input<F, R>(f: F) -> R
where
    F: FnOnce(&str) -> R,
{
    with_state(|s| f(&s.global_input))
}

pub fn get_global_input() -> String { with_state(|s| s.global_input.clone()) }
pub fn set_global_input(val: String) { with_state(|s| s.global_input = val); }
pub fn clear_global_input() { with_state(|s| s.global_input.clear()); }

pub fn push_message(msg: String) { with_state(|s| s.message_queue.push_back(msg)); }
pub fn pop_message() -> Option<String> { with_state(|s| s.message_queue.pop_front()) }
pub fn message_queue_len() -> usize { with_state(|s| s.message_queue.len()) }

pub fn add_to_history(cmd: &str) {
    with_state(|s| {
        if s.command_history.is_empty() || s.command_history.last().unwrap() != cmd {
            s.command_history.push(String::from(cmd));
            if s.command_history.len() > 50 { s.command_history.remove(0); }
        }
        s.history_index = s.command_history.len();
    });
}

pub fn get_history_index() -> usize { with_state(|s| s.history_index) }
pub fn set_history_index(idx: usize) { with_state(|s| s.history_index = idx); }
pub fn get_history_len() -> usize { with_state(|s| s.command_history.len()) }
pub fn get_history_item(idx: usize) -> Option<String> { with_state(|s| s.command_history.get(idx).cloned()) }

pub fn get_saved_input() -> String { with_state(|s| s.saved_input.clone()) }
pub fn set_saved_input(val: String) { with_state(|s| s.saved_input = val); }

pub fn set_model_and_provider(model: &str, provider: &str) {
    with_state(|s| {
        s.model_name = String::from(model);
        s.provider_name = String::from(provider);
    });
}

pub fn get_model_and_provider() -> (String, String) {
    with_state(|s| (s.model_name.clone(), s.provider_name.clone()))
}

pub fn with_model_and_provider<F, R>(f: F) -> R
where
    F: FnOnce(&str, &str) -> R,
{
    with_state(|s| f(&s.model_name, &s.provider_name))
}

pub fn get_last_history_kb() -> usize { with_state(|s| s.last_history_kb) }
pub fn set_last_history_kb(kb: usize) { with_state(|s| s.last_history_kb = kb); }
