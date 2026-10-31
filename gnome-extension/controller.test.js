import assert from 'node:assert/strict';
import test from 'node:test';
import {Controller, IDLE_TIMEOUT_MS} from './controller.js';
import {confirmPopup} from './confirmation.js';

function fixture(workspaceOnly = false) {
    const timers = new Map();
    const popups = [];
    let next = 0;
    const controller = new Controller({
        createPopup(options) {
            const popup = {
                options,
                shown: true, position: 1, confirmed: 0, canceled: 0,
                onClosed(callback) { this.closed = callback; },
                onActivity(callback) { this.activity = callback; },
                show() { return this.shown; },
                move(delta) { this.position = (this.position + delta + 50) % 5; this.activity(); },
                confirm() { this.confirmed++; this.closed(); },
                cancel() { this.canceled++; this.closed(); },
            };
            popups.push(popup);
            return popup;
        },
        schedule(delay, callback) {
            assert.equal(delay, IDLE_TIMEOUT_MS);
            const id = ++next;
            timers.set(id, () => { timers.delete(id); callback(); });
            return id;
        },
        unschedule(id) { timers.delete(id); },
        workspaceOnly: () => workspaceOnly,
    });
    return {controller, timers, popups};
}

test('native selection moves both directions and confirms only on click', () => {
    const {controller:c, popups, timers} = fixture();
    const token = c.open(':1.5');
    assert.equal(c.active(':1.5', token), true);
    c.move(':1.5', token, 2);
    c.move(':1.5', token, -1);
    assert.equal(popups[0].position, 2);
    assert.equal(popups[0].confirmed, 0);
    assert.equal(c.finish(':1.5', token, true), true);
    assert.equal(popups[0].confirmed, 1);
    assert.equal(c.active(':1.5', token), false);
    assert.equal(timers.size, 0);
});

test('local selection forwards its first delta and works with the workspace-only preference', () => {
    for (const delta of [-16, -1, 1, 3, 16]) {
        const {controller:c, popups} = fixture(true);
        const token = c.open(':1.5', {local: true, delta});
        assert.deepEqual(popups[0].options, {local: true, delta});
        assert.equal(c.active(':1.5', token), true);
        assert.equal(popups[0].confirmed, 0);
        c.finish(':1.5', token, true);
        assert.equal(popups[0].confirmed, 1);
    }
    for (const delta of [0, 17, -17, NaN, 1.5]) {
        const {controller:c, popups} = fixture();
        assert.throws(() => c.open(':1.5', {local: true, delta}), /Invalid/);
        assert.equal(popups.length, 0);
    }
});

test('local mode keeps ownership, context cancellation and idle cleanup', () => {
    for (const reason of ['context', 'idle', 'disconnect']) {
        const {controller:c, popups, timers} = fixture();
        const token = c.open(':1.5', {local: true, delta: -1});
        assert.throws(() => c.open(':1.6', {local: true, delta: 1}), /another client/);
        assert.equal(c.finish(':1.6', token, true), false);
        c.move(':1.5', token, 1);
        if (reason === 'context') popups[0].closed();
        else if (reason === 'idle') [...timers.values()][0]();
        else c.ownerGone(':1.5');
        assert.equal(c.active(':1.5', token), false);
        assert.equal(popups[0].confirmed, 0);
        assert.equal(timers.size, 0);
    }
});

test('dial confirmation outside an input event uses fresh compositor time to activate the selected workspace', () => {
    const {controller:c, popups} = fixture();
    const token = c.open(':1.5');
    c.move(':1.5', token, 2);
    const popup = popups[0];
    const lastUserTime = 9000;
    let freshTime = 9001;
    let requested = 0;
    let focusedWorkspace = 0;
    let received = 0;
    const native = {
        _finish(timestamp) {
            received = timestamp;
            // Model GNOME's focus-stealing guard and the selected window.
            if (timestamp >= lastUserTime)
                focusedWorkspace = popup.position;
        },
    };
    const display = {get_current_time_roundtrip() { requested++; return freshTime; }};
    // Establish the old failure: closing with CURRENT_TIME does not activate.
    native._finish(0);
    assert.equal(focusedWorkspace, 0);
    popup.confirm = () => confirmPopup(native, display);
    freshTime = 9010; // Obtain time at confirmation, not when the strip opened.
    c.finish(':1.5', token, true);
    assert.equal(requested, 1);
    assert.equal(received, 9010);
    assert.equal(focusedWorkspace, 3);
});

