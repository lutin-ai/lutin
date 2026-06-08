# lutin-tui — feature spec

A modal, keyboard-first terminal client for the lutin agent platform. Helix-inspired interaction model. Not an implementation plan — just what the app _does_.

---

## 1. Philosophy

- **Modal.** Three real modes (Normal, Insert, Command); everything else is a transient overlay that auto-exits.
- **Keyboard-first, mouse-tolerant.** Every interaction is reachable by key. Mouse clicks map to the same actions Enter triggers, never to anything keyboard cannot do.
- **Pane-level focus, not element-level.** Focus lives on a pane. Each pane owns its internal selection. `Enter` activates the pane's primary selectable.
- **One verb, context-dependent.** `Enter` always means "activate the focused thing." What that does is up to the thing.
- **View stack.** Every detail screen pushes onto a stack; `ga` pops. Like browser back.
- **Discoverable.** Leader keys (`Space`, `g`) summon a which-key popup. Nothing relies on memorization.

---

## 2. Launch behaviour

- **Default:** restore the last active session.
- **No prior session / `--new` flag:** open the dashboard.
- **`--session <id>` flag:** open directly into that session.
- **`--dashboard` flag:** force the dashboard regardless of history.

---

## 3. Modes

| Mode        | Entered by                   | Exited by        | What it does                                                                                            |
| ----------- | ---------------------------- | ---------------- | ------------------------------------------------------------------------------------------------------- |
| **Normal**  | default; `Esc` from anywhere | —                | All keys are commands. Navigation, leaders, view switching.                                             |
| **Insert**  | `i` in Normal                | `Esc`            | Composer captures keystrokes. Helix-style cursor with motions (opt-in via setting; plain mode default). |
| **Command** | `:` in Normal                | `Esc` or `Enter` | Ex-style command line at the bottom. `:q`, `:settings`, `:new-session`, etc.                            |

Transient overlays (not modes — auto-exit on confirm/cancel):

- **Picker** — a modal list selection (project/session/persona/command/etc).
- **Which-key** — leader-pending popup showing next-key bindings.
- **Approval prompt** — when an agent requests permission for a tool call.
- **Confirm prompt** — for destructive actions.

---

## 4. Windows & layout

```
┌─ breadcrumb ─────────────────────────────────── project · persona ─┐
├─ tab strip ────────────────────────────────────────────────────────┤
│                                                                    │
│   ┌─ body view (one of: main / dashboard / detail / settings) ──┐  │
│   │                                                              │  │
│   │   [ chat pane ]                       [ right rail pane ]    │  │
│   │                                                              │  │
│   └──────────────────────────────────────────────────────────────┘  │
│                                                                    │
├─ composer ─────────────────────────────────────────────────────────┤
├─ status bar — mode · focus · flash · hints ────────────────────────┤
└────────────────────────────────────────────────────────────────────┘
```

**Persistent chrome (always visible):**

- **Breadcrumb** — shows the view stack: `dashboard › session: refactor auth › message #4`.
- **Tab strip** — open sessions, with running-task indicators on inactive ones.
- **Composer** — input area; expands when in Insert mode.
- **Status bar** — mode pill, current focus, flash message (transient feedback), key hint summary.

**Body — one view at a time** (see §6).

---

## 5. Panes (in the Main view)

Two panes side-by-side. Focused pane has a brighter border and an accent-colored bracketed title; others are dim.

### Chat pane (left, primary)

- Displays the conversation linearly.
- Primary selectable: the currently selected **message**.
- Internal nav: `j` / `k` between messages, `g g` top, `G` bottom.
- `Enter` on a message → pushes Message Detail view.
- Tool calls rendered inline (collapsed by default; the most recent / running one expanded).
- Sub-agent activity rendered inline with running progress.

### Right rail (right, secondary)

Stack of cards. Each card is one selectable. `j` / `k` moves between cards.

| Card             | Activated by `Enter`         | Notes                                                      |
| ---------------- | ---------------------------- | ---------------------------------------------------------- |
| **Persona**      | Opens persona picker         | Shows current persona name, model, temp, brief description |
| **Sub-agents**   | Pushes sub-agent detail view | Live progress for any running sub-agent                    |
| **Session info** | Pushes session detail        | Token counts, started-at, message count                    |
| **Project info** | Opens project picker         | Project name, session count                                |
| **Metrics**      | Pushes metrics view          | ttft, tok/s, cost, context window usage                    |

The rail can be hidden via `Space r` to give the chat full width.

---

## 6. Views (the view stack)

Every view pushes onto a stack. `ga` pops one; `gA` pops to root.

