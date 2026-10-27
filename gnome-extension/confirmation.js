// D-Bus callbacks have no current input event. Ask the compositor for a fresh
// timestamp, just as the native switcher's own no-modifier completion does.
export function confirmPopup(popup, display) {
    const timestamp = display.get_current_time_roundtrip();
    if (!Number.isInteger(timestamp) || timestamp <= 0 || timestamp > 0xffffffff)
        throw new Error('No valid compositor timestamp for switcher confirmation');
    popup._finish(timestamp);
}
