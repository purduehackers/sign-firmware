# Sign Firmware
This is the firmware for the Sign. It features Lightning Time, self-updating firmware, and user-scriptable LED animations via [Rhai](https://rhai.rs).

Monitor the status of connected signs at https://sign.purduehackers.com.

## Repository layout
- The root crate is the ESP32 firmware (xtensa, esp-idf, `esp` toolchain).
- `crates/script-env` is the shared Rhai environment: engine construction, sandbox limits, and the script-facing API (`set_block`, `set_all`, `sleep`, `sleep_until`, `millis`, `hsv`, `rand_*`, `lightning_time`). It is compiled into both the firmware and the validator so the two can never disagree. Test it on the host with `RUSTUP_TOOLCHAIN=stable cargo test -p script-env --target <host-triple>`.
- `crates/validator` builds `script-env` to WASM (`wasm-pack build --target nodejs --scope purduehackers`) as `@purduehackers/sign-script-validator`, which [api-v4](https://github.com/purduehackers/api-v4) calls to reject broken scripts at upload time. It is excluded from the workspace and pins the stable toolchain.

## Scripting
Scripts arrive over the api-v4 WebSocket (`set_script`/`clear_script`), run on a dedicated 32KB-stack thread, and drive the five LED blocks imperatively — a script owns its own loop and paces itself with `sleep`/`sleep_until`. When no script runs (or a script errors or finishes), the sign renders Lightning Time. Nothing is persisted on the device: the server re-pushes the stored script on every connect. Push one with `PUT /sign/:device/script`.

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
