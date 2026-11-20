# SpiderNotes: спецификация протоколов

Всё, что здесь описано, реализовано в `crates/p2p`, `crates/core` и
`crates/server`. Документ — контракт между устройствами и сервером.

## 1. Идентичность устройства

- `id` — hex(64) публичного ключа ed25519 (стабильный id устройства,
  хранится в `~/.config/spidernotes/identity.json`).
- `short_id` — первые 16 hex-символов `id`; используется как `device`
  в HLC.
- x25519-ключ — для Noise-транспорта (отдельная пара от ed25519).

## 2. Обнаружение соседей (mDNS)

- Тип сервиса: `_spidernotes._tcp.local.`
- Имя инстанса: `sn-<первые 12 символов did>`
- Host: `spidernotes.local.`, адрес подставляется автоматически
  (`enable_addr_auto`).
- TXT-записи:
  | ключ | значение |
  |------|----------|
  | `did`  | `id` устройства (hex 64) |
  | `pub`  | hex(32) публичного x25519-ключа (для Noise_XK) |
  | `port` | TCP-порт P2P-слушателя (по умолчанию `8788`) |
  | `name` | человекочитаемое имя устройства |

Устройство исключает из списка соседей запись со своим `did`.

## 3. Транспорт (TCP + Noise_XK)

- Соединение: обычный TCP; инициатор — устройство, нажавшее
  «Синхронизировать» (или авто-синк).
- Noise-паттерн **XK** (известный статический ключ ответчика,
  публикуется в mDNS):
  - msg1 (I→R): `E`
  - msg2 (R→I): `E, EE, S, ES`
- Крипто: `ChaCha_Poly1305` (AEAD), `SHA-256`, `X25519`.
  Pre-message: пустой.
- Инициатор после msg2 сверяет полученный `rs` с ожидаемым
  (из mDNS); рассогласование → разрыв («wrong device»).
- Кадр поверх зашифрованного потока:
  `[u32 LE длина][AEAD-шифротекст]`, длина = plaintext + 16 (тег).
  Ограничения: `16 <= длина <= 64 MiB`.

## 4. Сообщения протокола синка

Все сообщения — JSON в кадре. Схема сериализации: `{"t": <тип>, "c": <payload>}`.

```rust
enum Msg {
  Hello     { device: String, name: String, hlc: Hlc },
  HelloAck  { device: String, name: String, hlc: Hlc },
  Summaries { summaries: Vec<NoteSummary> },
  Note      { update: NoteUpdate },
  Done,
  Error     { message: String },
}

struct NoteSummary { uuid: Uuid, path: String, hlc: Hlc,
                     hash: String, deleted: bool }
struct NoteUpdate  { uuid: Uuid, path: String, hlc: Hlc,
                     hash: Option<String>, deleted: bool,
                     content: Option<String> }   // None для deleted

struct Hlc { ms: u64, counter: u32, device: String }
```

`path` — путь относительно корня vault, разделитель `/`, только `.md`.
`hash` — sha256 hex тела заметки без frontmatter.

### Порядок сообщений (одна сессия)

```
I ──Hello{device,name,hlc}──────────► R
I ◄──HelloAck{device,name,hlc}────── R
I ──Summaries{мои}─────────────────► R
I ◄──Summaries{их}────────────────── R
I ──Note{...} ... Done─────────────► R   (дельта I)
I ◄──Note{...} ... Done──────────── R   (дельта R)
```

Дальше соединение закрывается. Ошибки: `Error{message}` — сторона
разворачивает сессию; таймаутов в v1 нет (LAN).

### Дельта

`delta(mine, theirs)` = заметки из `mine`, которых нет в `theirs`
по `uuid` либо `mine.hlc > theirs.hlc` (включая tombstones —
`deleted=true` отправляются, чтобы донести удаление).

## 5. Часы и разрешение конфликтов

### HLC (Hybrid Logical Clock)

Порядок: лексикографическая пара `(ms, counter)`, затем `device`
(tiebreak между устройствами).

- `tick` (локальное событие): если wall > ms → `ms = wall, counter = 0`,
  иначе `counter += 1`.
- `receive` (получение удалённого):
  - `remote.ms > wall` → `ms = remote.ms, counter = remote.counter + 1`;
  - `remote.ms == wall` → `counter = max(counter, remote.counter) + 1`;
  - иначе ничего.
  `device` всегда остаётся своим.

### LWW (last-writer-wins)

- `apply_update(update)` применяет входящую версию только если
  `update.hlc > local.hlc` (или заметки локально нет). Иначе —
  `skipped` (локальная версия новее).
- Удаление = tombstone (запись с `deleted=true` и новым hlc);
  tombstone распространяется как обычная заметка.
- Файл, исчезнувший с диска, превращается в tombstone при
  `scan_and_repair`; появление файла «возвращает» заметку.
- После сессии синка индексы обеих сторон совпадают: для каждого
  `uuid` хранится версия с максимальным HLC.

Содержимое файла пишется с frontmatter `spidernotes-id: <uuid>`
(добавляется при необходимости, не трогается иначе).

## 6. Сервер (self-hosted, E2E)

Сервер — только «ящик» с шифротекстом: plaintext ему не виден.

- TLS: self-signed сертификат (генерируется при первом старте,
  хранится рядом с конфигом). Клиент либо принимает его явно
  (local server), либо пинит PEM-корень.
- Шифрование заметки: `vault key` (32 B, `OsRng`, лежит в
  `<vault>/.spidernotes/vault.key`, mode 600, base64).
  Формат `data`: `base64( nonce(24) || XChaCha20Poly1305-шифротекст )`.

### REST API

| Метод | Путь | Тело | Ответ |
|-------|------|------|-------|
| POST | `/v1/health` | — | `200` |
| POST | `/v1/auth` | `{"password": ...}` | `{"token": ...}` (argon2-проверка) |
| GET  | `/v1/notes` | — (Bearer) | `[{uuid, path, hlc, deleted, data?}]` |
| PUT  | `/v1/notes/{uuid}` | `{hlc, path, deleted, data?}` (Bearer) | `200` |

- `hlc` в API: `{"ms": u64, "counter": u32, "device": String}`.
- Сервер сохраняет максимум по HLC на `uuid` (тот же LWW, что и P2P).
- `data` обязателен для живых заметок, `null` для deleted.
- Клиентский `sync()`: pull (все remote с `hlc > local`) → push
  (все local с `hlc > remote`), те же правила дельты, что в P2P.

## 7. Известные ограничения (v1)

- mDNS-запись с `pub` открыта: активный MITM в LAN может подменить
  публичный ключ (пассивное подслушивание исключено Noise-ом).
  Путь: подпись записей ed25519 + «печатки» устройств в UI.
- Нет таймаутов/повторов на кадрах и сессиях — допустимо для LAN.
- Синк попарный (не рой): N устройств → до N(N-1)/2 сессий;
  в домашней сети это не проблема.
- Конфликт «одна заметка, два разных пути» невозможен: идентичность
  за uuid, путь — лишь текущее имя.
