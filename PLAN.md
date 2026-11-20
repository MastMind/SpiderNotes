# SpiderNotes — P2P-заметки без центрального сервера

Заметки в стиле Obsidian (plain Markdown, `[[wikilinks]]`, теги, вложения).
Синхронизация: **P2P внутри домашней сети (mDNS + зашифрованный канал)**,
опционально — **self-hosted сервер** для классической синхронизации (E2E-шифрование).

Кроссплатформенность: один Rust-core на всех платформах.
Приоритет: **Linux, Windows** → macOS → Android, iOS.

## Стек

| Слой | Технология | Почему |
|---|---|---|
| Оболочка | **Tauri 2** (Rust + WebView) | Linux/Windows/macOS/Android/iOS из одного кода, лёгкие бинарники |
| Core | **Rust** (`spidernotes-core`) | vault, md, HLC, merge, crypto — общий для всех платформ |
| P2P | `mdns-sd` (обнаружение) + TCP + `noise` (XK) | mDNS — стандарт LAN-обнаружения (как Chromecast/AirPrint); Noise — аутентифицированный шифрованный канал без сертификатов |
| Конфликты | **HLC (hybrid logical clock) + LWW на заметку + tombstones** | просто и предсказуемо; путь апгрейда — Automerge/Yjs для merge внутри заметки |
| Frontend | React + Vite + TypeScript + **CodeMirror 6** | Obsidian тоже на CodeMirror; wiki-автокомплит, подсветка md |
| Сервер | Rust: `axum` + `rustls`, бинарник `spidernotes-server` | один статичный бинарник, деплой на любой VPS/NAS |
| Шифрование | ed25519 (идентичность устройств), X25519 + Noise (канал), AES-GCM / XChaCha20-Poly1305 (данные на сервере), Argon2id (пароль сервера) | |

## Архитектура

```
┌────────────────────────────── Tauri app ─────────────────────────────┐
│  React/CodeMirror UI                                                 │
│    tree · editor · search · peers panel · sync status · settings     │
│  tauri commands (invoke)                                             │
└───────────────┬──────────────────────────────────────┬───────────────┘
                │                                       │
        ┌───────▼────────┐                     ┌────────▼─────────┐
        │ spidernotes-   │                     │ spidernotes-p2p  │
        │ core           │◄────────────────────┤  mDNS discovery  │
        │  Vault         │   merge/apply       │  Noise TCP       │
        │  Note/Id/Index │                     │  Sync protocol   │
        │  HLC · LWW     │                     └────────┬─────────┘
        │  md/frontmatter│                              │
        └───────┬────────┘                     ┌────────▼─────────┐
                │                              │  peers (LAN)     │
        vault dir (.md files)                  │  OR              │
                │                              │ spidernotes-     │
        ┌───────▼────────┐                     │ server (TLS)     │
        │  .spidernotes/ │                     └──────────────────┘
        │  index.json    │
        └────────────────┘
```

### Модель vault
- Vault = каталог с `.md` файлами (совместим с Obsidian: обычный markdown).
- Каждая заметка имеет стабильный **UUID**, хранится в YAML frontmatter:
  ```yaml
  ---
  spidernotes-id: 6f1c…
  ---
  ```
  Obsidian фронтmatter показывает как property, ничего не ломает.
  UUID нужен, чтобы переименование/перемещение файла не рвало идентичность.
- Локальный индекс `.spidernotes/index.json`:
  `{ notes: { "<uuid>": { path, hlc, contentHash, deleted } }, device: {...} }`
  — позволяет быстро определять «что изменилось» без полного rescana.

### Синхронизация P2P (LAN)
1. **Обнаружение**: каждый узел анонсирует `_spidernotes._tcp.local`
   с TXT-записями: `name` (имя устройства), `pub` (hex X25519 public key).
2. **Подключение**: клиент открывает TCP к найденному адресу,
   handshake **Noise_XK** (static peer key из mDNS — аутентификация без сертификатов).
