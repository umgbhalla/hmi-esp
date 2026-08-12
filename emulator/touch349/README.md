# Touch349 V2 full UI emulator

This is a self-contained browser emulator for the exact 172x640 portrait UI.
It does not connect to, flash, or control physical hardware.

Open it with:

```sh
./scripts/open-touch349-emulator.sh
```

The device surface supports pointer and touch input. The exact inner viewport
is 172x640 pixels. Use the developer console to load deterministic hardware
scenarios, inject faults, inspect state, download a state snapshot, and copy a
deep link. The scenarios use a fixed virtual clock and seeded signals. Settings
and touch calibration persist in browser storage.

Modeled flows include boot, home, recorder, live audio, files, WAV player, text
viewer, diagnostics, five-point touch calibration, network/time, settings,
mounted-empty, missing, full, and failed SD states, audio failure, recording
failure, long-press shutdown, cancellation, save-before-off, USB and battery
off states, and wake.

Add `?capture=1&scenario=healthy&view=home` to show only the native device
surface. This mode is used to create the README screenshots.

The emulator is deliberately separate from the firmware and from the existing
pixel-output simulator. It is a product-design and interaction-validation tool;
approved behavior can later be implemented against the board adapter.
