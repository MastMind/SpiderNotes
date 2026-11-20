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
- Search, tag filter, folder tree
- P2P sync between devices on the same network: mDNS discovery,
  end-to-end-encrypted transport (Noise_XK), no middleboxes
- Manual trust: nothing syncs until you compare device IDs and trust each
  device on both sides
- Autosave with a configurable delay, dark/light themes
- Android background sync (foreground service, survives app backgrounding)

## Download

Prebuilt binaries live in the [GitHub Releases](https://github.com/MastMind/SpiderNotes/releases) page:

| Platform | Artifacts |
|----------|-----------|
| Linux    | `.deb`, `.AppImage` |
| Windows  | `.msi`, NSIS installer `.exe` |
| macOS    | `.dmg` (universal: Intel + Apple Silicon) |
| Android  | universal `.apk` (all ABIs) |

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

## How it works

- **Vault** — a directory tree of `.md` files. Every note carries a stable
  `spidernotes-id` in its frontmatter; that ID is the note's identity for
  sync (the app restores it if you delete the line).
- **Discovery** — mDNS on the LAN; no fixed server addresses.
- **Transport** — plain TCP + a `Noise_XK` handshake
  (X25519 + ChaCha20-Poly1305 + SHA-256). Every session is end-to-end
  encrypted and authenticated; there is no relay or server in between.
- **Merge** — every note has a Hybrid Logical Clock (HLC) timestamp and a
  content hash. Conflicts resolve last-writer-wins per note; deletions are
  synced as tombstones.
- **Trust** — sync is explicit. Only mutually trusted device IDs exchange
  data; unknown devices can see you on the network but cannot sync.

Full protocol specification: [`docs/protocol.md`](docs/protocol.md).

## Security notes

- No third party ever sees your notes: the app only connects to peers it
  discovers via mDNS on your own network.
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
cargo test                                   # core + p2p unit tests
node app/scripts/check-live-preview.mjs      # editor decoration checks
node app/scripts/check-live-preview-browser.mjs  # needs chromium + `vite build`
```

Layout:

| Path | What |
|------|------|
| `app/` | Tauri app: React + CodeMirror frontend, backend commands |
| `crates/core` | vault, HLC, LWW merge, identity, trust store |
| `crates/p2p` | mDNS discovery, Noise_XK transport, sync protocol |
| `plugins/fgservice` | Android foreground-service plugin (background sync) |
| `docs/` | protocol specification |

## License

MIT
