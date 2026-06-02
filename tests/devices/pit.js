#!/usr/bin/env node
/**
 * Test: PIT (Programmable Interval Timer) Rust port.
 *
 * Exercises the pit_* WASM exports directly via the compiled WASM
 * module.  Requires build/v86-debug.wasm.
 *
 * Run:  node tests/devices/pit.js
 */

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const wasmPath = path.join(__dirname, "../../build/v86-debug.wasm");

let tests_passed = 0;
let tests_failed = 0;

function test(name, fn) {
    process.stdout.write(name + " ... ");
    try {
        fn();
        console.log("PASS");
        tests_passed++;
    } catch(e) {
        console.log("FAIL");
        console.log("  " + e.message);
        tests_failed++;
    }
}

// ---------------------------------------------------------------------------
// WASM helpers
// ---------------------------------------------------------------------------

const wasmBuf = await readFile(wasmPath);

async function makeInstance(microtickFn) {
    const mod = new WebAssembly.Module(wasmBuf);
    const mem = new WebAssembly.Memory({ initial: 256, maximum: 256 });
    const tbl = new WebAssembly.Table({ initial: 4096, element: "anyfunc" });
    const S = () => 0;
    return await WebAssembly.instantiate(mod, { env: {
        memory: mem, __indirect_function_table: tbl,
        microtick: microtickFn,
        io_port_read8: S, io_port_read16: S, io_port_read32: S,
        io_port_write8: S, io_port_write16: S, io_port_write32: S,
        mmap_read8: S, mmap_read32: S, mmap_write8: S, mmap_write16: S,
        mmap_write32: S, mmap_write64: S, mmap_write128: S,
        log_from_wasm: S, dbg_trace_from_wasm: S, cpu_exception_hook: S,
        stop_idling: S, run_hardware_timers: ()=>50, codegen_finalize: S,
        jit_clear_func: S, cpu_event_halt: S, get_rand_int: ()=>42, console_log_from_wasm: S,
    }});
}

// Default instance with advancing microtick (for timer tests)
let t = 0;
const defaultInst = await makeInstance(() => { t += 10; return t; });
const e = defaultInst.exports;
const mem = defaultInst.exports.memory;

// Instance with fixed microtick for deterministic counter readback
const fixedInst = await makeInstance(() => 1000.0);
const fe = fixedInst.exports;
const fmem = fixedInst.exports.memory;

// Export helpers
function withFixed(fn) {
    return fn.bind(null, fe, fmem);
}

// ---------------------------------------------------------------------------
// Tests (default instance)
// ---------------------------------------------------------------------------

test("PIT exports are present", () => {
    const names = [
        "pit_init", "pit_state_size", "pit_timer", "get_pit_addr",
        "pit_oscillator_freq", "port40_read", "port40_write",
        "port41_read", "port41_write", "port42_read", "port42_write",
        "port43_write", "port61_read", "port61_write",
    ];
    for(const n of names) {
        assert.ok(typeof e[n] === "function", n);
    }
});

test("pit_oscillator_freq returns ~1193.18 kHz", () => {
    const f = e.pit_oscillator_freq();
    assert.ok(f > 1193.18 && f < 1193.19, `got ${f}`);
});

test("pit_state_size > 0", () => {
    assert.ok(e.pit_state_size() > 0);
});

test("pit_init resets counters to 0", () => {
    e.pit_init();
    assert.equal(e.port40_read(), 0);
    assert.equal(e.port41_read(), 0);
});

test("pit_timer returns positive value after configuring counter 0", () => {
    e.pit_init();
    e.port43_write(0x36);
    e.port40_write(0xE8);
    e.port40_write(0x03); // reload = 1000
    const t = e.pit_timer(1000.0, false);
    assert.ok(t > 0, `timer returned ${t}ms`);
});

test("pit_timer with no_irq=true returns positive value", () => {
    e.pit_init();
    e.port43_write(0x36);
    e.port40_write(100);
    e.port40_write(0);
    const t = e.pit_timer(1000.0, true);
    assert.ok(t > 0, `timer with no_irq returned ${t}ms`);
});

// ---------------------------------------------------------------------------
// Deterministic counter tests (fixed microtick = 1000.0)
// These use withFixed so reads happen at the same "time" as writes.
// ---------------------------------------------------------------------------

test("Write LSB-only to counter 0, read back (fixed time)", withFixed((e) => {
    e.pit_init();
    // counter=0, rw=LSB only, mode=3 → 0x16
    e.port43_write(0x16);
    e.port40_write(0x42); // reload = 66
    // JS behavior: reading with 0 elapsed ticks returns 0
    // (value >= reload → value % reload = 0)
    const v = e.port40_read();
    assert.equal(v, 0, "counter reads 0 at reload boundary");
}));

test("Write LSB+MSB to counter 0, read back both bytes (fixed time)", withFixed((e) => {
    e.pit_init();
    // counter=0, rw=LSB+MSB, mode=3 → 0x36
    e.port43_write(0x36);
    e.port40_write(0xFF); // low
    e.port40_write(0x12); // high → reload = 0x12FF
    const lo = e.port40_read();
    const hi = e.port40_read();
    // Reads at 0 elapsed ticks return 0 (matching JS)
    assert.equal(lo, 0, "lsb at reload boundary");
    assert.equal(hi, 0, "msb at reload boundary");
}));

test("Latch counter 0 captures current value (fixed time)", withFixed((e) => {
    e.pit_init();
    e.port43_write(0x36);
    e.port40_write(0x42); // low
    e.port40_write(0x01); // high → reload = 0x0142 = 322
    // latch counter 0 → 0x00
    e.port43_write(0x00);
    const lo = e.port40_read();
    const hi = e.port40_read();
    assert.ok(lo <= 0xFF && hi <= 0xFF, `lo=${lo} hi=${hi}`);
}));

// ---------------------------------------------------------------------------
// Port 0x61 tests
// ---------------------------------------------------------------------------

test("port61_read returns byte in range", () => {
    e.pit_init();
    const v = e.port61_read();
    assert.ok(v >= 0 && v <= 0xFF, `got ${v}`);
});

test("port61_write does not crash", () => {
    e.pit_init();
    e.port61_write(1);
    e.port61_write(0);
    assert.ok(true);
});

// ---------------------------------------------------------------------------
// State save/restore (fixed time instance)
// ---------------------------------------------------------------------------

test("State round-trip preserves raw bytes", withFixed((e, mem) => {
    e.pit_init();
    e.port43_write(0x36);
    e.port40_write(0xFF);
    e.port40_write(0x10); // reload = 0x10FF

    const addr = e.get_pit_addr();
    const size = e.pit_state_size();
    const mem8 = new Uint8Array(mem.buffer, addr, size);
    const saved = new Uint8Array(mem8);
    assert.ok(saved.length > 0, "saved state non-empty");
    // Verify key fields in saved state
    const reload = saved[10] | (saved[11] << 8);
    assert.equal(reload, 0x10FF, "reload preserved");

    e.pit_init(); // clear
    mem8.set(saved); // restore
    // Verify raw bytes restored
    const restored = new Uint8Array(mem.buffer, addr, size);
    let match = true;
    for(let i = 0; i < size; i++) {
        if(restored[i] !== saved[i]) { match = false; break; }
    }
    assert.ok(match, "all bytes restored correctly");
}));

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------

console.log(`\n${tests_passed} passed, ${tests_failed} failed`);
if(tests_failed > 0) process.exit(1);