| View                 | How to open                              | Body contents                                                        |
| -------------------- | ---------------------------------------- | -------------------------------------------------------------------- |
| **Main**             | default; root of the stack               | Chat + right rail                                                    |
| **Dashboard**        | `gd` or `lutin --dashboard`              | Recent projects, recent sessions, quick-actions                      |
| **Message Detail**   | `Enter` on a message in chat             | Full message body, all tool calls expanded, metrics for that turn    |
| **Tool Call Detail** | `Enter` on a tool call in message detail | Full input/output, diff with line numbers, file path, status, timing |
| **Sub-agent Detail** | `Enter` on sub-agents card               | Sub-agent transcript, current step, parent message link              |
| **Settings**         | `Space ,` or `:settings`                 | Sectioned settings (general, control plane, keybinds, theme)         |
| **Help**             | `Space ?` or `:help`                     | Cheat sheet of keybinds; same content as which-key, but full-screen  |

The breadcrumb at the top always reflects the current stack. The stack is **per-tab** (switching sessions doesn't lose where you were in another).

---

## 7. Focus model

- **Pane focus** moves with `Alt+h` / `Alt+l` (and `Alt+j` / `Alt+k` once vertical splits exist).
- **Selection within a pane** moves with `j` / `k`.
- **`Enter` activates** the focused pane's primary selectable.
- **Composer is not a normal pane** — it has its own focus state, entered via `i` and exited via `Esc`.
- **Mouse click on a card / message** focuses the pane and the element, then activates as if Enter were pressed.

---

## 8. Sessions, tabs, splits

- **Tabs** — multiple sessions open simultaneously. Each has its own independent view stack and chat state.
- **`1` / `2` / `3` / …** switch directly to that tab.
- **`Space [` / `Space ]`** previous / next tab.
- **`Space t n`** new session tab; **`Space t c`** close current tab; **`Space t r`** rename.
- **Inactive tab indicators** — small dot or icon shows running task (`●`), pending approval (`!`), or completed since last view (`*`).
- **Splits (deferred to v2)** — `:vsplit` / `:hsplit` will split the body into multiple panes, each running its own session.

---

## 9. Personas

- Always visible in the right rail (Persona card) and in the breadcrumb on the right.
- **Switch via:** `Space k`, `Enter` on persona card, `:persona <name>`, or mouse click on the card.
- **Picker shows:** name, model, temperature, short description, last-used timestamp, tool list.
- **Switching mid-session** — the new persona takes effect from the next message.
- **Persona-scoped settings** persisted per-persona (model, temp, tool allowlist, system prompt).

---

## 10. Tool calls

Rendered inline in chat under their parent assistant message.

- **Collapsed (default):** single-line summary — `▸ edit · src/auth/middleware.rs · +12 -28 · ✓`.
- **Expanded:** body visible (diff for edits, output for shell, file content snippet for reads).
- **Toggle:** when chat pane is focused and a message containing the tool call is selected, `z c` collapses, `z o` expands, `z a` toggles. `Tab` from selected message focuses the next tool call within it for individual toggling.
- **State icons:** `⟳` running, `✓` done, `✗` failed, `?` awaiting approval.

### Approval flow

When an agent requests permission for a tool call:

- Screen dims.
- Approval overlay appears with: tool name, args / diff / command preview, and `y` / `n` buttons.
- Status bar flashes `! approval needed`.
- An inactive tab with a pending approval gets a `!` indicator.
- Keys in the overlay: `y` approve, `n` deny, `e` edit-then-approve (opens the tool args for editing), `Esc` defer (closes the overlay; can be reopened via `Space a`).

---

## 11. Sub-agents

- Listed in the right rail's Sub-agents card.
- Each entry shows name, status (running/idle/done), progress bar where applicable, elapsed time.
- `Enter` on the card opens the Sub-agent Detail view: full transcript of the sub-agent's tool calls and messages, with a back-link to the parent message that spawned it.

---

## 12. Composer

The text input at the bottom.

- **Plain mode (default):** standard cursor between characters. `Enter` sends. `Shift+Enter` newline. `Ctrl+w` delete word back. `Ctrl+u` clear line.
- **Helix-modal mode (opt-in via setting):** cursor lives _on_ a character. Motions `h j k l w b e W B E x X 0 $ ^ g g G`. Insert mode entered with `i` / `a` / `o` / `O`. `Esc` returns to composer-normal. No selections in v1.
- **Send** with `Enter` (in plain mode) or `Enter` in composer-normal (in modal mode).
- **Slash commands** — typing `/` at the start of the composer opens an inline picker for commands the workflow supports (e.g. `/compact`, `/clear`, `/export`).
- **Mentions** — typing `@` opens an inline picker for personas or sub-agents.
- **File attach** — typing `:attach <path>` or dragging a path / using the fuzzy file picker via `Space f`.
- **Paste** — bracketed paste mode supported; long pastes show a "pasted N lines — `Ctrl+e` to expand" placeholder.

---

## 13. Pickers

All pickers share the same modal UX: fuzzy search input on top, results list below, footer with key hints.

| Picker              | Open with                            | Items                                                                  |
| ------------------- | ------------------------------------ | ---------------------------------------------------------------------- |
| **Project**         | `Space p`                            | All known projects + session counts                                    |
| **Session**         | `Space s`                            | All sessions in current project, with last-message preview + timestamp |
| **Persona**         | `Space k` or `Enter` on persona card | All personas with model, temp, description                             |
| **Command palette** | `Ctrl+P` or `Space :`                | Every named action in the app                                          |
| **File**            | `Space f`                            | Files in current project (for attach / inspect)                        |
| **Workflow**        | `Space w`                            | Available workflows (chat, scratchpad, image, reviewed, principled)    |

**Picker keys:** `j/k` or `↑/↓` move, `Enter` select, `Esc` cancel, type to filter, `Ctrl+n`/`Ctrl+p` alternate up/down, `Tab` accept current filter as input.

---

## 14. Which-key

A transient bottom-left popup showing what the next keystroke does after a leader.

- Appears 300 ms after pressing a leader key (so fast users never see it).
- Updates as you go deeper (e.g. `Space t` shows `n / c / r` for tab actions).
- Dismissed by completing the chord, pressing `Esc`, or by the action firing.

---

## 15. Settings

Opened via `Space ,` or `:settings`. Sectioned:

| Section           | Contents                                                                                 |
| ----------------- | ---------------------------------------------------------------------------------------- |
| **General**       | Theme, composer mode (plain vs modal), leader key, which-key delay, default startup view |
| **Control plane** | CP endpoint URL, auth token, reconnect behaviour                                         |
| **Personas**      | Edit persona files (opens the file in inline editor or external `$EDITOR`)               |
| **Workflows**     | Default workflow for new sessions, per-project overrides                                 |
| **Keybinds**      | Inspect and override the keymap; per-mode                                                |
| **Theme**         | Pick from bundled themes, or point at a custom theme file                                |
| **Daemons**       | TTS / STT process status, start/stop, configuration                                      |

Settings persist to `~/.config/lutin/config.toml` (or platform equivalent). The settings view itself is a pushed view; `ga` returns to wherever the user was.

---

## 16. Notifications

- **Flash messages** — short transient text in the status bar (3-5s) for non-critical feedback ("persona → @reviewer", "session created", "tool failed").
- **Toasts** — top-right ephemeral cards for events the user might miss (sub-agent finished, approval needed, daemon disconnected). Auto-dismiss after a configurable timeout.
- **System notifications** — optional; shells out to `notify-send` / equivalent for high-priority events when the app is unfocused.

---

## 17. TTS / STT

Run as **separate daemon processes**, supervised by the lutin control panel — not in the TUI process.

- **Status indicator** in the status bar: `🎙 listening` when STT is active, `🔈 speaking` when TTS is active.
- **Push-to-talk:** `Space v` toggles STT capture; the daemon streams transcribed text into the composer.
- **Read aloud:** `Space V` (capital) on a focused message sends it to the TTS daemon.
- **Configuration** lives in Settings → Daemons; the TUI just talks to them over the CP socket.
- **Works over SSH:** because daemons stay local and only their state is reported.

---

## 18. Theming

- Bundled themes: `slate` (default), `helix-onedark`, `notebook-cream`, `linear-dark`, `solarized-dim`, `high-contrast`.
- Themes are TOML files mapping semantic names (`accent`, `dim`, `running`, `success`, `error`, `bg`, `fg`, `user`, `assistant`, etc) to RGB.
- Live reload — editing the active theme file refreshes immediately.
- Per-persona theme override (so `@reviewer` can look visually distinct).

---

## 19. Keybind reference

### Normal mode — global

| Key               | Action                                                                             |
| ----------------- | ---------------------------------------------------------------------------------- |
| `q`               | Quit                                                                               |
| `:`               | Enter Command mode                                                                 |
| `i`               | Enter Insert mode (focus composer)                                                 |
| `Space`           | Begin leader sequence                                                              |
| `g`               | Begin `g`-prefix sequence                                                          |
| `Esc`             | Clear flash / cancel pending leader                                                |
| `Enter`           | Activate focused element                                                           |
| `j` / `k`         | Move selection within focused pane                                                 |
| `Alt+h` / `Alt+l` | Move pane focus (left/right)                                                       |
| `Alt+j` / `Alt+k` | Move pane focus (up/down) — used once splits exist                                 |
| `1` … `9`         | Switch to tab N                                                                    |
| `Tab`             | Cycle internal sub-focus within a pane (e.g. tool calls within a selected message) |

### Leader (`Space …`)

| Chord                 | Action                        |
| --------------------- | ----------------------------- |
| `Space p`             | Project picker                |
| `Space s`             | Session picker                |
| `Space k`             | Persona picker                |
| `Space n`             | New session (current project) |
| `Space ,`             | Settings                      |
| `Space ?`             | Help                          |
| `Space :`             | Command palette               |
| `Space f`             | File picker                   |
| `Space w`             | Workflow picker               |
| `Space r`             | Toggle right rail             |
| `Space a`             | Show pending approvals        |
| `Space v`             | Toggle STT                    |
| `Space V`             | Read focused message aloud    |
| `Space t n`           | New tab                       |
| `Space t c`           | Close tab                     |
| `Space t r`           | Rename tab                    |
| `Space [` / `Space ]` | Previous / next tab           |

### Goto (`g …`)

| Chord         | Action                 |
| ------------- | ---------------------- |
| `g a`         | Back (pop view)        |
| `g A` / `g h` | Back to root           |
| `g d`         | Open dashboard         |
| `g g`         | Top of current pane    |
| `G`           | Bottom of current pane |

### Insert mode (composer)

| Key                                           | Action                      |
| --------------------------------------------- | --------------------------- |
| `Esc`                                         | Back to Normal              |
| `Enter`                                       | Send message                |
| `Shift+Enter`                                 | Newline                     |
| `Ctrl+w`                                      | Delete word back            |
| `Ctrl+u`                                      | Clear line                  |
| `/` (at start)                                | Inline slash-command picker |
| `@`                                           | Inline mention picker       |
| (modal mode only) `h j k l w b e x X 0 $ ^ G` | Helix motions               |

### Command mode (`:`)

| Command                    | Action                               |
| -------------------------- | ------------------------------------ |
| `:q` / `:quit`             | Quit                                 |
| `:settings`                | Open settings                        |
| `:dashboard`               | Open dashboard                       |
| `:new-session [title]`     | Create new session                   |
| `:rename <title>`          | Rename current session               |
| `:persona <name>`          | Switch persona                       |
| `:project <name>`          | Switch project                       |
| `:workflow <name>`         | Open new session with given workflow |
| `:vsplit` / `:hsplit` (v2) | Split current view                   |
| `:set <key>=<value>`       | Set a setting                        |
| `:reload`                  | Reconnect to CP / reload config      |
| `:export <path>`           | Export current session to markdown   |

### Picker / overlay

| Key                    | Action                     |
| ---------------------- | -------------------------- |
| `j` / `k` or `↑` / `↓` | Move                       |
| Type                   | Fuzzy filter               |
| `Enter`                | Select                     |
| `Esc`                  | Cancel                     |
| `Ctrl+n` / `Ctrl+p`    | Alternate up/down          |
| `Tab`                  | Accept filter as raw input |

### Approval overlay

| Key   | Action                       |
| ----- | ---------------------------- |
| `y`   | Approve                      |
| `n`   | Deny                         |
| `e`   | Edit then approve            |
| `Esc` | Defer (reopen via `Space a`) |

### Tool-call interactions (chat pane, message selected)

| Key                    | Action                     |
| ---------------------- | -------------------------- |
| `Tab`                  | Focus next tool call       |
| `Shift+Tab`            | Focus previous tool call   |
| `z c`                  | Collapse                   |
| `z o`                  | Expand                     |
| `z a`                  | Toggle                     |
| `Enter` (on tool call) | Push Tool Call Detail view |

---

## 20. Out of scope (v1)

- Inline image rendering (Kitty / Sixel) — defer; show URLs and paths instead
- Real-time charts and sparklines beyond the simple sub-agent progress bar
- Drag-and-drop file attachment (file picker only)
- Custom workflow UIs (workflows render through a fixed widget vocabulary; cdylib plugin model from lutin-desktop is not portable)
- Selections / multi-cursors in the composer (Helix motions yes, selections no)
- Mouse-driven pane resize (keyboard only in v1)
- Splits — deferred to v2 once tabs feel insufficient
- Notebook-style rich output (tables with reflow, embedded code execution) — chat-style flat rendering only
