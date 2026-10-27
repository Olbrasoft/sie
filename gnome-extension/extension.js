// This adapter calls the installed GNOME 48 switcher; it does not copy its UI.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import Shell from 'gi://Shell';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as AltTab from 'resource:///org/gnome/shell/ui/altTab.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Controller} from './controller.js';

const BUS = 'cz.olbrasoft.SIE.Switcher';
const PATH = '/cz/olbrasoft/SIE/Switcher';
const XML = `<node><interface name="${BUS}">
  <method name="Open"><arg type="u" direction="out"/></method>
  <method name="Move"><arg type="u" direction="in"/><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
  <method name="Finish"><arg type="u" direction="in"/><arg type="b" direction="in"/><arg type="b" direction="out"/></method>
  <method name="Active"><arg type="u" direction="in"/><arg type="b" direction="out"/></method>
</interface></node>`;

function createPopup() {
    // Match the native Alt+Tab keybinding's normal-session action mode.
    if (Main.sessionMode.isLocked || Main.sessionMode.isGreeter ||
        Main.actionMode !== Shell.ActionMode.NORMAL)
        throw new Error('The native switcher requires the normal unlocked desktop');
    const popup = new AltTab.AppSwitcherPopup();
    let activity = () => {};
    // Use GNOME's no-modifier mode, with our cancellation-only idle timeout.
    // Changes apply only to this popup instance, never to Shell prototypes.
    popup._resetNoModsTimeout = () => activity();
    const select = popup._select.bind(popup);
    popup._select = (...args) => {
        select(...args);
        activity();
    };
    const keyHandler = popup._keyPressHandler.bind(popup);
    popup._keyPressHandler = (symbol, action) => {
        if (symbol === Clutter.KEY_Tab || symbol === Clutter.KEY_ISO_Left_Tab) {
            popup._select(symbol === Clutter.KEY_Tab ? popup._next() : popup._previous());
            return Clutter.EVENT_STOP;
        }
        return keyHandler(symbol, action);
    };
    return {
        onClosed(callback) { popup.connect('destroy', callback); },
        onActivity(callback) { activity = callback; },
        show() {
            const shown = popup.show(false, 'switch-applications', 0);
            if (shown)
                popup._showImmediately();
            return shown;
        },
        move(delta) {
            for (let i = 0; i < Math.abs(delta); i++)
                popup._select(delta > 0 ? popup._next() : popup._previous());
        },
        confirm() { popup._finish(global.get_current_time()); },
        cancel() { popup.destroy(); },
    };
}

export default class NativeSwitcherExtension extends Extension {
    enable() {
        this._settings = new Gio.Settings({schema_id: 'org.gnome.shell.app-switcher'});
        this._controller = new Controller({
            createPopup,
            workspaceOnly: () => this._settings.get_boolean('current-workspace-only'),
            schedule: (delay, callback) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, delay, () => {
                callback();
                return GLib.SOURCE_REMOVE;
            }),
            unschedule: id => GLib.source_remove(id),
        });
        this._impl = Gio.DBusExportedObject.wrapJSObject(XML, this);
        this._impl.export(Gio.DBus.session, PATH);
        this._name = Gio.DBus.session.own_name(BUS, Gio.BusNameOwnerFlags.NONE, null, null);
        this._ownerSignal = Gio.DBus.session.signal_subscribe(
            'org.freedesktop.DBus', 'org.freedesktop.DBus', 'NameOwnerChanged',
            '/org/freedesktop/DBus', null, Gio.DBusSignalFlags.NONE,
            (_connection, _sender, _path, _iface, _signal, params) => {
                const [name, , next] = params.deep_unpack();
                if (next === '')
                    this._controller?.ownerGone(name);
            });
        this._sessionSignal = Main.sessionMode.connect('updated', () => {
            if (Main.sessionMode.isLocked || Main.sessionMode.isGreeter)
                this._controller?.close(false);
        });
    }

    _reply(invocation, type, action) {
        try {
            invocation.return_value(new GLib.Variant(type, [action()]));
        } catch (error) {
            invocation.return_dbus_error(`${BUS}.Failed`, error.message);
        }
    }

    OpenAsync(_args, invocation) {
        this._reply(invocation, '(u)', () => this._controller.open(invocation.get_sender()));
    }

    MoveAsync([token, delta], invocation) {
        this._reply(invocation, '(b)', () => this._controller.move(invocation.get_sender(), token, delta));
    }

    FinishAsync([token, confirm], invocation) {
        this._reply(invocation, '(b)', () => this._controller.finish(invocation.get_sender(), token, confirm));
    }

    ActiveAsync([token], invocation) {
        this._reply(invocation, '(b)', () => this._controller.active(invocation.get_sender(), token));
    }

    disable() {
        this._controller.close(false);
        Main.sessionMode.disconnect(this._sessionSignal);
        Gio.DBus.session.signal_unsubscribe(this._ownerSignal);
        Gio.DBus.session.unown_name(this._name);
        this._impl.unexport();
        this._impl = null;
        this._controller = null;
        this._settings = null;
    }
}
