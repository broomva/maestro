# Maestro UI redesign (BRO-3606)

`maestro-ui.pen` is the design of record for the redesigned Maestro owner surface: one web app served by maestrod. The same app renders as an MCP App in chat and installs as a PWA on the phone. Open the file in Pen; `maestro-ui-export/` holds a WebP of every frame for review without Pen.

This is design only. No product code changes with it.

## Decisions it encodes (owner, 2026-11-04 and 2026-11-05)

- **Style:** the `liquid-dawn` skill (broomva/skills, `skills/design/liquid-dawn`): paper grounds, dew glass with a pearl liquid-metal rim, one tide accent. Light mode is Dawn as specified.
- **Dark mode:** Midnight at OKLCH hue about 250. This is a Maestro-only override of Liquid Dawn's Dusk, which sits at hue 228 and reads sea-green.
- **Write path:** maestrod becomes the one answers writer. Answers are written in place and signed with a passkey, with an 8 second undo and an instant receipt. The T3 hand-off stays only until this ships.
- **Where the writer runs:** on the Mac for now. An answer made while the Mac sleeps is signed on the phone with a code the Mac issued beforehand. It is queued, then re-checked and written when the Mac wakes. Moving the writer to the NUC is a later step the owner approves separately.
- **Views:** Decisions, Today and Health only.
- **Desktop decision:** three columns: inbox, decision, and what the agent tried.

## Frames

Every screen exists in Dawn and in Midnight. The Midnight copies carry `theme: {mode: "midnight"}`, and all colours are Pen variables on the `mode` theme axis, so editing a token changes both modes.

| Surface | Frames |
|---|---|
| Desktop (1280 wide) | Decide, Today, Health, Nothing needs you |
| Phone (390 by 844) | Decide, Passkey, Undo window, Receipt, Mac asleep, Queued, Health, Today |

## What the design needs from the backend

1. **An answer writer in maestrod** with passkey owner verbs: sign, write, undo for 8 seconds, then re-check when the answer applies.
2. **Self-fetch.** Maestro fetches main itself about every 2 minutes. Today it depends on other jobs fetching, so "read 20 s ago" is only true once this ships.
3. **Pre-issued codes** so the phone can sign while the Mac sleeps. The code is single-use, expires in about 12 hours and is tied to the device. The PWA keeps an outbox and caches the last snapshot.
4. **A receipt lifecycle** per answer: signed, written, on main, acting, acted.

## Not in the file

- The headline accent on the recommended option (`park it` in tide) is one text run in Pen. The code renders it as an `em`.
- The tide field on the empty state is a static frame at t = 6. In the app it is the live `liquid-dawn` kit field.
- Ask content is real (the open asks on 2026-11-04 and 11-05). Times, commits and counts in the receipts are illustrative.
