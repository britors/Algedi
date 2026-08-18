"""Algedi status badges and context menu for Nautilus (Files).

A thin client only: all sync state and actions come from the algedid D-Bus
service (org.lyraos.Algedi1). This extension never duplicates sync logic.
See PROMPT-ALGEDI.md sec. 7.
"""

import time

import gi

gi.require_version("Nautilus", "4.0")
from gi.repository import Nautilus, GObject, Gio, GLib

BUS_NAME = "org.lyraos.Algedi1"
OBJECT_PATH = "/org/lyraos/Algedi1"

# TTL curto para evitar sobrecarregar o daemon ao listar pastas grandes
# (PROMPT-ALGEDI.md sec. 7, nota de implementacao).
STATUS_CACHE_TTL_SECONDS = 2.0

STATUS_EMBLEMS = {
    "synced": "algedi-status-synced",
    "syncing": "algedi-status-syncing",
    "conflict": "algedi-status-conflict",
    "paused": "algedi-status-paused",
}

PROVIDER_LABELS = {
    "gdrive": "Google Drive",
    "onedrive": "OneDrive",
}


class AlgediStatusCache:
    """Caches GetFileStatus results for STATUS_CACHE_TTL_SECONDS per path."""

    def __init__(self):
        self._entries = {}  # path -> (status, expires_at)

    def get(self, path):
        entry = self._entries.get(path)
        if entry is None:
            return None
        status, expires_at = entry
        if time.monotonic() > expires_at:
            return None
        return status

    def set(self, path, status):
        self._entries[path] = (status, time.monotonic() + STATUS_CACHE_TTL_SECONDS)

    def invalidate(self, path):
        self._entries.pop(path, None)


class AlgediDBusClient:
    def __init__(self):
        self._proxy = None
        self._cache = AlgediStatusCache()
        self._connect()

    def _connect(self):
        try:
            self._proxy = Gio.DBusProxy.new_for_bus_sync(
                Gio.BusType.SESSION,
                Gio.DBusProxyFlags.NONE,
                None,
                BUS_NAME,
                OBJECT_PATH,
                BUS_NAME,
                None,
            )
            # Emblemas atualizam via sinal, nao por polling do lado da
            # extensao (checklist do PROMPT-ALGEDI.md).
            self._proxy.connect("g-signal", self._on_signal)
        except GLib.Error:
            self._proxy = None

    def _on_signal(self, _proxy, _sender, signal_name, params):
        if signal_name == "StatusChanged":
            path, _status = params.unpack()
            self._cache.invalidate(path)

    def get_file_status(self, path):
        cached = self._cache.get(path)
        if cached is not None:
            return cached

        if self._proxy is None:
            return "unknown"

        try:
            (status,) = self._proxy.call_sync(
                "GetFileStatus",
                GLib.Variant("(s)", (path,)),
                Gio.DBusCallFlags.NONE,
                500,
                None,
            ).unpack()
        except GLib.Error:
            status = "unknown"

        self._cache.set(path, status)
        return status

    def sync_now(self, pair_id):
        self._call("SyncNow", GLib.Variant("(s)", (pair_id,)))

    def pause_sync(self, pair_id):
        self._call("PauseSync", GLib.Variant("(s)", (pair_id,)))

    def resume_sync(self, pair_id):
        self._call("ResumeSync", GLib.Variant("(s)", (pair_id,)))

    def get_pair_for_path(self, path):
        """Returns (pair_id, account_id, provider, paused), or None if the
        call failed. `pair_id` is empty when no pair owns `path`."""
        if self._proxy is None:
            return None
        try:
            result = self._proxy.call_sync(
                "GetPairForPath",
                GLib.Variant("(s)", (path,)),
                Gio.DBusCallFlags.NONE,
                500,
                None,
            )
        except GLib.Error:
            return None
        return result.unpack()

    def _call(self, method, params):
        if self._proxy is None:
            return
        try:
            self._proxy.call_sync(method, params, Gio.DBusCallFlags.NONE, 500, None)
        except GLib.Error:
            pass


_client = AlgediDBusClient()


class AlgediInfoProvider(GObject.GObject, Nautilus.InfoProvider):
    def update_file_info(self, file):
        path = file.get_location().get_path()
        if path is None:
            return Nautilus.OperationResult.COMPLETE

        status = _client.get_file_status(path)
        emblem = STATUS_EMBLEMS.get(status)
        if emblem:
            file.add_emblem(emblem)

        return Nautilus.OperationResult.COMPLETE


class AlgediMenuProvider(GObject.GObject, Nautilus.MenuProvider):
    def get_file_items(self, files):
        # Pair-level actions (sync/pause) only make sense for a single,
        # unambiguous selection.
        if len(files) != 1:
            return []

        path = files[0].get_location().get_path()
        if path is None:
            return []

        pair = _client.get_pair_for_path(path)
        if pair is None:
            return []

        pair_id, _account_id, provider, paused = pair
        if not pair_id:
            # Not inside any synced folder pair: no Algedi actions apply.
            return []

        items = []

        sync_item = Nautilus.MenuItem(
            name="Algedi::sync_now",
            label="Sincronizar agora",
            tip="Forca uma sincronizacao imediata deste item",
        )
        sync_item.connect("activate", lambda _item: _client.sync_now(pair_id))
        items.append(sync_item)

        if paused:
            resume_item = Nautilus.MenuItem(
                name="Algedi::resume",
                label="Retomar sincronizacao desta pasta",
                tip="Retoma a sincronizacao a partir de onde parou",
            )
            resume_item.connect("activate", lambda _item: _client.resume_sync(pair_id))
            items.append(resume_item)
        else:
            pause_item = Nautilus.MenuItem(
                name="Algedi::pause",
                label="Pausar sincronizacao desta pasta",
                tip="Pausa a sincronizacao ate ser retomada manualmente",
            )
            pause_item.connect("activate", lambda _item: _client.pause_sync(pair_id))
            items.append(pause_item)

        # TODO: "Ver no <provider>" (PROVIDER_LABELS) volta quando algedid
        # expuser resolucao de remote_id/URL — depende do fluxo OAuth ainda
        # nao implementado nos adapters gdrive/onedrive.

        return items

    def get_background_items(self, folder):
        return []