3. **Протокол sync** (кадровый: len+type+payload, JSON):
   - `hello { deviceId, name, hlc, caps }` → `helloAck`
   - `syncRequest { sinceHlc, noteSummaries[] }` — «вот мой список заметок с HLC и хэшем»
   - peer отвечает `delta[]` — заметки, которые новее (по HLC/tiebreak deviceId)
   - `note { uuid, hlc, content|encrypted, deleted }` — полный контент заметки
   - `done` → взаимно (оба направления) → merge в индекс.
4. **Конфликты**: на заметку — LWW по `(hlc, deviceId)`; удаление — tombstone
   (tombstone тоже «заметка» и участвует в LWW). Конфликт редактирования
   одной заметки на двух устройствах → побеждает более свежий HLC;
   история потерь — принятый trade-off v1 (в планах CRDT merge).
5. **Авто-синхронизация**: при сохранении файла + периодический scan;
   новые peer по mDNS → автоматический обмен delta.

### Сервер (опционально, self-hosted)
- Бинарник `spidernotes-server --config config.toml`:
  ```toml
  listen = "0.0.0.0:8787"
  password = "…"            # Argon2id → мастер-ключ шифрования
  data_dir = "/var/lib/spidernotes"
  ```
  + TLS: свои сертификаты (auto self-signed при старте, либо свои PEM).
- **E2E**: клиент шифрует каждую заметку **ключом vault**
  (XChaCha20-Poly1305, ключ vault хранится локально у пользователя),
  сервер видит только `{ uuid, hlc, ciphertext, deleted }`.
  Пароль сервера — только для auth сессии (не для расшифровки данных).
- API (JSON over HTTPS):
  - `POST /v1/auth` → session token (пароль, Argon2id verify)
  - `GET  /v1/notes?since=…` → summaries
  - `PUT  /v1/notes/{uuid}` → `{ hlc, deleted, data(b64) }` (LWW-merge на сервере)
  - `GET  /v1/notes/{uuid}`, `DELETE /v1/notes/{uuid}`
  - `GET  /v1/peers` (кто сейчас онлайн — опционально, для «классики» не обязателен)
- Клиент sync-over-server = тот же merge-код из core (транспорт абстрагирован).

### Криптография (сводка)
| Что | Как |
|---|---|
| Идентичность устройства | ed25519 keypair в `~/.config/spidernotes/identity.key` |
| P2P-канал | X25519 (из ed25519-pair через SLH-derivation, либо отдельный x25519) + Noise_XK |
| Данные на сервере | XChaCha20-Poly1305 с vault key (E2E) |
| Auth на сервере | пароль → Argon2id → сравнение хэша; session token (HMAC) |

## Структура репозитория

```
SpiderNotes/
├── PLAN.md
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # spidernotes-core (lib)
│   │   └── src/{lib,vault,note,frontmatter,hlc,merge,identity,server_client}.rs
│   ├── p2p/                   # spidernotes-p2p (lib)
│   │   └── src/{lib,discovery,transport,protocol,agent}.rs
│   └── server/                # spidernotes-server (bin)
 │       └── src/{main,lib,api,store,tls,config}.rs + tests/server.rs
 ├── app/                       # Tauri 2 application
 │   ├── src-tauri/             # rust backend (tauri commands → core/p2p/server_client)
 │   │   └── src/{main,lib,state,commands}.rs
 │   ├── src/                   # React + Vite + CodeMirror 6
 │   │   └── {main.tsx,App.tsx,Editor.tsx,NotesTree.tsx,PeersPanel.tsx,api.ts,types.ts}
 │   └── scripts/gen-icon.mjs   # генерация 32x32 PNG-иконки
 ├── .sysdeps/                  # локальные dev-зависимости (deb, без root)
 │   ├── x/                     # распакованный префикс (include/lib/pkgconfig)
 │   ├── resolve.sh             # цикл: pkg-config → добор missing dev-пакетов
 │   └── env.sh                 # export PKG_CONFIG_PATH=...
 └── docs/protocol.md           # spec P2P-протокола (написать в фазе 3)
```

