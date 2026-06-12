# IR Blaster Wiring on Jetson Orin Nano

This guide covers wiring an IR LED to the Jetson Orin Nano's GPIO header,
installing LIRC, and registering an IR device in GIAP so that the AI can
control legacy appliances (TVs, ACs, audio receivers) over infrared.

---

## Hardware bill of materials

| Part | Value | Notes |
|---|---|---|
| IR LED | TSAL6400 or similar 940 nm | Higher power = longer range |
| NPN transistor | 2N2222 or BC547 | Any general-purpose NPN |
| Base resistor | 1 kΩ | Limits base current from GPIO |
| LED current resistor | 33–47 Ω | Sets peak drive current |
| Jumper wires | — | Female-to-male for 40-pin header |
| Breadboard or PCB | — | |

---

## Circuit

```
Jetson 40-pin header
   Pin 2 or 4 (5 V) ─────┬─── 33 Ω ─── IR LED (+) ─── IR LED (-) ─── NPN Collector
                          │
                         GND ──────────────────────────────────────── NPN Emitter

   GPIO pin (e.g. GPIO12)─── 1 kΩ ─── NPN Base
```

In plain terms:

1. **GPIO → 1 kΩ → Base (B)** of 2N2222
2. **Emitter (E)** of 2N2222 → GND (pin 6, 9, 14, 20, 25, 30, 34, or 39)
3. **Collector (C)** of 2N2222 → **IR LED cathode (−)**
4. **IR LED anode (+)** → **33 Ω resistor** → **5 V rail** (pin 2 or 4)

When the GPIO pin is driven HIGH, the transistor saturates and pulls current
through the LED. With 5 V supply and a 33 Ω resistor the peak current is
approximately 115 mA — well within the LED's pulse rating for the short bursts
that LIRC's 38 kHz modulation uses.

### Jetson Orin Nano 40-pin header GPIO numbering

The header is compatible with the Raspberry Pi pinout. LIRC's `gpio-ir-tx`
driver uses the **BCM (Broadcom) GPIO number**, not the physical pin number.

Recommended choices that are unshared on the Orin Nano:

| Physical pin | BCM GPIO | Notes |
|---|---|---|
| 12 | GPIO18 | Often used for RPi IR — good default |
| 32 | GPIO12 | Available on Orin Nano |
| 33 | GPIO13 | Available on Orin Nano |

Use `sudo gpioinfo` to verify a pin is not already claimed by another driver.

---

## Software setup

### 1. Install LIRC

```bash
sudo apt update
sudo apt install lirc
```

### 2. Enable the gpio-ir-tx kernel module

Create `/etc/modprobe.d/gpio-ir-tx.conf`:

```
options gpio_ir_tx gpio_nr=18
```

Replace `18` with your BCM GPIO number.

Load the module:

```bash
sudo modprobe gpio_ir_tx gpio_nr=18
```

Verify it created a lirc device:

```bash
ls /dev/lirc*
# Expected: /dev/lirc0
```

To load automatically on boot, add to `/etc/modules`:

```
gpio_ir_tx
```

### 3. Configure lircd

Edit `/etc/lirc/lirc_options.conf`:

```ini
[lircd]
driver  = default
device  = /dev/lirc0
```

### 4. Add remote control codes

**Option A — Use an existing config from the LIRC database**

```bash
# Browse at https://sourceforge.net/p/lirc-remotes/code/
sudo wget -O /etc/lirc/lircd.conf.d/samsung_tv.lircd.conf \
    "https://sourceforge.net/p/lirc-remotes/code/HEAD/tree/remotes/samsung/...conf?format=raw"
```

**Option B — Learn codes from your original remote**

Put lircd in learning mode and point your original remote at the IR receiver
(a separate component — the IR LED here is transmit-only):

```bash
sudo systemctl stop lircd
sudo irrecord -d /dev/lirc0 /etc/lirc/lircd.conf.d/my_tv.lircd.conf
sudo systemctl start lircd
```

Follow the on-screen prompts. The key names you assign during learning
(e.g. `KEY_POWER`, `KEY_VOLUMEUP`) must match what GIAP sends via `irsend`.

### 5. Start lircd

```bash
sudo systemctl enable --now lircd
sudo systemctl status lircd   # should show "active (running)"
ls /var/run/lirc/lircd        # socket created by lircd — GIAP checks for this
```

### 6. Test with irsend

