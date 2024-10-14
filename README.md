# Sign Firmware
This is the firmware for the Sign. It features Lightning Time and self-updating firmware.

## Important
- There are credetials stored in the GitHub action secrets that need to be updated whenever the `.env` file is updated.

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
