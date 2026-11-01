import assert from 'node:assert/strict';
import test from 'node:test';
import {scopePopup} from './scope.js';

function fixture(initialDelta = 1) {
    const origin = {};
    const other = {};
    let current = origin;
    const window = workspace => ({
        workspace, live: true, skip_taskbar: false,
        get_workspace() { return this.workspace; },
        get_compositor_private() { return this.live ? {} : null; },
    });
    const first = window(origin);
    const second = window(origin);
    const remote = window(other);
    const third = window(origin);
    const icon = windows => ({
        cachedWindows: windows,
        app: {disconnected: false, disconnectObject() { this.disconnected = true; }},
    });
    const mixed = icon([remote, first, second]);
    const remoteOnly = icon([remote]);
    const localOnly = icon([third]);
    const icons = [mixed, remoteOnly, localOnly];
    const list = {
        _arrows: icons.map(() => ({hidden: false, hide() { this.hidden = true; }})),
        _removeIcon(app) {
            const index = icons.findIndex(i => i.app === app);
            icons.splice(index, 1);
            this._arrows.splice(index, 1);
        },
    };
    const popup = {
        _switcherList: list, _items: icons, _selectedIndex: 0, _currentWindow: -1,
        activated: [], destroyed: false,
        _select(index) { this._selectedIndex = index; this._currentWindow = -1; },
        _finish(timestamp) {
            this.activated.push({window: this._items[this._selectedIndex].cachedWindows[Math.max(0, this._currentWindow)], timestamp});
        },
        destroy() { this.destroyed = true; },
    };
    const scope = scopePopup(popup, origin, () => current, first, initialDelta);
    return {popup, scope, origin, other, first, second, remote, third, remoteOnly,
        setWorkspace: value => { current = value; }};
}

test('native icons and their cached windows both exclude other workspaces', () => {
    const {popup, scope, first, second, third, remoteOnly} = fixture();
    assert.equal(popup._items.length, 2);
    assert.deepEqual(popup._items.map(i => i.cachedWindows), [[first, second], [third]]);
    assert.deepEqual(scope.windows, [first, second, third]);
    assert.equal(remoteOnly.app.disconnected, true);
    assert.equal(popup._switcherList._arrows[1].hidden, true);
    assert.equal(popup.activated.length, 0);
});

test('the first detent selects in both directions with wrapping, never activating', () => {
    for (const [delta, expected] of [[1, 1], [-1, 1], [2, 0], [-2, 0], [3, 1]]) {
        const {popup} = fixture(delta);
        popup._initialSelection(false, 'switch-applications');
        assert.equal(popup._selectedIndex, expected);
        assert.deepEqual(popup.activated, []);
    }
});

test('confirmation uses the local cached window and preserves the fresh timestamp', () => {
    for (const thumbnail of [-1, 1]) {
        const {popup, first, second} = fixture();
        popup._currentWindow = thumbnail;
        popup._finish(12345); // Same guard is used by the dial, Enter and mouse.
        assert.deepEqual(popup.activated, [{window: thumbnail === -1 ? first : second, timestamp: 12345}]);
    }
});

test('workspace changes, moved/closed/hidden windows and stale selections cannot activate', () => {
    for (const change of ['workspace', 'moved', 'closed', 'hidden', 'app', 'thumbnail']) {
        const {popup, first, other, setWorkspace} = fixture();
        if (change === 'workspace') setWorkspace(other);
        if (change === 'moved') first.workspace = other;
        if (change === 'closed') first.live = false;
        if (change === 'hidden') first.skip_taskbar = true;
        if (change === 'app') popup._selectedIndex = 99;
        if (change === 'thumbnail') popup._currentWindow = 99;
        popup._finish(12345);
        assert.deepEqual(popup.activated, []);
        assert.equal(popup.destroyed, true);
    }
});

test('empty local scope and a focus outside the strip are safe', () => {
    const {popup, origin} = fixture();
    const scope = scopePopup(popup, origin, () => origin, null);
    scope.select(1);
    assert.equal(popup._selectedIndex, 0);
    scope.select(-1);
    assert.equal(popup._selectedIndex, popup._items.length - 1);
    const empty = scopePopup(popup, {}, () => origin, null);
    assert.equal(popup._items.length, 0);
    empty.select(1);
    assert.deepEqual(popup.activated, []);
});
