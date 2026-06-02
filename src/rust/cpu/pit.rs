// Programmable Interval Timer (Intel 8253/8254)
//
// Ported from src/pit.js.  See also:
//   https://wiki.osdev.org/Programmable_Interval_Timer

use crate::cpu::pic;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Oscillator frequency in kHz (1.193182 MHz)
const OSCILLATOR_FREQ: f64 = 1193.1816666;

/// Number of counters in the PIT (0, 1, 2)
const NUM_COUNTERS: usize = 3;

// Counter read modes
const READ_MODE_LATCH: u8 = 0;
const READ_MODE_LSB: u8 = 1;
const READ_MODE_MSB: u8 = 2;
const READ_MODE_LSB_MSB: u8 = 3;

// Latch state constants
const LATCH_NONE: u8 = 0;
const LATCH_LOW: u8 = 1;
const LATCH_HIGH: u8 = 2;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Counter {
    /// When the counter was last started/reset (in ms from microtick())
    start_time: f64,
    /// The counter value at start_time
    start_value: u16,

    /// Reload value written by the guest
    reload: u16,

    /// Whether the counter is currently enabled
    enabled: bool,

    /// Counter mode (0..7, but 6/7 aliased to 2/3)
    mode: u8,

    /// Access mode for reading/writing (READ_MODE_*)
    read_mode: u8,

    /// Whether the next access is low byte (true) or high byte (false)
    next_low: bool,

    /// Latch state: 2=high pending, 1=low pending, 0=none
    latch: u8,
    /// Latched value snapshot
    latch_value: u16,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            start_time: 0.0,
            start_value: 0,
            reload: 0,
            enabled: false,
            mode: 0,
            read_mode: 0,
            next_low: true,
            latch: 0,
            latch_value: 0,
        }
    }
}

pub struct Pit {
    counters: [Counter; NUM_COUNTERS],
}

impl Default for Pit {
    fn default() -> Self {
        Self {
            counters: [Counter::default(); NUM_COUNTERS],
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static PIT: std::sync::LazyLock<Mutex<Pit>> = std::sync::LazyLock::new(|| {
    Mutex::new(Pit {
        counters: [Counter {
            start_time: 0.0,
            start_value: 0,
            reload: 0,
            enabled: false,
            mode: 0,
            read_mode: 0,
            next_low: true,
            latch: 0,
            latch_value: 0,
        }; NUM_COUNTERS],
    })
});

fn get_pit() -> std::sync::MutexGuard<'static, Pit> {
    PIT.try_lock().unwrap()
}

// IRQ 0 is the PIT's interrupt line
fn raise_irq() { pic::set_irq(0); }
fn lower_irq() { pic::clear_irq(0); }

// ---------------------------------------------------------------------------
// Exports (called from JavaScript via WASM imports)
// ---------------------------------------------------------------------------

pub fn get_pit_addr() -> u32 {
    &raw mut *get_pit() as u32
}

pub fn pit_init() {
    let mut pit = get_pit();
    *pit = Pit::default();
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Get the current counter value for a given counter index at time `now`.
fn get_counter_value(counter: &Counter, now: f64) -> u16 {
    if !counter.enabled {
        return 0;
    }

    let diff = now - counter.start_time;
    let diff_in_ticks = (diff * OSCILLATOR_FREQ) as u32;
    let reload = if counter.reload == 0 { 0x10000 } else { counter.reload as u32 };

    let start = counter.start_value as u32;
    let mut value = if start >= diff_in_ticks {
        start - diff_in_ticks
    } else {
        // Handle wraparound
        let under = diff_in_ticks - start;
        reload - (under % reload)
    };

    value %= reload;
    value as u16
}

/// Check whether counter `i` has rolled over (i.e., counted down past zero)
/// at time `now`.
fn did_rollover(counter: &Counter, now: f64) -> bool {
    let diff = now - counter.start_time;
    if diff < 0.0 {
        return true; // should only happen after state restore
    }
    let diff_in_ticks = (diff * OSCILLATOR_FREQ) as u32;
    (counter.start_value as u32) < diff_in_ticks
}
