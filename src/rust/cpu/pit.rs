// Programmable Interval Timer (Intel 8253/8254)
//
// Ported from src/pit.js.  See also:
//   https://wiki.osdev.org/Programmable_Interval_Timer

use crate::cpu::cpu::js;
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

// State layout must match JS save/restore.  Keep in sync with cpu.js.
const PIT_STRUCT_SIZE: usize = std::mem::size_of::<Pit>();

#[derive(Clone, Copy)]
#[repr(C)]
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

#[repr(C)]
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

pub fn pit_state_size() -> u32 {
    PIT_STRUCT_SIZE as u32
}

pub fn pit_oscillator_freq() -> f64 {
    OSCILLATOR_FREQ
}

pub fn pit_init() {
    let mut pit = get_pit();
    *pit = Pit::default();
}

/// Called every tick from the JS main loop.  Returns the number of
/// milliseconds until the next PIT interrupt (for scheduling).
pub fn pit_timer(now: f64, no_irq: bool) -> f64 {
    let mut pit = get_pit();
    let mut time_to_next = 100.0_f64;

    let ctr0 = &mut pit.counters[0];

    // Counter 0 produces interrupts
    if !no_irq {
        if ctr0.enabled && did_rollover(ctr0, now) {
            ctr0.start_value = get_counter_value(ctr0, now);
            ctr0.start_time = now;

            lower_irq();
            raise_irq();

            if ctr0.mode == 0 {
                ctr0.enabled = false;
            }
        } else {
            lower_irq();
        }

        if ctr0.enabled {
            let diff = now - ctr0.start_time;
            let diff_in_ticks = (diff * OSCILLATOR_FREQ) as u32;
            let ticks_missing = ctr0.start_value as u32 - diff_in_ticks;
            time_to_next = ticks_missing as f64 / OSCILLATOR_FREQ;
        }
    }

    time_to_next
}

// ---------------------------------------------------------------------------
// IO port handlers (wired in cpu.rs io_port_read8 / io_port_write8)
// ---------------------------------------------------------------------------

pub fn port40_read() -> u32 { counter_read(0) }
pub fn port40_write(v: u8) { counter_write(0, v); }

pub fn port41_read() -> u32 { counter_read(1) }
pub fn port41_write(v: u8) { counter_write(1, v); }

pub fn port42_read() -> u32 { counter_read(2) }
pub fn port42_write(v: u8) {
    counter_write(2, v);
    // TODO: notify speaker of counter 2 update
}

/// Control word register (write-only)
pub fn port43_write(v: u8) {
    let mut pit = get_pit();
    let mode = (v >> 1) & 7;
    let _binary_mode = v & 1;
    let i = (v >> 6) as usize & 3;
    let read_mode = (v >> 4) & 3;

    if i >= NUM_COUNTERS || i == 3 {
        // read-back command (counter 3) not implemented
        return;
    }

    let ctr = &mut pit.counters[i];

    if read_mode == READ_MODE_LATCH {
        // Latch current value
        ctr.latch = LATCH_HIGH;
        let val = get_counter_value(ctr, unsafe { js::microtick() });
        ctr.latch_value = if val > 0 { val - 1 } else { 0 };
        return;
    }

    // Modes 6 and 7 are aliased to 2 and 3
    let mode = if mode >= 6 { mode & !4 } else { mode };

    ctr.mode = mode;
    ctr.read_mode = read_mode;
    ctr.next_low = read_mode != READ_MODE_MSB;

    if i == 0 {
        lower_irq();
    }

    match mode {
        0 => { /* interrupt on terminal count — handled in pit_timer */ }
        2 | 3 => { /* rate generator / square wave */ }
        _ => { /* unimplemented */ }
    }
}

/// Port 0x61: read speaker gate + counter 2 output
pub fn port61_read() -> u32 {
    let now = unsafe { js::microtick() };
    let pit = get_pit();
    let ref_toggle = ((now * (1000.0 * 1000.0 / 15000.0)) as u64 & 1) as u32;
    let counter2_out = if did_rollover(&pit.counters[2], now) { 1 } else { 0 };
    ref_toggle << 4 | counter2_out << 5
}

/// Port 0x61: write speaker gate
pub fn port61_write(_v: u8) {
    // Speaker enable/disable is handled by the bus; for now this is a no-op.
    // The JS code sends "pcspeaker-enable"/"pcspeaker-disable" bus messages.
}

// ---------------------------------------------------------------------------
// Internal counter read/write
// ---------------------------------------------------------------------------

fn counter_read(i: usize) -> u32 {
    let mut pit = get_pit();
    let ctr = &mut pit.counters[i];

    if ctr.latch != LATCH_NONE {
        ctr.latch -= 1;
        return if ctr.latch == LATCH_LOW {
            (ctr.latch_value & 0xFF) as u32
        } else {
            (ctr.latch_value >> 8) as u32
        };
    }

    let now = unsafe { js::microtick() };

    // Mode 3 toggles next_low on each read
    if ctr.mode == 3 {
        ctr.next_low = !ctr.next_low;
    }

    let value = get_counter_value(ctr, now);

    if ctr.next_low {
        (value & 0xFF) as u32
    } else {
        (value >> 8) as u32
    }
}

fn counter_write(i: usize, value: u8) {
    let now = unsafe { js::microtick() };
    let mut pit = get_pit();
    let ctr = &mut pit.counters[i];

    if ctr.next_low {
        ctr.reload = (ctr.reload & !0xFF) | value as u16;
    } else {
        ctr.reload = (ctr.reload & 0xFF) | ((value as u16) << 8);
    }

    if ctr.read_mode != READ_MODE_LSB_MSB || !ctr.next_low {
        if ctr.reload == 0 {
            ctr.reload = 0xFFFF;
        }
        ctr.start_value = ctr.reload;
        ctr.enabled = true;
        ctr.start_time = now;
    }

    if ctr.read_mode == READ_MODE_LSB_MSB {
        ctr.next_low = !ctr.next_low;
    }
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
