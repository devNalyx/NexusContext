'use strict';

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const POLL_INTERVAL_SECONDS = 15;

// Deliberately thin, per the proposal: a status icon + launcher only.
// Anything requiring real interaction (search, project management,
// config) lives in the GTK4 app, not here - Shell extensions that do
// real work are the most fragile part of this stack across GNOME
// version upgrades.
const NexusIndicator = GObject.registerClass(
class NexusIndicator extends PanelMenu.Button {
    _init() {
        super._init(0.0, 'NexusContext', false);

        this._icon = new St.Icon({
            icon_name: 'folder-symbolic',
            style_class: 'system-status-icon',
        });
        this.add_child(this._icon);

        this._statusItem = new PopupMenu.PopupMenuItem('Checking daemon status…', {
            reactive: false,
        });
        this.menu.addMenuItem(this._statusItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const openManagerItem = new PopupMenu.PopupMenuItem('Open NexusContext Manager');
        openManagerItem.connect('activate', () => this._launchGui());
        this.menu.addMenuItem(openManagerItem);

        this.menu.connect('open-state-changed', (_menu, isOpen) => {
            if (isOpen)
                this._refreshStatus();
        });

        this._refreshStatus();
        this._timeoutId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, POLL_INTERVAL_SECONDS, () => {
            this._refreshStatus();
            return GLib.SOURCE_CONTINUE;
        });
    }

    _controlSocketPath() {
        const runtimeDir = GLib.getenv('XDG_RUNTIME_DIR');
        return runtimeDir ? `${runtimeDir}/nexuscontext/nexuscontext.sock` : null;
    }

    _refreshStatus() {
        const socketPath = this._controlSocketPath();
        if (!socketPath) {
            this._setDisconnected('XDG_RUNTIME_DIR is not set');
            return;
        }

        const address = Gio.UnixSocketAddress.new(socketPath);
        const client = new Gio.SocketClient();

        client.connect_async(address, null, (source, result) => {
            let connection;
            try {
                connection = source.connect_finish(result);
            } catch (err) {
                this._setDisconnected('nexusd not reachable - run `nexusd serve`');
                return;
            }

            const request = `${JSON.stringify({
                jsonrpc: '2.0',
                id: 1,
                method: 'status.get',
                params: {},
            })}\n`;

            try {
                connection.get_output_stream().write_all(request, null);
            } catch (err) {
                this._setDisconnected('failed to send request to nexusd');
                connection.close(null);
                return;
            }

            const input = new Gio.DataInputStream({ base_stream: connection.get_input_stream() });
            input.read_line_async(GLib.PRIORITY_DEFAULT, null, (stream, res) => {
                try {
                    const [line] = stream.read_line_finish_utf8(res);
                    const response = JSON.parse(line);
                    const result = response.result;
                    this._icon.icon_name = 'folder-symbolic';
                    this._statusItem.label.text =
                        `v${result.version} — ${result.projects_indexed} project(s) indexed`;
                } catch (err) {
                    this._setDisconnected('unexpected response from nexusd');
                } finally {
                    connection.close(null);
                }
            });
        });
    }

    _setDisconnected(message) {
        this._icon.icon_name = 'dialog-warning-symbolic';
        this._statusItem.label.text = message;
    }

    _launchGui() {
        try {
            Gio.Subprocess.new(['nexuscontext-gui'], Gio.SubprocessFlags.NONE);
        } catch (err) {
            logError(err, 'NexusContext: failed to launch nexuscontext-gui');
        }
    }

    vfunc_destroy() {
        if (this._timeoutId) {
            GLib.source_remove(this._timeoutId);
            this._timeoutId = null;
        }
        super.vfunc_destroy();
    }
});

export default class NexusContextExtension extends Extension {
    enable() {
        this._indicator = new NexusIndicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator);
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
