// Native popup ownership and timeout policy, independent of GJS for testing.
export const IDLE_TIMEOUT_MS = 8000;

export class Controller {
    constructor({createPopup, schedule, unschedule, workspaceOnly}) {
        this._createPopup = createPopup;
        this._schedule = schedule;
        this._unschedule = unschedule;
        this._workspaceOnly = workspaceOnly;
        this._popup = null;
        this._owner = null;
        this._token = 0;
        this._timer = null;
    }

    open(owner) {
        if (this._popup && this._owner !== owner)
            throw new Error('The native switcher belongs to another client');
        this.close(false);
        if (this._workspaceOnly())
            throw new Error('GNOME app-switcher current-workspace-only must be false');
        const popup = this._createPopup();
        this._token = (this._token + 1) >>> 0 || 1;
        this._popup = popup;
        this._owner = owner;
        popup.onClosed(() => {
            if (this._popup === popup)
                this._clear();
        });
        popup.onActivity(() => {
            if (this._popup === popup)
                this._touch();
        });
        try {
            if (!popup.show()) {
                this.close(false);
                throw new Error('GNOME could not open the native application switcher');
            }
            this._touch();
            return this._token;
        } catch (error) {
            this.close(false);
            throw error;
        }
    }

    active(owner, token) {
        return this._popup !== null && this._owner === owner && this._token === token;
    }

    move(owner, token, delta) {
        if (!this.active(owner, token))
            return false;
        if (!Number.isInteger(delta) || Math.abs(delta) > 16)
            throw new Error('Invalid dial step count');
        this._popup.move(delta);
        this._touch();
        return true;
    }

    finish(owner, token, confirm) {
        if (!this.active(owner, token))
            return false;
        this.close(confirm);
        return true;
    }

    ownerGone(owner) {
        if (owner === this._owner)
            this.close(false);
    }

    close(confirm) {
        const popup = this._popup;
        this._clear();
        if (popup) {
            if (confirm)
                popup.confirm();
            else
                popup.cancel();
        }
    }

    _clear() {
        if (this._timer !== null)
            this._unschedule(this._timer);
        this._timer = null;
        this._popup = null;
        this._owner = null;
    }

    _touch() {
        if (this._timer !== null)
            this._unschedule(this._timer);
        this._timer = this._schedule(IDLE_TIMEOUT_MS, () => {
            this._timer = null;
            this.close(false);
        });
    }
}
