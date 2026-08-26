# Sign Firmware
This is the firmware for the Sign. It features Lightning Time, self-updating firmware, and user-scriptable LED animations via [Rhai](https://rhai.rs).

Monitor the status of connected signs at https://sign.purduehackers.com.

## Repository layout
- The root crate is the ESP32 firmware (xtensa, esp-idf, `esp` toolchain).
- `crates/script-env` is the shared Rhai environment: engine construction, sandbox limits, and the script-facing API (`set_block`, `set_all`, `sleep`, `sleep_until`, `millis`, `hsv`, `rand_*`, `lightning_time`). It is compiled into both the firmware and the validator so the two can never disagree. Test it on the host with `RUSTUP_TOOLCHAIN=stable cargo test -p script-env --target <host-triple>`.
- `crates/validator` builds `script-env` to WASM (`wasm-pack build --target nodejs --scope purduehackers`) as `@purduehackers/sign-script-validator`, which [api-v4](https://github.com/purduehackers/api-v4) calls to reject broken scripts at upload time. It is excluded from the workspace and pins the stable toolchain.

## Writing sign scripts

Scripts are written in [Rhai](https://rhai.rs/book/ref/index.html) and drive the sign's five LED blocks imperatively: a script owns its own loop and paces itself with `sleep`/`sleep_until`. Push one to every connected sign with a single request:

```bash
curl -X PUT https://api.purduehackers.com/sign/script \
  -H "Authorization: Bearer $PHACK_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"script": "loop { set_all(hsv(rand_float() * 360.0, 1.0, 0.5)); sleep(500); }"}'
```

The server compiles your script with the exact engine the firmware runs, so a broken script comes back as a `422` with the error, line, and column — nothing reaches the sign until it compiles. `GET /sign/script` reads the stored script, `DELETE /sign/script` returns the sign to Lightning Time early, and `GET /sign/status` counts connected signs.

### Lifecycle

A script is a moment, not a takeover. It runs for **at most 30 seconds** — or less, if it finishes on its own — and then the sign returns to Lightning Time, which is always the resting state. A script that hits a runtime error also falls back to Lightning Time. `sleep` cannot outlive the cap: sleeping past the deadline just ends the run.

### API

Blocks are addressed with the constants `TOP`, `CENTER`, `RIGHT`, `BOTTOM_LEFT`, `BOTTOM_RIGHT` (and `NUM_BLOCKS` is 5). Colors are `0..=255` per channel and are gamma-corrected by the firmware.

| Function | What it does |
|---|---|
| `set_block(block, r, g, b)` | Set one block's color (also accepts `set_block(block, [r, g, b])`) |
| `set_all(r, g, b)` | Set every block (also accepts `set_all([r, g, b])`) |
| `set_brightness(v)` | Global output scale, `0.0..=1.0`, applied to later `set_*` calls; resets to `1.0` each run |
| `hsv(h, s, v)` | Hue `0..360`, saturation/value `0..1` → an `[r, g, b]` array |
| `sleep(ms)` | Pause the script |
| `sleep_until(t)` | Pause until an absolute time — use this for drift-free animation loops |
| `millis()` | Milliseconds since the script started |
| `rand_float()` | Uniform `[0.0, 1.0)` from the hardware RNG |
| `rand_int(lo, hi)` | Uniform integer, inclusive on both ends |
| `rand_chance(p)` | `true` with probability `p` |
| `lightning_time()` | `#{ bolts, zaps, sparks, charges, colors: #{ bolt, zap, spark } }` — each color an `[r, g, b]` |

### Examples

A synchronized rainbow at 30fps (`sleep_until` keeps the period exact no matter how long each frame takes):

```rhai
let t = 0;
loop {
  set_all(hsv((t % 14400).to_float() / 40.0, 1.0, 0.6));
  sleep_until(t + 33);
  t += 33;
}
```

A five-second breathing pulse that ends itself:

```rhai
let t = 0;
while t < 5000 {
  let phase = t.to_float();
  let b = if phase < 2500.0 { phase / 2500.0 } else { (5000.0 - phase) / 2500.0 };
  set_brightness(b);
  set_all(255, 255, 255);
  sleep_until(t + 33);
  t += 33;
}
```

Lightning Time's own colors, remixed:

```rhai
let lt = lightning_time();
loop {
  set_all(lt.colors.bolt);
  sleep(300);
  set_all(lt.colors.zap);
  sleep(300);
  set_all(lt.colors.spark);
  sleep(300);
}
```

### Limits

Scripts are capped at 8KB of source and run sandboxed: max 16 call levels, 1024-element arrays, 4KB strings, 64-entry maps, no `eval`, and undefined variables are compile errors. There is no operation cap — the 30-second wall clock is the budget. Heavy per-frame work is fine; the interpreter comfortably does 30fps.

### How it works underneath

The server lowers accepted scripts to [grain](https://github.com/rhaiscript/rhai) bytecode via `@purduehackers/sign-script-validator` (a WASM build of this repo's own `script-env` crate) and broadcasts the verified artifact to every connected sign over WebSocket (`set_script`/`clear_script`); the device never parses script text. Each run gets a fresh engine on a dedicated 20KB-stack thread, guarded by a heap admission check — if the sign is low on memory the push is refused with an error rather than risking a reboot. Nothing is persisted on the device: the server re-pushes the stored script whenever a sign connects, and clears it once a sign reports the script finished.

## API
The sign speaks to api-v4 at `api.purduehackers.com`: an `auth` frame carrying `PHACK_API_KEY`, then request/reply frames for WiFi config and scripts, with an app-level ping keepalive and exponential-backoff reconnects. All connected signs share one identity and mirror the same content — every unit runs the identical binary with zero per-device configuration (api-v3's provisioning flow and per-device keys are gone). `SIGN_API_BASE` at build time points a bench sign at a different deployment.

## Important
- There are credentials stored in the GitHub action secrets that need to be updated whenever the `.env` file is updated. The `.env` file needs `PHACK_API_KEY` in addition to the WiFi credentials.

## Related Repos
- [Power Delivery Board](https://github.com/purduehackers/sign-pcb)
- [ESP to Pico Converter Board](https://github.com/purduehackers/EspToPico)

## Caveats and Workarounds
Reference the [ESP to Pico repo](https://github.com/purduehackers/EspToPico) for more details into hardware problems.

The current revision of the ESP to Pico PCB (rev 2) has some problems:
- While the EEPROM code works, the current revision of the ESP to Pico PCB
has a misconfigured line that prevents the EEPROM from being accessed. The ESP32 has enough flash that it can be used instead if needed.
- The button LED and switch lines are reversed in code compared to the Sign Mainboard since I accidentially assigned the LED to an input-only pin.
- I failed to use the correct serial chip that allows for automatic resets and programming. In order to program the PCB manually, you must:
  - Unplug the PCB
  - Hold down the wire-attached button
  - Plug in the PCB still holding the button
  - Wait a few seconds
  - Release the button
  - Attempt to program (`cargo run --release`)
    - If this fails, repeat the process
  - Once the PCB issue is fixed, the `runner` command in `.cargo/config.toml` will need to be reverted to the original.
