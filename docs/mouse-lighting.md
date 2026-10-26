# Combined keyboard and mouse lighting

Since SIE 0.9.0, **Vypnout osvětlení klávesnice a myši** requests darkness for
both devices. The action becomes **Zapnout osvětlení klávesnice a myši**.
The existing check/cross marks the selected mode. Separate keyboard and mouse
status rows report success, absence or failure; failure of one adapter never
prevents the other from applying the request. Dictation and workspace controls
remain available.

## Supported mouse and protocol

The connected mouse is **Logitech G203 LIGHTSYNC**, USB `046d:c092` (also marketed
as G102 LIGHTSYNC). SIE uses its existing Linux hidraw vendor-control interface,
not OpenRazer, an input-event node or the pointer interface. No new daemon,
firmware, Python runtime dependency or driver is installed.

Discovery checks the exact USB ID, the known 7/20-byte HID++ vendor report
collections, the RGB Effects feature `0x8071` version 0, and its single Logo
cluster. Before each request, an ioctl checks the USB identity of the opened
file descriptor again. The RGB power-mode query must succeed before any lighting
change. Unsupported interfaces and ambiguous multiple matching mice are refused.

The adapter looks up feature indices rather than hardcoding their position.
Only three feature functions are used:

- `getInfo` (`0x00`) for capability discovery.
- `manageSWControl` (`0x50`) to claim/release volatile LED control.
- `manageRGBPowerMode` (`0x80`) to set/query FullRgb (1) or PowerOff (3).

Off claims software LED/power control and selects PowerOff, then reads back the
power and control state. On selects FullRgb while software control is held,
then releases control to the device so its saved lighting effect resumes. An
already device-controlled mouse is left alone at startup. No effect parameters,
onboard profiles, DPI, report rate, buttons, calibration or EEPROM are written.

Sources reviewed for protocol behavior: [libratbag device support](
https://github.com/libratbag/libratbag/blob/master/data/devices/logitech-g102-g203.device),
[libratbag RGB discovery](https://github.com/libratbag/libratbag/blob/master/src/hidpp20.c),
[Solaar LED control](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py),
and the [OpenLogi RGB Effects reference](https://openlogi.org/hidpp/features/x8071-rgb-effects).
SIE's implementation is original Rust code using the protocol, not a copied driver.

## Local access

SIE runs as the desktop user. Install the narrowly scoped rule once:

```sh
sudo install -Dm644 packaging/70-sie-logitech-mouse.rules \
  /etc/udev/rules.d/70-sie-logitech-mouse.rules
sudo udevadm control --reload-rules
```

Reconnect the mouse, or retrigger only its matching hidraw vendor-control node.
The rule imports `path_id` before matching USB interface 1 (before `73-seat-late`
applies ACLs). `uaccess` grants the active local desktop session access to that
interface only. It does not grant access to the pointer interface or other
Logitech products. The regular user installer prints a hint if the rule is
missing/different; it never runs sudo automatically. `sie --doctor` probes the
mouse capabilities and reports software-control/RGB-power state without changing
lighting. A missing mouse is optional; permission/protocol errors are reported.

On the development workstation, the rule was applied to the detected control
node only. The desktop user could open that node afterwards, while the pointer
hidraw node remained inaccessible. No logout or USB removal was needed.

## Lifecycle and errors

Mouse control has its own asynchronous worker and a separate watch channel for
On/Off requests. It does not subscribe to keyboard recording animation, media
or workspace frames. Unchanged successful modes issue no further USB requests;
two-second checks only validate the existing node or retry missing/failed devices.
Reconnects apply the current requested mode, including Off.

Each HID++ reply wait is bounded to 750 ms and correlated by feature, function,
device and software ID. Non-vendor reports are discarded without parsing their
payload or logging them. The program never grabs input, parses pointer/button
events or records ordinary typing. Protocol errors are logged only when changed;
the tray retains a working keyboard toggle and reports mouse unavailability.

Quit retains the chosen lighting state, including dark mode. Starting SIE again
starts with lighting On, releasing any prior software-off state. Tray-only mode
does not open or control either device. The choice is scoped to the application
run; this feature does not add persistent preferences.

## Validation

Isolated tests use fake HID++ exchanges, temporary descriptor directories and a
fake worker backend. They cover supported-interface selection, malformed/missing
capabilities, correlated/error replies, read-back mismatch, deduplication,
release after restart and recovery from missing/unresponsive devices. They never
open actual input devices or use the user's desktop. Existing private D-Bus
tests cover keyboard matrix validation, dark mode during workspace changes and
recording animation/shutdown.

Separately, the actual mouse accepted the commands and read back PowerOff (3),
then FullRgb (1) and device control (0). The onboard profile mode stayed unchanged.
After deployment, exercise the combined tray action and visually check that both
devices darken and that the mouse's normal lighting effect returns on enable.
