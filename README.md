# SpiderNotes

Local-first Markdown notes with peer-to-peer sync over your home network.
No cloud, no server: notes are plain `.md` files in a folder you choose, and
they sync directly between your devices.

> **Status: alpha.** v0.1.0 is the first alpha release — the core flow
> (notes, editor, P2P sync, Android background sync) works, but expect rough
> edges, possible data-loss footguns, and changing behavior. Back up your
> vault before trusting it with anything important.

## Features

- Plain Markdown files in a folder of your choice (a "vault") — the files
  stay readable without the app
- Editor with live Markdown preview (one click to raw source)
- `[[wikilinks]]`, `#tags`, backlinks
- Search, tag filter, and folders: create / rename / delete them, drag &
  drop notes between folders (right-click or long-press for the menu, edge
  swipes to show/hide the notes list)
- P2P sync between devices: mDNS discovery on the LAN plus an optional
  relay server for devices mDNS cannot find (different subnets, NAT, VPN);
  end-to-end-encrypted transport (Noise_XK) in both cases
- Manual trust: nothing syncs until you compare device IDs and trust each
  device on both sides
- Autosave with a configurable delay, dark/light themes
- Android background sync (foreground service, survives app backgrounding)

## Download

Prebuilt binaries live in the [GitHub Releases](https://github.com/MastMind/SpiderNotes/releases) page:

| Platform | Artifacts |
|----------|-----------|
| Linux    | `.deb` (x86_64, aarch64), `.AppImage` (x86_64) |
| Windows  | `.msi` + NSIS installer `.exe` (x86_64, i686), NSIS installer `.exe` (aarch64) |
| macOS    | `.dmg` (universal: Intel + Apple Silicon) |
| Android  | universal `.apk` (all ABIs) |
| Relay server | `spidernotes-server-linux-x86_64`, `spidernotes-server-linux-aarch64` |

On first run, point the app at a vault folder. An empty field uses the
default: `~/Documents/SpiderNotes` on desktop, the app's internal storage on
Android.

## Getting started with two devices

1. Open the same or different vaults on both devices.
2. On each device open **Network & log** (the gear button).
3. Under **Network** find the other device and press **Trust** — compare the
   device ID shown in the confirm box with the one on the other device.
4. Press **Sync with all trusted**, or just wait for the automatic sync
    interval (default 30 s, adjustable in **Advanced**).

## Getting started with the relay server (across subnets / NAT)

1. Run `spidernotes-server` on any always-on machine that both devices can
   reach (a home server, a Raspberry Pi, a VPS):
   ```
   spidernotes-server --host 0.0.0.0 --port 8788 --secret "a-long-random-secret"
   ```
   One global `--secret` guards the whole server. **Rooms are created by the
   clients**: the first device that joins a room name creates it, devices
   with the same room name see each other, and a room is deleted
   automatically when its last member leaves.
2. On each device open **Network & log** (the gear button) → **Server sync**:
   tick **Use sync server** and enter the server **Host**, **Port**, the
   **Room** name you want to be in, and the server **Secret**, then
   **Save & connect**.
3. Trust the other device (compare IDs) as usual, then sync — directly or
   through the server. The status line shows the room and peer count.

**Inspecting the server** — the control socket (default `127.0.0.1:8789`,
change with `--ctrl HOST:PORT`) is a line protocol for `nc`:
```
$ nc 127.0.0.1 8789
rooms
family: 2 member(s), created 12m 5s ago
members
family: phone [a1b2c3…] @ 192.168.1.110 — joined 12m ago, 42 frames, 12000 B
stats
connections 5 (auth failures 1), relayed 120 frames / 150000 B, ctrl queries 3
quit
```
**Logging** — joins/leaves/room lifecycle and auth failures are logged at
INFO (default); set `RUST_LOG=debug` for per-frame relay details.

The server is optional and stateless: it only routes end-to-end-encrypted
sync frames between room members. Anyone without the shared secret cannot
join any room (it is verified with an HMAC challenge and is never sent over
the wire). mDNS keeps working in parallel; if a device is reachable
directly, the direct connection is preferred.

## How it works

- **Vault** — a directory tree of `.md` files. Every note carries a stable
  `spidernotes-id` in its frontmatter; that ID is the note's identity for
  sync (the app restores it if you delete the line).
- **Discovery** — mDNS on the LAN. When mDNS cannot reach a device (different
  subnets, NAT, VPN), an optional **relay server** acts as a meeting point:
  devices pick a *room* name (the first joiner creates it) and prove the
  server's single shared secret, after which the server hands them each
  other's public keys. Direct connections always win when both paths exist.
- **Transport** — plain TCP + a `Noise_XK` handshake
  (X25519 + ChaCha20-Poly1305 + SHA-256). Every session is end-to-end
  encrypted and authenticated. Over the relay server the same encrypted
  frames are forwarded as opaque payloads — the server only relays ciphertext
  and never sees note content (the shared secret authenticates access via an
  HMAC challenge and is never transmitted).
- **Merge** — every note has a Hybrid Logical Clock (HLC) timestamp and a
  content hash. Conflicts resolve last-writer-wins per note; deletions are
  synced as tombstones.
- **Trust** — sync is explicit. Only mutually trusted device IDs exchange
  data; unknown devices can see you on the network but cannot sync.

Full protocol specification: [`docs/protocol.md`](docs/protocol.md).

## Security notes

- No third party ever sees your notes: sync is end-to-end encrypted. With the
  optional relay server, the server only forwards ciphertext between room
  members — it cannot read notes and never learns the room secret.
- Trust is by device ID — always compare the full ID on both screens before
  confirming.
- The vault folder and the device identity live on your machine only.

## Development

Prerequisites: Rust (stable), Node 22, pnpm 12, and the Tauri platform
dependencies (on Linux: `libwebkit2gtk-4.1-dev` and friends).

```sh
git clone https://github.com/MastMind/SpiderNotes.git
cd SpiderNotes/app
pnpm install
pnpm tauri dev          # run the desktop app
pnpm tauri build        # release bundles (deb/appimage/msi/nsis/dmg)
pnpm tauri android build --apk   # Android (needs Android SDK + NDK)
```

Checks:

```sh
cargo test                                   # core + p2p + relay unit tests
node app/scripts/check-live-preview.mjs      # editor decoration checks
node app/scripts/check-live-preview-browser.mjs  # needs chromium + `vite build`
node app/scripts/check-tree-browser.mjs      # folders/DnD/swipes browser checks
```

Layout:

| Path | What |
|------|------|
| `app/` | Tauri app: React + CodeMirror frontend, backend commands |
| `crates/core` | vault, HLC, LWW merge, identity, trust store |
| `crates/p2p` | mDNS discovery, Noise_XK transport, sync protocol, relay client |
| `crates/server` | `spidernotes-server` relay binary (room-based, E2E opaque) |
| `plugins/fgservice` | Android foreground-service plugin (background sync) |
| `docs/` | protocol specification |

## License

MIT
