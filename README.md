# Encrypted Note

A native, Obsidian-like markdown note-taking app written in Rust, with
**AES-256 encryption** for everything it stores. It is **Wayland-only** — it
refuses to start under X11 or XWayland — and targets Arch Linux.

Your notes never touch disk in plaintext: every note is encrypted with
AES-256-GCM, and the encryption key is derived from your master password with
Argon2id. The app asks for that password **every time it starts**, and there is
no passwordless way in.

## Contents

- [Encryption model](#encryption-model) · [Password policy](#password-policy) ·
  [Login lockout](#login-lockout-escalating) · [Forgot password](#forgot-password)
- [Features](#features) · [GPU rendering](#gpu-rendering) ·
  [Editing experience](#editing-experience) ·
  [Obsidian-parity features](#obsidian-parity-features)
- [Theme](#theme) · [Attachments](#attachments) · [RSS reader](#rss-reader) ·
  [Password manager](#password-manager) · [Icons & UX](#icons--ux) ·
  [Supported markdown](#supported-markdown)
- [Install](#install-arch-linux) · [Run](#run) ·
  [Security hardening](#security-hardening) ·
  [Keyboard shortcuts](#keyboard-shortcuts)
- [Where things are stored](#where-things-are-stored) · [Layout](#layout) ·
  [Development](#development) · [License](#license)

## Encryption model

| Concern | Choice |
| --- | --- |
| Data cipher | **AES-256-GCM** (authenticated encryption) |
| Key derivation | **Argon2id** (v19, 19 MiB, t=2, p=1), 256-bit key |
| Salt | 16 random bytes per vault, stored in the keyfile |
| Nonce | fresh random 12 bytes per note write |
| Password storage | **never stored** — only a salt and a verifier ciphertext |

- On first launch you create a master password. We generate a random salt and
  derive the key from your password with Argon2id. To recognise the correct
  password later we store a small **verifier** (the encryption of a fixed marker
  under your key). The password itself is never written anywhere.
- Each note on disk is framed as
  `magic("ENCD1") ‖ nonce(12) ‖ AES-256-GCM ciphertext+tag`, written via a
  temp-file-and-rename so a crash cannot corrupt a note.
- Notes from earlier (unencrypted) versions are transparently **migrated**: the
  first time you open such a vault while unlocked, any plaintext `.md` files are
  encrypted in place.

## Password policy

A master password must contain **all** of:

- at least **16 characters**
- at least **one number**
- at least **one capital letter**
- at least **one lower-case letter**
- at least **one special character**
- **no words** — it must not contain 4 letters in a row

A live checklist on the setup screen shows which rules are satisfied. The setup
screen also has a **sparkles** button that generates a random, policy-compliant
password for you (using OS randomness, never more than two letters in a row).

### After you set your password

A modern popup appears warning you to **record your master password somewhere
safe**, because it is the only key to your notes and cannot be recovered. The
vault only opens once you press the **OK** icon at the bottom-right of the popup.

## Login lockout (escalating)

Incorrect passwords are rate-limited, and the lockout state is persisted so
restarting the app cannot bypass it:

| Wrong attempts | Lockout |
| --- | --- |
| first 5 | **5 minutes** |
| next 5 | **10 minutes** |
| next 5 | **20 minutes** |
| next 5 | **40 minutes** |
| … | doubles each time (5 · 2ⁿ minutes) |

A successful login resets the counter and the backoff.

While locked out the login card shows an **animated hourglass** (drawn with
vector shapes) whose sand drains from the top bulb to the bottom as the
countdown runs down, next to a live `m:ss` timer.

## Forgot password

The unlock screen has a **Forgot password?** link. It shows a confirmation
warning that **everything will be lost**. If you confirm:

1. every note in your vault is deleted,
2. the keyfile (and therefore the password) is deleted,
3. the app resets and returns to the *create password* screen, pointing at your
   (now empty) vault.

You then set a new password and start with an **empty vault**.

> Because the key lives only in your password, a forgotten password means the
> notes are unrecoverable. The lockout is an app-level speed bump stored in a
> user-writable config file; the real protection is the AES-256 key itself.

## Features

- **Vaults** — a folder of markdown notes, with last-vault and recent-vaults
  memory; opened only after unlocking.
- **Sub-notes** — **hover** any note in the sidebar to reveal inline icons: **＋ New
  sub-note**, **✎ Rename**, **🗑 Delete** (no right-click menu). Sub-notes nest under
  the note using Obsidian's convention: `Parent.md` + `Parent/Child.md`.
- **File tree sidebar** with per-folder note counts.
- **Editor + live preview** in `Edit`, `Split`, and `Preview` modes per tab, with
  **undo/redo** (toolbar buttons, `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`, and palette commands).
- **Tabs** with dirty indicators and autosave.
- **Wikilinks** — `[[Note]]` / `[[Note|alias]]` as clickable preview links.
- **Backlinks** and **Outline** panels.
- **Full-text search** across decrypted notes.
- **Lock now** (File menu) drops the key and returns to the login screen.
- A modern, rounded dark theme matching Obsidian's default palette, with a
  violet accent (see [Theme](#theme)).

## GPU rendering

All drawing is GPU-accelerated. The app renders through **`wgpu`**, which on
Wayland uses **Vulkan** by default, and it asks for a **high-performance**
(hardware) adapter. On launch it prints the chosen device once, e.g.:

```
Encrypted Note: GPU rendering — backend=Vulkan, device='NVIDIA GeForce RTX 3070', driver='NVIDIA 615.78.08'
```

If Vulkan isn't available, wgpu transparently falls back to an OpenGL backend.
You can influence adapter selection with the standard wgpu environment variables:
`WGPU_BACKEND` (`vulkan`, `gl`, …), `WGPU_POWER_PREF` (`high`/`low`), and
`WGPU_ADAPTER_NAME`.

On Arch, make sure a Vulkan driver is installed for the GPU in use (e.g.
`vulkan-radeon`, `vulkan-intel`, or `nvidia-utils`).

## Editing experience

- **Live Preview** — a fourth view mode (`Edit` / `Split` / `Preview` / `Live`).
  Non-active lines are rendered as markdown (with inline markup hidden); the **active
  line is a real character-accurate `TextEdit`**. Caveat: unlike Obsidian, the active
  line shows its raw markup in full rather than hiding the non-caret spans, so it is a
  *line-accurate* hybrid, not character-accurate WYSIWYG. True WYSIWYG needs a custom
  rich-text engine (not built).
- **Vim mode** — toggled from the command palette; a full modal editing layer
  (`src/vim.rs`): Normal/Insert/Visual, motions, operators, `p`/`P`, `r`, `u`,
  **text objects** (`iw aw i( a( i" i' i[ i{ ip` with `d`/`c`/`y`), and **`.`-repeat**.
- **Clipboard** — `Ctrl+C` / `Ctrl+X` / `Ctrl+V` (and `Ctrl+A`) work in the editor
  and every text field. On KDE/Wayland this needed a workaround: eframe enables
  egui-winit's `smithay-clipboard`, which speaks the older **wlr-data-control**
  protocol that **KWin does not implement** (KWin uses **ext-data-control**), so
  paste silently failed. A small **patched `egui-winit`** (vendored under
  `vendor/egui-winit`) now falls back to an **in-app clipboard** when the platform
  backend errors, so copy/paste works within the app everywhere. **Cross-app**
  copy/paste also works on KWin: `src/system_clipboard.rs` implements a
  **`ext-data-control`** bridge (a reader thread caches the current selection, and
  each copy serves a data source), which the vendored egui-winit consults after its
  built-in backends fail.
- **Single instance** — only one copy runs at a time; a per-user Unix socket
  (`$XDG_RUNTIME_DIR/encrypted-note.sock`) is bound at startup, and a second
  launch prints a message and exits instead of opening another window.

## Obsidian-parity features

- **YAML frontmatter** — `title`, `aliases`, and `tags` are parsed; the frontmatter
  block is hidden from the rendered preview, and `title` overrides the file name.
- **Tags** — inline `#tags` and frontmatter tags are indexed. The right panel lists
  every tag with a count; clicking one filters the sidebar to that tag.
- **Aliases** — `[[alias]]` resolves to the note that declares it.
- **Anchors** — `[[Note#Heading]]` and `[[Note#^block]]` resolve to the note.
- **Embeds / transclusion** — `![[Note]]` renders the other note inline (depth-limited).
- **Command palette / quick switcher** — `Ctrl+P` to fuzzy-jump to any note or run a
  command (new note, open vault, daily note, new-from-template, **open canvas**,
  cycle view, toggle panel, toggle graph, lock, close tab, quit).
- **Daily notes** — *Open today's / previous / next daily note* commands, plus a
  monthly **calendar sidebar** for arbitrary dates. Obsidian-style: configurable
  **moment date format**, an optional **template note**, and a **Daily notes settings**
  dialog. Templates support `{{title}}`, `{{date}}`, `{{time}}` **and inline format
  tokens** `{{date:FORMAT}}` / `{{time:FORMAT}}` (e.g. `{{date:dddd, MMMM D}}`).
- **Templates** — *New note from template* builds a note from any file in
  `Templates/`, substituting `{{date}}` and `{{title}}`.
- **Graph view** — force-directed graph of notes and links, with a **local graph**
  (depth-controlled, anchored on the active note) and **filters** (tag, orphans).
  Obsidian-like: every node shows its **note title** beside it (labels are hidden
  when zoomed out and appear on hover/zoom-in or for the active note), node size
  scales with **link count**, links are **straight lines** meeting at shared
  junctions, and you can **drag to pan** and **scroll to zoom** (with a reset
  button).
- **Canvas** — a full-featured `.canvas` board editor (Obsidian's JSON format).
  **New canvas** (command palette) creates an empty board in the vault; **Open canvas
  board** opens one — or shows a **picker** when there are several. Cards:
  text/file/group/link; pan (middle/right-drag),
  zoom-to-cursor, rubber-band multi-select, drag to move, corner-resize, double-click
  to edit text in place, drag the ◉ handle to connect cards, right-click for a
  background/node context menu (new card, colour, delete), `Del` to delete, keys
  `1`–`6` to recolour, and **undo/redo** (toolbar, `Ctrl+Z` / `Ctrl+Shift+Z`). **Saved on
  close and on exit** (plus the Save button).
- **Mermaid (built-in)** — ` ```mermaid ` fences render **without any external tool**:
  a native renderer for flowcharts (`graph`/`flowchart` TD/LR, node shapes, `-->`,
  `---`, `-.->`, `==>`, edge labels, chained arrows) parses, lays out, and draws the
  diagram. Other Mermaid diagram types fall back to `mmdc` (if installed) then to a
  source block.
- **Richer backlinks** — the right panel shows **Backlinks** (with line numbers and
  context) plus **Unlinked mentions** (title/alias appearing as plain text).
- **Search power** — the search box understands a query language: plain terms,
  `"quoted phrases"`, `tag:`, `path:`, `file:`, `/regex/`, and `-` negation (all ANDed).
- **Math** — `$inline$` and `$$display$$` LaTeX is prettified to Unicode
  (Greek, operators, super/subscripts) — see `src/math.rs`.
- **Workspace persistence** — open tabs, active tab, and panel state are restored.
- **Markdown extensions** — GitHub tables, callouts, footnotes, strikethrough,
  `[text](url)` links (via `xdg-open`), plus everything listed below.

### Not implemented (yet)

Nothing from the earlier gap list remains unimplemented except **sync**, which was
out of scope by request.

## Logo

<p align="center"><img src="assets/logo.svg" width="96" alt="Encrypted Note logo"></p>

The app logo lives at `assets/logo.svg` — a rounded violet-gradient tile with a
note and a lock. It's shown on the welcome and password screens and set as the
window icon at startup (via `resvg`).

**Taskbar / system-monitor icon.** A Wayland window has no icon protocol: panels
resolve an app's icon from the `.desktop` file whose name matches the window's
**app id**. On launch the app installs, idempotently, into `~/.local/share`:

- `applications/encrypted-note.desktop` (with `Icon=encrypted-note` and
  `StartupWMClass=encrypted-note`)
- `icons/hicolor/scalable/apps/encrypted-note.svg`

so panels can show the logo. If your panel was already running, refresh its cache
(e.g. re-login, or run `kbuildsycoca6` on KDE). `packaging/encrypted-note.desktop`
is provided for a system-wide `pacman` install too.

## Theme

The dark theme mirrors Obsidian's default dark palette: `#1e1e1e` main /
`#161616` sidebars, `#8a5cf5` accent, `#dcddde` text, subtle `#333` borders, and
small corner radii on widgets, menus, and windows.

## Attachments

Attach **images, voice notes (audio), and videos** to a note from the **Attach:**
bar above the editor (image / mic / video icons). The file is copied into
`<vault>/assets/` and a markdown reference is inserted into the note (recorded
in the note's undo history, so **undo/redo covers attachments** too). In the
preview and live preview the note's attachments are shown at the top, **scroll
with the note**, and **play inline — no external player window opens**:

- **Images** are decoded in-process (PNG / JPEG / GIF / BMP via the `image`
  crate, SVG via `resvg`) — **WEBP is not supported** — rendered inline, and are
  **resizable** (drag the accent handle in the bottom-right corner) and **movable**
  (drag the image itself). The layout is remembered per attachment in
  `media-layout.json`.
- **Audio / voice notes** get an inline player with **play / pause / stop**, a
  **click-to-seek scrubber**, and a **live volume** slider. Audio is decoded to
  raw PCM by `ffmpeg`, volume-scaled in-process, and streamed through PipeWire
  (`pw-cat`) or ALSA (`aplay`).
- **Video** is decoded by `ffmpeg` into RGBA frames rendered as an egui texture,
  with the same **play / pause / stop** controls, **click-to-seek scrubber**, and
  **volume** slider; a video's soundtrack plays through the same audio path as
  its frames render, so you get **picture and sound together** in the note.

**Seeking** — click anywhere on the scrubber (or drag it and release) to jump to
that position; pause remembers the position and resume continues from it.
**Volume** — the slider (0–150%) applies instantly to whatever is playing and is
remembered in `media-layout.json`.

Playback uses the `ffmpeg` / `ffprobe` / `pw-cat` (or `aplay`) programs found
on `PATH`; nothing is linked into the binary, and only one item plays at a time
(playback stops on its own when you navigate away from the note or lock the
vault). *(Attachments are stored unencrypted so they can be played/rendered.)*

## RSS reader

A built-in feed reader (the **RSS** toolbar button, or the *RSS feeds* palette command):

- Add feeds by URL; they're fetched with the system **`curl`** in a **background
  thread** (no blocking), then parsed by a built-in RSS 2.0 / RSS 1.0 / Atom parser.
- **Articles** show the title, date, the **cleaned full text** — `<script>` /
  `<style>` / `<noscript>` / `<template>` contents are dropped entirely (so inline
  ad code like `(adsbygoogle = window.adsbygoogle || []).push({});` never shows),
  obvious JavaScript/ad lines are filtered out, HTML is stripped with paragraph
  breaks preserved, and the body is laid out in a centred ~720 px reading column
  with comfortable paragraph spacing — plus the **article link** as a modern
  clickable row beneath the text. Lead images and video/audio are intentionally
  not embedded; the reader focuses on readable text.
- **Load full article** — when a feed only provides a short summary, the reader
  offers a *Load full article* button that fetches the linked page in the
  background and extracts the readable body (favoring `<article>`/`<main>`) so
  the whole article shows inline. The fetched text is remembered on the item and
  saved with your feeds.
- **Edit** and **Delete** a feed from the hover icons on its row; a **＋** adds one.
- **Categories** — give a feed a category and feeds are grouped by it in the sidebar;
  *Uncategorized* holds the rest.
- **Save articles** with the **bookmark** icon in the reader; saved articles appear
  under a **bookmark-icon entry with a count** at the top of the feed list.
- Articles are always listed **newest first** (dates are parsed from RSS/Atom).
- **Global search** — the search box matches titles and text across **all** feeds.
- Feeds, categories, and cached articles persist in
  `$XDG_CONFIG_HOME/encrypted-note/feeds.json`.

## Password manager

A built-in password manager, opened from the **shield icon** in the toolbar
(right of the RSS icon) or the *Password manager* palette command. It is
**encrypted at rest with the same vault key** (AES-256-GCM) — the store lives in
`$XDG_CONFIG_HOME/encrypted-note/passwords.enc` and is only decrypted while the
vault is unlocked (locking the vault wipes it from memory).

- **Entries** hold a title, an optional **icon**, username, password, website,
  **folder**, free-form notes, and an optional **TOTP secret**.
- **Icons** — pick an **emoji**, or paste a **favicon image URL** (e.g.
  `https://youtube.com/favicon.ico`); the image is fetched in the background and
  shown **live** as you type (PNG / JPEG / GIF / ICO / SVG), in the list and the
  detail header.
- **Add / edit / delete** entries; editing shows a live **strength meter** with an
  **entropy estimate** (in bits) and a one-click **password generator**; a dice
  icon fills the username with a random, friendly one (e.g. `brave_otter42`).
  Hover an entry row (in the tree) to show **edit** / **delete** icons, like the
  folders.
- **Folders** — the sidebar is a **tree**: each folder row has a chevron, and
  clicking it **expands the folder to show its entries nested underneath** (like
  the notes/sub-notes tree). Uncategorized entries live under their own node.
  Create folders from the sidebar (or inline in the editor) and file entries into
  them; hover a folder to **rename** or **delete** it (deleting moves its entries to
  Uncategorized). Folders persist even when empty. Searching auto-expands matching
  folders.
- **Copy** the username, password, website, or a live **2FA code** to the
  clipboard; **reveal / hide** passwords with the eye toggle.
- **Two-factor codes** are generated in-app from a base32 secret (a from-scratch
  HMAC-SHA1 / RFC 6238 implementation), showing the **current and next code** with
  a countdown to the next window.
- **Search** across all fields, **filter by folder**, and mark entries as
  **favorites**; a header shows how many entries are favorites and how many have
  **weak** passwords.
- **Password generator** — choose length (8–128) and character classes, with a
  live strength read-out, entropy, and one-click copy.
- **Encrypted import / export** — the transfer icon opens a dialog that asks for a
  **password** (enforcing the same rules as the master password) and writes the
  store as an **AES-256-GCM backup with an Argon2id key** from that password; the
  same field decrypts a backup on import. Import also still reads plaintext JSON,
  a JSON array of entries (tolerant of common field names such as
  `name`/`login`/`pass`/`website`), a `{ "Title": "secret" }` map of TOTP
  secrets, or a single `otpauth://` URI.

## Icons & UX

Every control in the app is a hand-drawn **SVG icon** (see `assets/icons/`)
rather than text or emoji: the toolbar (a **vertical strip docked top-left**),
tabs, sidebar tree, dialogs, the
password gate and its checklist. The icons are monochrome SVGs embedded at
compile time and rasterised by `egui_extras`' SVG loader, then tinted to match
the current state (normal / hovered / selected / disabled). Hover any control to
see a tooltip describing it.

## Supported markdown

Headings, **bold**, *italic*, ~~strikethrough~~, `inline code`, bullet and numbered
lists, task list checkboxes, blockquotes, callouts, fenced code blocks, GitHub
tables, footnotes, horizontal rules, `[text](url)` links, Obsidian-style
`[[wikilinks]]` (with `|alias`, `#heading`, `#^block` anchors) and `![[embeds]]`.

## Install (Arch Linux)

This build is Wayland-only. It links against Wayland client libraries (and no
X11/XCB libraries):

```sh
sudo pacman -S --needed base-devel wayland libxkbcommon mesa
```

Build and install:

```sh
cargo build --release
sudo install -Dm755 target/release/encrypted-note /usr/local/bin/encrypted-note
sudo install -Dm644 packaging/encrypted-note.desktop \
    /usr/local/share/applications/encrypted-note.desktop
```

The app also installs a per-user desktop entry and icon into
`~/.local/share` on first launch (see [Logo](#logo)).

### Runtime tools (optional)

Some features shell out to programs found on `PATH`; everything else is built in:

| Feature | Needs |
| --- | --- |
| RSS reader, favicon fetching | `curl` |
| Audio / video attachments | `ffmpeg`, `ffprobe`, and `pw-cat` (PipeWire) or `aplay` (ALSA) |
| Mermaid diagrams other than flowcharts | `mmdc` (Mermaid CLI) |
| Opening external links | `xdg-open` |

Built-in Markdown, math, and Mermaid **flowcharts** need none of these.

## Run

The app opens as a **normal (windowed) window**. Press **`F11`** to cycle
**fullscreen → maximized → windowed** (or use the *Cycle window mode* command); the
fullscreen mode is true borderless fullscreen (no title bar), which works on KDE/KWin.
It also asks the compositor for keyboard focus on the first frame and auto-focuses the
password field.

```sh
cargo run --release
```

`WAYLAND_DISPLAY` must be set; otherwise the app prints an error and exits. The
binary is compiled without any X11/XCB backend, so it cannot fall back to X11 or
XWayland.

## Security hardening

Beyond the AES-256-GCM / Argon2id encryption, the code was audited and hardened:

- **No shell is ever used** — every external tool (`curl`, `ffmpeg`, `ffprobe`,
  `pw-cat`, `aplay`, `mmdc`, `xdg-open`) is run via `Command` with argument arrays,
  so note/feed content cannot inject shell commands.
- **`curl` is sandboxed** — only `http(s)` URLs, `-q` (ignores `.curlrc`),
  `--proto`/`--proto-redir =http,https` (a malicious feed cannot redirect to
  `file://` to read local files), `--max-redirs 5`, and `--` so the URL can never be
  parsed as a curl option.
- **Path confinement** — media/attachment paths taken from note text are resolved
  with `vault_file()`, which rejects absolute paths and `..`, canonicalises, and
  requires the result to stay inside the vault (defeats symlink traversal).
- **Sensitive files are `0600`** — the keyfile and the encrypted password store
  (and encrypted exports) are owner-only.
- **Unbiased randomness** — password generation uses rejection sampling
  (`crypto::random_below`) rather than `% n`, so there is no modulo bias.
- **Single-instance lock** lives in `$XDG_RUNTIME_DIR` (or the per-user config dir),
  never a world-writable `/tmp` path.
- **Link handlers are scheme-restricted** (`http`/`https`/`mailto` only); XML is
  parsed with `roxmltree` (no DTD/entity expansion, so no XXE / billion-laughs);
  regexes use the linear-time `regex` crate (no ReDoS).
- **Clipboard fallback** — the vendored `egui-winit` never returns `None` for paste
  when the platform backend errors (KWin's ext-data-control is unsupported by
  smithay), and the built-in `ext-data-control` bridge (`src/system_clipboard.rs`)
  provides cross-application copy/paste on KWin.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+N` | New note |
| `Ctrl+O` | Open vault |
| `Ctrl+S` | Save current note |
| `Ctrl+Z` | Undo |
| `Ctrl+Shift+Z` / `Ctrl+Y` | Redo |
| `Ctrl+P` | Command palette / quick switcher |
| `Ctrl+F` | Focus search |
| `Ctrl+W` | Close current tab |
| `Ctrl+A` | Select all (editor and text fields) |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste |
| `F11` | Cycle window mode (fullscreen/maximized/windowed) |

## Where things are stored

- Config and lockout state: `$XDG_CONFIG_HOME/encrypted-note/config.json`
- Keyfile (salt + password verifier): `$XDG_CONFIG_HOME/encrypted-note/keyfile.json`
  (written with `0600` permissions)
- Password manager (encrypted): `$XDG_CONFIG_HOME/encrypted-note/passwords.enc`
- RSS feeds and saved articles: `$XDG_CONFIG_HOME/encrypted-note/feeds.json`
- Media playback volume + image layout: `$XDG_CONFIG_HOME/encrypted-note/media-layout.json`
- Your notes: wherever your vault folder is, as encrypted `.md` files
- Attachments: `<vault>/assets/`, referenced from notes
- Single-instance socket: `$XDG_RUNTIME_DIR/encrypted-note.sock`
- Installed desktop entry + icon: `$XDG_DATA_HOME/applications/encrypted-note.desktop`
  and `$XDG_DATA_HOME/icons/hicolor/scalable/apps/encrypted-note.svg`

`$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, and `$XDG_RUNTIME_DIR` fall back to
`~/.config`, `~/.local/share`, and the temp dir respectively.

## Layout

```
src/
  main.rs       entry point, Wayland-only guard, eframe bootstrap
  app.rs        the egui UI, password gate, lockout, palette, graph, tags
  icons.rs      SVG icon set + icon-button helpers
  theme.rs      the dark colour scheme and spacing
  crypto.rs     AES-256-GCM + Argon2id, keyfile format, policy, generator
  frontmatter.rs minimal YAML frontmatter + inline tags
  index.rs      title/alias/tag/link indexes and wikilink resolution
  palette.rs    fuzzy matching for the command palette
  graph.rs      deterministic force-directed graph layout
  dates.rs      moment-style date formatting, day/month arithmetic, calendar
  vault.rs      vault scanning, tree building, note CRUD, daily notes
  markdown.rs   markdown renderer (tables, callouts, footnotes, embeds)
  math.rs       LaTeX → Unicode math prettifier
  mermaid.rs    built-in Mermaid flowchart parser, layout, and renderer
  search.rs     note search query language (phrases, tag:, path:, file:, regex)
  canvas.rs     Obsidian-compatible `.canvas` documents (nodes, edges)
  vim.rs        modal Vim editing layer (motions, operators, text objects)
  config.rs     persisted configuration, lockout, and workspace state
  desktop.rs    installs the desktop entry + icon for panel/taskbar display
  feeds.rs      RSS/Atom reader: fetch, parse, and article extraction
  passwords.rs  password manager: entries, generator, strength, TOTP
  media.rs      in-app audio/video playback and image layout
  system_clipboard.rs  ext-data-control clipboard bridge (KWin cross-app copy/paste)
assets/
  logo.svg      app logo (welcome screen, window icon, installed icon)
  icons/*.svg   the icon set
packaging/
  encrypted-note.desktop  system-wide desktop entry
vendor/
  egui-winit/   patched egui-winit (clipboard fallback for KWin)
```

## Development

```sh
cargo test      # unit tests: crypto, policy, lockout, icons, frontmatter, index,
                # palette, graph, math, mermaid, dates, search, canvas, vim,
                # markdown, vault, feeds, passwords, media
cargo clippy    # lints
```

There is one debugging flag: `cargo run -- --clip-selftest` starts a Wayland
clipboard instance, writes a test string, reads it back, and exits — useful for
checking the `ext-data-control` bridge on a compositor.

## License

MIT
