# Sign Firmware
This is the firmware for the Sign. It features Lightning Time, self-updating firmware, and user-scriptable LED animations via [Rhai](https://rhai.rs).

Monitor the status of connected signs at https://sign.purduehackers.com.

## Repository layout
- The root crate is the ESP32 firmware (xtensa, esp-idf, `esp` toolchain).
- `crates/script-env` is the shared Rhai environment: engine construction, sandbox limits, and the script-facing API (`set_block`, `set_all`, `sleep`, `sleep_until`, `millis`, `hsv`, `rand_*`, `lightning_time`). It is compiled into both the firmware and the validator so the two can never disagree. Test it on the host with `RUSTUP_TOOLCHAIN=stable cargo test -p script-env --target <host-triple>`.
- `crates/validator` builds `script-env` to WASM (`wasm-pack build --target nodejs --scope purduehackers`) as `@purduehackers/sign-script-validator`, which [api-v4](https://github.com/purduehackers/api-v4) calls to reject broken scripts at upload time. It is excluded from the workspace and pins the stable toolchain.

## Writing sign scripts

Scripts are [Rhai](https://rhai.rs/book/ref/index.html): write a loop, set colors, sleep. Push to every connected sign:

```bash
curl -X PUT https://api.purduehackers.com/sign/script \
  -H "Authorization: Bearer $PHACK_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"script": "loop { set_all(hsv(rand_float() * 360.0, 1.0, 0.5)); sleep(500); }"}'
```

Broken scripts come back as a `422` with the error, line, and column — the server compiles with the same engine the firmware runs. `GET /sign/script` reads, `DELETE /sign/script` stops early, `GET /sign/status` counts signs.

A script runs **at most 30 seconds** (less if it ends itself), then the sign returns to Lightning Time. Runtime errors fall back the same way.

### API

Blocks: `TOP`, `CENTER`, `RIGHT`, `BOTTOM_LEFT`, `BOTTOM_RIGHT`. Colors are `0..=255`, gamma-corrected.

| Function | What it does |
|---|---|
| `set_block(block, r, g, b)` | One block (also takes `[r, g, b]`) |
| `set_all(r, g, b)` | Every block (also takes `[r, g, b]`) |
| `set_brightness(v)` | Scale output `0.0..=1.0`; resets to `1.0` each run |
| `hsv(h, s, v)` | Hue `0..360`, sat/val `0..1` → `[r, g, b]` |
| `sleep(ms)` | Pause |
| `sleep_until(t)` | Pause until an absolute time — drift-free loops |
| `millis()` | Ms since script start |
| `rand_float()` | `[0.0, 1.0)` |
| `rand_int(lo, hi)` | Inclusive both ends |
| `rand_chance(p)` | `true` with probability `p` |
| `lightning_time()` | `#{ bolts, zaps, sparks, charges, colors: #{ bolt, zap, spark } }` |

### Examples

Synchronized rainbow at 30fps:

```rhai
let t = 0;
loop {
  set_all(hsv((t % 14400).to_float() / 40.0, 1.0, 0.6));
  sleep_until(t + 33);
  t += 33;
}
```

Five-second breathing pulse that ends itself:

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

### Limits

8KB of source. Sandboxed: 16 call levels, 1024-element arrays, 4KB strings, 64-entry maps, no `eval`, undefined variables fail at compile. No operation cap — the 30-second clock is the budget, and 30fps is comfortable.

### Underneath

The server lowers scripts to grain bytecode (via `@purduehackers/sign-script-validator`, a WASM build of this repo's `script-env`) and broadcasts verified artifacts over WebSocket — the device never parses text. Each run gets a fresh engine behind a heap admission check: a low-memory sign refuses the push instead of rebooting. Nothing persists on the device; the server re-pushes on connect and clears once a sign reports the script done.

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