## Фазы

### Фаза 0 — каркас ✅
- workspace, core crate: vault/note/frontmatter/HLC/LWW-merge + юнит-тесты (26 тестов).

### Фаза 1 — рабочее приложение (Linux/Windows) ✅
- Tauri 2 + React/CodeMirror UI: открыть vault, дерево файлов, редактор md
  (markdown-подсветка, oneDark, autosave ~600ms), создание/переименование/
  удаление заметок, поиск по тексту (в Rust).
- Команды Tauri → core (13 команд). Linux-сборка без root через .sysdeps.

### Фаза 2 — P2P-синхронизация ✅
- mDNS discovery, Noise_XK TCP transport, sync protocol (delta по summaries),
  peers-panel в UI (список соседей, sync c одним / со всеми), loopback-тесты
  (2 «устройства», LWW-конфликт, распространение удаления; 8 тестов).
- Авто-синхронизация: таймер 30с + триггер по join пира; все синки под
  `sync_gate` (clone → sync → put-back без переплетений), pre-sync scan
  подхватывает внешние изменения файлов; IPv6-адреса (скобки) и таймаут
  соединения 5с; приоритет non-link-local адресам в discovery.
- Проверено E2E: два инстанса (разные identity), mDNS-обнаружение,
  конвергенция vault'ов, live-тест (файл на устройстве B → устройство A
  за один тик).

### Фаза 3 — сервер ✅ (ядро + клиент), интеграция в UI — кнопки на месте
- `spidernotes-server`: self-signed TLS (rcgen), argon2-auth, REST API,
  sqlite LWW store, E2E-тест через реальный TLS.
- server_client в core (XChaCha20-Poly1305, vault key) + панель в UI
  (URL, пароль, «синхронизировать с сервером»).

### Фаза 4 — полировка и платформы
- docs/protocol.md ✅.
- CLI: `--open <путь> [--порт N]` открывает vault при старте; команда
  `vault_status` восстанавливает состояние в UI.
- **wikilinks** ✅: `[[...]]` — автокомплит в редакторе (путь/бейзнейм, вставка
  `[[path]]`), подсветка ссылок, `resolve_link` (путь → basename → case-insensitive),
  backlinks-панель («Ссылаются»), теги `#тег` (панель с счётчиками, фильтр дерева).
