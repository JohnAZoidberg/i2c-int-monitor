# i2c-int-monitor

A terminal-based interrupt rate monitor for I2C controllers and HID devices on Linux.

## Overview

This tool monitors interrupt rates from `/proc/interrupts`, focusing on I2C-related sources:

- **I2C Controller interrupts** (e.g., `i2c_designware.N`) - These are affected by RX FIFO threshold optimizations in the DesignWare I2C driver
- **I2C HID device interrupts** (GPIO interrupts from touchpad/touchscreen) - One interrupt per HID report, determined by device firmware

The tool automatically discovers the I2C HID device topology from sysfs and identifies:
- Which controller each HID device is attached to
- Device types (Touchpad, Touchscreen, Sensor Hub, etc.)
- Vendor/product IDs and driver information

This helps diagnose excess interrupt activity that may prevent CPU deep idle states and cause increased power consumption.

## Installation

```bash
cargo build --release
sudo cp target/release/i2c-int-monitor /usr/local/bin/
```

## Usage

### List detected devices and topology

```bash
sudo i2c-int-monitor list
```

Shows the I2C HID device hierarchy with controllers and their attached devices:

```
=== I2C HID Device Topology ===

i2c_designware.1 [bus 1] (IRQ 28)
  FRMW0005:00 - Sensor Hub [32AC:001B] (IRQ 200)
  FRMW0004:00 - Keyboard/Controls [32AC:0006] (IRQ 201)

i2c_designware.5 [bus 5] (IRQ 21)
  PIXA3854:00 - Touchpad [093A:0274] (IRQ 203)
```

### Text-mode monitoring

```bash
sudo i2c-int-monitor monitor --interval 1000 --count 10
```

Options:
- `--interval, -i` - Sampling interval in milliseconds (default: 1000)
- `--count, -n` - Number of samples, 0 for unlimited (default: 0)
- `--threshold, -t` - Rate threshold for "HIGH" alerts (default: 100 irqs/s)

### TUI dashboard

```bash
sudo i2c-int-monitor tui
```

Interactive dashboard with real-time charts showing interrupt rates over time.

Options:
- `--interval, -i` - Sampling interval in milliseconds (default: 1000)
- `--threshold, -t` - Rate threshold for highlighting (default: 100 irqs/s)

#### TUI keybindings

| Key | Action |
|-----|--------|
| `q` / `Esc` | Quit |
| `j` / `Down` | Select next source |
| `k` / `Up` | Select previous source |
| `Space` | Toggle visibility of selected source |

The TUI shows:
- Controllers with their attached HID devices in a hierarchical view
- Consistent colors between the chart and the table for easy identification
- Real-time interrupt rates, averages, and maximums

## Background

On Intel platforms with DesignWare I2C controllers, active touchpad use can cause excessive interrupt rates. A typical 30-byte I2C HID report at 137 Hz can generate ~4,000 I2C controller interrupts per second when the RX FIFO threshold is set to 0 (one interrupt per byte).

The [RX FIFO threshold optimization](https://lore.kernel.org/linux-i2c/) in the DesignWare I2C driver reduces this by batching bytes, changing the ratio from ~30:1 to ~4:1 (controller IRQs per HID IRQ).

This tool helps measure the effectiveness of such optimizations by showing:
- Raw interrupt rates per source
- Ratio between controller and HID device interrupts
- Historical trends via the TUI chart

## Requirements

- Linux with `/proc/interrupts` and `/sys/bus/i2c/`
- Root privileges (to read interrupt counts and sysfs)
- I2C HID device (touchpad, touchscreen) or I2C controller present

## License

MIT