```bash
# List available remotes and their keys
irsend LIST "" ""
irsend LIST SAMSUNG_TV ""

# Send a test keypress
irsend SEND_ONCE SAMSUNG_TV KEY_POWER
```

You should see the IR LED flicker and the TV toggle.

---

## Registering the device in GIAP

Use the GIAP device registration API or the `pond-server` REST endpoint.

**Example — register a Samsung TV:**

```bash
curl -X POST http://localhost:8080/api/devices \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Living Room TV",
    "device_type": "tv",
    "transport": "lirc",
    "address": "SAMSUNG_TV",
    "capabilities": ["ir"],
    "structured_capabilities": [
      {"key": "power",        "kind": "Toggle"},
      {"key": "volume_up",    "kind": "Toggle"},
      {"key": "volume_down",  "kind": "Toggle"},
      {"key": "mute",         "kind": "Toggle"},
      {"key": "input",        "kind": {"Enum": {"options": ["hdmi1","hdmi2","hdmi3","av"]}}},
      {"key": "channel_up",   "kind": "Toggle"},
      {"key": "channel_down", "kind": "Toggle"},
      {"key": "menu",         "kind": "Toggle"}
    ]
  }'
```

The `address` field is the LIRC remote name — it must match the name inside
your `.lircd.conf` file (the `begin remote` / `name` field).

---

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `IR_LIRC_SOCKET` | `/var/run/lirc/lircd` | Path to lircd socket (used for hardware detection) |
| `IR_LIRC_REMOTE` | _(none)_ | Optional default remote name when device has no address |
| `IR_KEY_REPEAT_DELAY_MS` | `80` | Milliseconds between repeated keypresses (volume steps) |
| `IR_TEST_REMOTE` | `TEST_REMOTE` | Remote name used by integration tests |

---

## Telling the AI to control the TV

Once the device is registered and lircd is running, you can simply say to GIAP:

> "Turn off the living room TV"
> "Mute the TV"
> "Turn the volume up five steps"

The `giap-control` MCP tool calls `set_device_state` → `IrDeviceController::set_state`
→ `irsend SEND_ONCE SAMSUNG_TV KEY_POWER` (or whatever key maps to the request).

---

## Key name mapping

GIAP maps friendly names to LIRC key names automatically:

| GIAP key | LIRC key |
|---|---|
| `power` | `KEY_POWER` |
| `volume_up` | `KEY_VOLUMEUP` |
| `volume_down` | `KEY_VOLUMEDOWN` |
| `mute` | `KEY_MUTE` |
| `input` | `KEY_INPUT` |
| `channel_up` | `KEY_CHANNELUP` |
| `channel_down` | `KEY_CHANNELDOWN` |
| `menu` | `KEY_MENU` |
| `home` | `KEY_HOME` |
| `back` | `KEY_BACK` |
| `hdmi1` | `KEY_HDMI1` |
| `hdmi2` | `KEY_HDMI2` |

For any key not in the table, GIAP passes it through unchanged — so you can
use exact LIRC key names (e.g. `KEY_SLEEP`) as capability keys in your device
registration.

### Volume steps

Pass a number as the value to repeat a key press:

> "Turn up the volume by 5"

This calls `set_state("volume_up", Number(5.0))` which sends `KEY_VOLUMEUP`
five times with 80 ms between presses (configurable via `IR_KEY_REPEAT_DELAY_MS`).

---

## Troubleshooting

**lircd won't start / no /dev/lirc0**
- Check `dmesg | grep gpio_ir` — the module may have failed to load
- Verify the GPIO pin number matches what you wired
- Try `sudo modprobe gpio_ir_tx gpio_nr=<pin>` manually

**irsend times out**
- Confirm `/var/run/lirc/lircd` socket exists: `ls -la /var/run/lirc/`
- Check lircd is running: `systemctl status lircd`

**TV doesn't respond to irsend**
- Check your `.lircd.conf` remote name matches what you pass to `irsend LIST`
- Verify the IR LED is oriented correctly (longer leg = anode = to resistor)
- Test the LED with a phone camera (IR shows as purple/white in camera preview)
- Increase LED drive current by reducing the series resistor (min 22 Ω with 5 V)

**GIAP reports "IR/LIRC hardware not available"**
- The adapter checks `/var/run/lirc/lircd` at startup — ensure lircd is running
  *before* starting pond-server
- Or set `IR_LIRC_SOCKET` to a custom path if your lircd uses a non-default socket
