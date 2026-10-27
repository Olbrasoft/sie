import assert from 'node:assert/strict';
import test from 'node:test';
import {Controller, IDLE_TIMEOUT_MS} from './controller.js';

function fixture(workspaceOnly = false) {
    const timers = new Map();
    const popups = [];
    let next = 0;
    const controller = new Controller({
        createPopup() {
            const popup = {
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
            timers.set(++next, callback);
            return next;
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