- graph view (stretch), опц. шифрование локального vault; история версий (stretch).
- **Android** ✅: проект в `app/src-tauri/gen/android/` (Tauri 2.12 кладёт его
  в `gen/` — это исходники, не артефакты; в .gitignore исключён).
  `tauri::mobile_entry_point!()` в `lib.rs`. Манифест: `INTERNET`,
  `ACCESS_NETWORK_STATE`, `CHANGE_WIFI_MULTICAST_STATE` (mDNS). minSdk 24.
  Сборка: `cargo tauri android build --apk --target aarch64` (CI: job `android`,
  JDK 21 + SDK platform 37.1 + NDK 29.0.13846066).
  Ограничение v1: vault по умолчанию — app-specific каталог (scoped storage);
  произвольные пути (чужие Obsidian-vault'ы) потребуют SAF/fs-плагин.
- **Сборка**:
  - **deb готов** (`target/release/bundle/deb/SpiderNotes_0.1.0_amd64.deb`,
    depends: libwebkit2gtk-4.1-0, libgtk-3-0). AppImage в этой среде не
    собирается (linuxdeploy виснет — FUSE/сеть), собрать на обычном десктопе.
  - **Windows NSIS готов** ✅: кросс-сборка `x86_64-pc-windows-gnu` с Linux
    (mingw-w64 + wine/makensis): `cargo tauri build --target x86_64-pc-windows-gnu`
    → `dist/SpiderNotes_0.1.0_x64-setup.exe`. Установка проверена в wine
    (exit 0: exe + uninstaller + WebView2Loader.dll; WebView2-секция в wine
    фейкится — на реальных Windows она ставит/находит runtime).
  - **MSI**: tauri-bundler на Linux-хосте его не строит (gated
    `cfg(target_os="windows")`); собирается на Windows / CI
    (`.github/workflows/build-windows.yml`, `scripts/build-windows.ps1`).
  - macOS/Android/iOS (Tauri 2) — см. ниже.

## Принятые решения / trade-off
1. **LWW вместо CRDT в v1** — проще, предсказуемо, достаточно для личного
   использования; путь миграции: заменить payload на Automerge-документ,
   протокол и HLC сохраняются.
2. **UUID во frontmatter** — лёгкий след в файлах, зато идентичность живёт
   при rename и при переносе файла между устройствами.
3. **TCP+Noise вместо QUIC/WebRTC** — UDP в LAN не гарантирован; Noise даёт
   aEAD без инфраструктуры PKI. WebRTC оставим как будущий вариант
   для синхронизации через интернет (STUN/TURN).
4. **Сервер не видит plaintext** — E2E с vault key; пароль сервера только для
   доступа, а не для расшифровки.
5. UI **русский по умолчанию** (+ en).

## Следующие шаги (сейчас)
1. [x] PLAN.md
2. [x] cargo workspace + `crates/core` (vault, md, HLC, merge) + тесты → `cargo test`
3. [x] `crates/p2p`: mdns discovery + noise transport + protocol + loopback-тесты
4. [x] `crates/server`: axum + rustls + sqlite store + E2E crypto + тесты
5. [x] `app/`: Tauri + React + CodeMirror, дерево + редактор + sync-panel
6. [x] sysdeps (webkit2gtk-4.1, libsoup3, gtk3, pango, ...) → `cargo build` OK, окно поднимается
7. [x] `cargo tauri dev` с фронтендом (devUrl 1420) — проверить UI вживую
8. [x] авто-синхронизация по таймеру + docs/protocol.md (+ E2E на 2 инстансах)
9. [x] wikilinks (автокомплит+подсветка), backlinks, теги
10. [x] артефакты: deb (Linux) + NSIS (Windows, кросс-сборка с Linux)
11. [ ] MSI — на Windows/CI (`scripts/build-windows.ps1` / workflow)
12. [ ] macOS: кросс-сборка/CI (см. заметки ниже)
13. [ ] graph view, история версий, шифрование vault (stretch)

## CI/CD (GitHub Actions)
- `.github/workflows/ci.yml`: матрица `ubuntu-latest` (deb+AppImage),
  `windows-latest` (msi+nsis), `macos-latest` (app+dmg, unsigned).
  Триггеры: ручной запуск + тег `v*`. По тегу — job `release` собирает
  артефакты в GitHub Release (`softprops/action-gh-release`).
- Версия бандлов берётся из `app/src-tauri/tauri.conf.json` (не из тега).
- Подпись: пока без неё (Windows — SmartScreen-предупреждение, macOS —
  Gatekeeper). Дальше — секреты сертификатов в Settings → Secrets.

## macOS — как собирать (заметки)
- Tauri 2 на macOS = WKWebView (системный), отдельного рантайма нет
  (в отличие от Windows/WebView2). Бинарник — `.app` bundle + DMG.
- **Кросс-сборка с Linux — нет** (нужен Xcode SDK + `codesign`/`hdiutil`,
  которые есть только на macOS). Варианты:
  1. **CI: `macos-latest` runner** (GitHub Actions) — `cargo tauri build`
     → `.app`/DMG в артефактах. Без подписи работает только на машине
     сборщика (Gatekeeper); для дистрибуции нужен **Developer ID cert**
     + **notarization** (Apple ID с $99/год) — либо пользователь
     «разрешает» в Системных настройках.
  2. **Локальный Mac** — тот же `cargo tauri build` (нужны Xcode CLT).
- Иконки для macOS уже сгенерированы (`icons/icon.icns`).
- Для личного использования (без подписи): DMG + правый клик → «Открыть»,
  либо `xattr -dr com.apple.quarantine SpiderNotes.app`.
