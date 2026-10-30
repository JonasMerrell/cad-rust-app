# Test firmware for the Raspberry Pi Pico simulation

Small Pico SDK programs that the RP2040 emulator's tests run (`src/mcu_rp2040.rs`,
`src/circuit_mcu.rs`) and that the `pico-*.json` examples load. The `.uf2` files are
builds of these sources; any RP2040 `.uf2` or `.elf` runs the same way.

| Program | What it does |
| --- | --- |
| `blink` | the on-board LED (GP25), 250 ms on, 250 ms off |
| `button` | GP15 as an input with its pull-up; GP16 follows it inverted (an LED that lights while a button to ground is pressed) |
| `adc` | reads ADC 0 (GP26) every 10 ms, sets GP16's PWM duty from it and prints the reading on UART0 |
| `pwm` | 1 kHz PWM on GP0 (25 %) and GP1 (50 %) |
| `hello` | `printf` with integers, floats and doubles on UART0 every 100 ms |
| `multicore` | core 1 doubles numbers core 0 sends it through the FIFO, toggling GP17 |
| `irq` | a GPIO edge interrupt on GP14 toggles GP18; a 500 µs repeating timer toggles GP19 |
| `arm` | homes an arm: runs a motor (GP15) until a limit switch (GP14) closes, printing the angle a potentiometer on GP26 reads; then sweeps a servo on GP16 from 1 ms to 2 ms pulses |

To rebuild (with `arm-none-eabi-gcc`, CMake and the Pico SDK 2.x):

    PICO_SDK_PATH=/path/to/pico-sdk cmake -B build -G Ninja -DPICO_BOARD=pico
    ninja -C build
    cp build/*.uf2 .

The sources are under this project's licence; the builds contain the Pico SDK (BSD-3-Clause,
Copyright Raspberry Pi Ltd).
