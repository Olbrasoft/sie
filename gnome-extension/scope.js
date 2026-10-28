// Per-popup workspace policy. No GNOME settings or shared prototypes are changed.
export function scopePopup(popup, workspace, currentWorkspace, focusedWindow, initialDelta) {
    const list = popup._switcherList;
    for (const icon of [...popup._items]) {
        icon.cachedWindows = icon.cachedWindows.filter(window =>
            window.get_workspace() === workspace && !window.skip_taskbar);
        if (icon.cachedWindows.length === 0) {
            icon.app.disconnectObject(list);
            list._removeIcon(icon.app);
        } else if (icon.cachedWindows.length === 1) {
            list._arrows[popup._items.indexOf(icon)].hide();
        }
    }

    // GNOME normally selects the next app in show(). Replace that initial step
    // so the first detent (including its direction/count) is applied only once.
    const select = delta => {
        const length = popup._items.length;
        if (length === 0)
            return;
        let index = popup._items.findIndex(icon => icon.cachedWindows.includes(focusedWindow));
        if (index < 0)
            index = delta > 0 ? -1 : 0;
        popup._select(((index + delta) % length + length) % length);
    };
    popup._initialSelection = () => select(initialDelta);
    const finish = popup._finish.bind(popup);
    popup._finish = timestamp => {
        const icon = popup._items[popup._selectedIndex];
        const window = icon?.cachedWindows[Math.max(0, popup._currentWindow)];
        if (currentWorkspace() !== workspace || !window || window.skip_taskbar ||
            window.get_workspace() !== workspace || !window.get_compositor_private()) {
            popup.destroy();
            return;
        }
        finish(timestamp);
    };

    return {
        windows: popup._items.flatMap(icon => icon.cachedWindows),
        select,
    };
}