test('invalid timestamps and native activation errors cancel and clear ownership/timers', () => {
    for (const timestamp of [0, -1, NaN, 0x100000000, 42]) {
        const {controller:c, popups, timers} = fixture();
        const token = c.open(':1.5');
        popups[0].confirm = () => confirmPopup({
            _finish() {
                assert.equal(timestamp, 42, 'Invalid timestamps must not reach GNOME');
                throw new Error('Selected window has closed');
            },
        }, {get_current_time_roundtrip: () => timestamp});
        assert.throws(() => c.finish(':1.5', token, true));
        assert.equal(popups[0].canceled, 1);
        assert.equal(c.active(':1.5', token), false);
        assert.equal(timers.size, 0);
    }
});

test('native Escape and inactivity never request an activation timestamp', () => {
    for (const finish of ['escape', 'idle', 'cancel']) {
        const {controller:c, popups, timers} = fixture();
        const token = c.open(':1.5');
        popups[0].confirm = () => assert.fail('Cancellation must not activate a window');
        if (finish === 'escape') popups[0].closed();
        else if (finish === 'idle') [...timers.values()][0]();
        else c.finish(':1.5', token, false);
        assert.equal(c.active(':1.5', token), false);
    }
});

test('eight-second inactivity cancels without activating; activity resets it', () => {
    const {controller:c, timers, popups} = fixture();
    const token = c.open(':1.5');
    const initial = [...timers.keys()][0];
    c.move(':1.5', token, 1);
    assert.equal(timers.has(initial), false);
    popups[0].activity();
    assert.equal(timers.size, 1);
    [...timers.values()][0]();
    assert.equal(popups[0].confirmed, 0);
    assert.equal(popups[0].canceled, 1);
    assert.equal(c.active(':1.5', token), false);
});

test('native Escape/mouse dismissal clears state without a second confirmation', () => {
    const {controller:c, popups, timers} = fixture();
    const token = c.open(':1.5');
    popups[0].closed();
    assert.equal(c.active(':1.5', token), false);
    assert.equal(c.finish(':1.5', token, true), false);
    assert.equal(popups[0].confirmed, 0);
    assert.equal(timers.size, 0);
});

test('foreign clients and stale tokens cannot control another popup', () => {
    const {controller:c, popups} = fixture();
    const first = c.open(':1.5');
    assert.throws(() => c.open(':1.6'), /another client/);
    assert.equal(c.finish(':1.6', first, true), false);
    const second = c.open(':1.5');
    assert.equal(popups[0].canceled, 1);
    assert.equal(c.move(':1.5', first, 1), false);
    assert.equal(c.finish(':1.5', first, true), false);
    assert.equal(c.active(':1.5', second), true);
    assert.throws(() => c.move(':1.5', second, 17), /Invalid/);
    popups[0].closed(); // Delayed signal from a previous instance.
    assert.equal(c.active(':1.5', second), true);
});

test('client disconnect and extension shutdown cancel instead of committing', () => {
    for (const action of ['disconnect', 'disable']) {
        const {controller:c, popups, timers} = fixture();
        const token = c.open(':1.5');
        c.ownerGone(':1.6');
        assert.equal(c.active(':1.5', token), true);
        if (action === 'disconnect') c.ownerGone(':1.5');
        else c.close(false);
        assert.equal(popups[0].canceled, 1);
        assert.equal(popups[0].confirmed, 0);
        assert.equal(timers.size, 0);
    }
});

test('missing applications/modal refusal and workspace-only preference fail cleanly', () => {
    const scoped = fixture(true);
    assert.throws(() => scoped.controller.open(':1.5'), /current-workspace-only/);
    assert.equal(scoped.popups.length, 0);
    const {controller:c, popups, timers} = fixture();
    const create = c._createPopup;
    c._createPopup = () => {
        const popup = create();
        popup.shown = false;
        return popup;
    };
    assert.throws(() => c.open(':1.5'), /could not open/);
    assert.equal(popups[0].canceled, 1);
    assert.equal(timers.size, 0);
    c._createPopup = () => { throw new Error('Desktop is locked or busy'); };
    assert.throws(() => c.open(':1.5'), /locked or busy/);
    assert.equal(c.active(':1.5', 1), false);
    assert.equal(timers.size, 0);
});
