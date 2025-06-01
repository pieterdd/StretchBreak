/* extension.js
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */

import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import St from 'gi://St';
import Clutter from 'gi://Clutter'

import { Extension, gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const DBUS_IFACE = `
<node>
    <interface name="io.github.pieterdd.StretchBreak.Core">
        <signal name="WidgetInfoUpdated">
            <arg type="s" />
        </signal>
        <method name="ToggleWindow"></method>
        <method name="MuteForMinutes">
            <arg type="x" direction="in" />
        </method>
        <method name="Unmute"></method>
        <method name="SetReadingMode">
            <arg type="b" direction="in" />
        </method>
    </interface>
</node>`;
const ProxyWrapper = Gio.DBusProxy.makeProxyWrapper(DBUS_IFACE);

function debugLog(...args) {
    args[0] = `[stretch-break] ${args[0]}`;
    console.log(...args);
}

class DBusClient {
    constructor(onServerDisconnectCallback, widgetInfoCallback) {
        this._onServerDisconnectCallback = onServerDisconnectCallback;
        this._widgetInfoCallback = widgetInfoCallback;
    }

    watch() {
        try {
            this._watchId = Gio.DBus.session.watch_name(
                "io.github.pieterdd.StretchBreak.Core",
                Gio.BusNameWatcherFlags.AUTO_START,
                this._onServerConnected.bind(this),
                this._onServerDisconnected.bind(this),
            )
            debugLog("Watching name");
        } catch (e) {
            console.error("Proxy could not connect to UI", e);
        }
    }

    _onServerConnected() {
        debugLog("Server process connected");

        this._proxy = ProxyWrapper(
            Gio.DBus.session,
            'io.github.pieterdd.StretchBreak.Core',
            '/io/github/pieterdd/StretchBreak/Core',
        );
        this._widgetInfoUpdatedSignal = this._proxy.connectSignal("WidgetInfoUpdated", this._widgetInfoCallback);
        debugLog('WidgetInfoUpdated signal connected', this._widgetInfoUpdatedSignal);
    }

    _onServerDisconnected() {
        debugLog("Server process disconnected");
        this._disconnectSignals();
        this._onServerDisconnectCallback();
    }

    _disconnectSignals() {
        if (!this._proxy) return;
        debugLog("Disconnecting signals");
        try {
            if (this._widgetInfoUpdatedSignal !== undefined) {
                this._proxy.disconnectSignal(this._widgetInfoUpdatedSignal);
                this._widgetInfoUpdatedSignal = undefined;
            }
        } catch (e) {
            debugLog('Signal disconnection failed or no longer needed', e);
        }
    }

    toggleWindow() {
        this._proxy.ToggleWindowSync();
    }

    unmute() {
        this._proxy.UnmuteSync();
    }

    muteForMinutes(numMinutes) {
        this._proxy.MuteForMinutesSync(numMinutes);
    }

    setReadingMode(value) {
        this._proxy.SetReadingModeSync(value);
    }

    unwatch() {
        if (this._watchId !== undefined) {
            debugLog('Unwatching DBus');
            this._disconnectSignals();
            Gio.DBus.session.unwatch_name(this._watchId);
            this._watchId = undefined;
        } else {
            debugLog('DBus already unwatched');
        }
    }
}

const Indicator = GObject.registerClass(
    class Indicator extends PanelMenu.Button {
        _init(extensionPath, dbusClient) {
            super._init(0.0, _('Stretch Break'));

            const box = new St.BoxLayout({ style_class: 'panel-status-indicators-box' });
            const gicon = Gio.icon_new_for_string(`${extensionPath}/logo-white.svg`);
            box.add_child(new St.Icon({
                gicon,
                width: 20,
                height: 20,
            }));
            this._normalLabel = new St.Label({
                visible: false,
                y_align: Clutter.ActorAlign.CENTER,
            });
            box.add_child(this._normalLabel);
            this._prebreakLabel = new St.Label({
                visible: false,
                y_align: Clutter.ActorAlign.CENTER,
                style_class: 'prebreakTimer',
            });
            box.add_child(this._prebreakLabel);
            this.add_child(box);

            const itemToggle = new PopupMenu.PopupMenuItem(_('Toggle window'));
            itemToggle.connect('activate', () => {
                dbusClient.toggleWindow();
            });

            this.menu.addMenuItem(itemToggle);

            this._readingModeSwitch = new PopupMenu.PopupSwitchMenuItem("Reading mode");
            this._readingModeSwitch.connect('toggled', (_, value) => {
                dbusClient.setReadingMode(value);
            });
            this.menu.addMenuItem(this._readingModeSwitch);

            this._modeSeparator = new PopupMenu.PopupSeparatorMenuItem("");

            this.menu.addMenuItem(this._modeSeparator);
            this._unmuteMenuItem = new PopupMenu.PopupMenuItem("Unmute");
            this._unmuteMenuItem.connect('activate', () => {
                dbusClient.unmute();
            });
            this.menu.addMenuItem(this._unmuteMenuItem);

            this._muteSubMenuItem = new PopupMenu.PopupSubMenuMenuItem("Mute for...");
            this._mute30mMenuItem = new PopupMenu.PopupMenuItem("30 minutes");
            this._mute30mMenuItem.connect('activate', () => {
                dbusClient.muteForMinutes(30);
            });
            this._muteSubMenuItem.menu.addMenuItem(this._mute30mMenuItem);
            this._mute60mMenuItem = new PopupMenu.PopupMenuItem("1 hour");
            this._mute60mMenuItem.connect('activate', () => {
                dbusClient.muteForMinutes(60);
            });
            this._muteSubMenuItem.menu.addMenuItem(this._mute60mMenuItem);
            this._mute6hMenuItem = new PopupMenu.PopupMenuItem("6 hours");
            this._mute6hMenuItem.connect('activate', () => {
                dbusClient.muteForMinutes(60*6);
            });
            this._muteSubMenuItem.menu.addMenuItem(this._mute6hMenuItem);
            this.menu.addMenuItem(this._muteSubMenuItem);
        }

        updateNormalLabel(text) {
            this._normalLabel.visible = !!text;
            this._normalLabel.text = text;
        }

        updatePrebreakLabel(text) {
            this._prebreakLabel.visible = !!text;
            this._prebreakLabel.text = text;
        }

        updateMuteStatus(mutedUntilTime) {
            if (mutedUntilTime) {
                this._normalLabel.style_class = 'muted';
                this._modeSeparator.label.text = `Muted until ${mutedUntilTime}`;
            } else {
                this._normalLabel.style_class = '';
                this._modeSeparator.label.text = '';
            }
            this._unmuteMenuItem.sensitive = !!mutedUntilTime;
        }

        updateReadingModeStatus(value) {
            this._readingModeSwitch.setToggleState(value);
        }
    });

export default class StretchBreakCompanionExtension extends Extension {
    _onWidgetInfoUpdated(_emitter, _senderName, rawWidgetInfo) {
        const widgetInfo = JSON.parse(rawWidgetInfo);
        if (this._indicator) {
            this._indicator.updateNormalLabel(widgetInfo.normal_timer_value);
            this._indicator.updatePrebreakLabel(widgetInfo.prebreak_timer_value);
            this._indicator.updateMuteStatus(widgetInfo.muted_until_time);
            this._indicator.updateReadingModeStatus(widgetInfo.reading_mode);
        }
    }

    _onServerDisconnected() {
        debugLog('Server disconnected hook triggered');
        this._indicator.updateNormalLabel('');
        this._indicator.updatePrebreakLabel('');
    }

    enable() {
        this._dbusClient = new DBusClient(
            this._onServerDisconnected.bind(this),
            this._onWidgetInfoUpdated.bind(this),
        );
        this._dbusClient.watch();
        this._indicator = new Indicator(this.path, this._dbusClient);
        Main.panel.addToStatusArea(this.uuid, this._indicator);
    }

    disable() {
        this._dbusClient.unwatch();
        this._dbusClient = null;
        this._indicator.destroy();
        this._indicator = null;
    }
}
