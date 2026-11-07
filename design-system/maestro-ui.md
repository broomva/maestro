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
| Desktop (1280 wide) | Decide, Today, Health (1280 by 1358), Nothing needs you |
| Phone (390 by 844) | Decide, Passkey, Undo window, Receipt, Mac asleep, Queued, Health (390 by 2594, scrolls), Today |

The Health frames sit in their own column to the right of the other screens because they are taller.

## Health: one pane for what works and what fails

Health answers "is the fleet up, and what is the weak spot?" It reads top to bottom:

1. **Verdict.** One sentence, five subsystem pills (Hosts, Providers, Tokens, Loops, Sources) and a "Needs a look" row of the items behind any warning.
2. **Hosts.** Mac, NUC and VPS on the tailnet, each with gauges for CPU (load per core), memory, swap, disk, plus SSD on the Mac and GPU and VRAM on the NUC; sessions against the host cap; and a state chip.
3. **Providers by host.** One row per provider lane, one column per host. The phone shows only lanes with a problem, with a "show all" link.
4. **Token capacity.** Per account: used against the 80% throttle, a 24 hour sparkline (7 days designed in), the reset time, routing slots, and a run-out projection.
5. **Loops** with the dispatch gate split (allowed, denied and sent elsewhere, denied).
6. **Last 24 hours.** One swimlane per lane or host that failed, in local time.

Rules the frames follow (from the `dataviz` skill):

- **Status colour always comes with a glyph and a word.** Status colours are the `ok`, `warn` and `fail` variables. They are tuned for each mode, and green and amber stay apart under colour-vision deficiency.
- **Glass only on the verdict and host tiles.** Everything else sits flat on the page.
- **"Not measured" never looks like zero.** It uses an empty track, a dash and the words "not measured". A span that could not be read is hatched. Sparklines shade the time before history began.

The values are a live read from 2026-11-05 at about 16:05 local (21:05Z).

### Data map

| On screen | Source today |
|---|---|
| Host CPUs, load per core, memory used, live sessions and caps | `fleet/meta/fleet-snapshot.json` (`host_load`, `host_caps`) |
| Mac swap, internal disk and SSD | `fleet/meta/capacity.json` (`host`) |
| Provider lane state per host, failure streak | `fleet-snapshot.json` (`connections`) |
| Token used, state, routing slots | `capacity.json` (`routing`, `claude_throttle`) |
| Token sparklines and reset times | `fleet/meta/capacity-history.jsonl` (from 15:12Z on 11-05) |
| Run-out projection | burn rate over the history; the logic already in `quota-dashboard/quota_dashboard.py` |
| Loops | `fleet/meta/loop-health.json` |
| Incident lanes | `capacity.log` state transitions, plus census samples |
| Sources pill | Maestro's existing read health |

### Sensor gaps (shown as "not measured")

- NUC and VPS disk and swap; NUC GPU and VRAM.
- Tailnet latency between hosts.
- Host history. CPU and memory are point-in-time today, so the tiles have no 24 hour line.
- Gemini Vertex quota.
- Token history beyond about 6 hours. The 7 day view is drawn but has no data yet.

The Mac's two memory sensors disagree. The census reports 23% available; the governor reports 65% free. They probably measure different things (available memory vs free plus reclaimable memory). The frames use the census figure because it is measured the same way on every host. Reconciling the two is backend work.

## What the design needs from the backend

1. **An answer writer in maestrod** with passkey owner verbs: sign, write, undo for 8 seconds, then re-check when the answer applies.
2. **Self-fetch.** Maestro fetches main itself about every 2 minutes. Today it depends on other jobs fetching, so "read 20 s ago" is only true once this ships.
3. **Pre-issued codes** so the phone can sign while the Mac sleeps. The code is single-use, expires in about 12 hours and is tied to the device. The PWA keeps an outbox and caches the last snapshot.
4. **A receipt lifecycle** per answer: signed, written, on main, acting, acted.
5. **Health sections.** Maestro reads the fleet meta files above as new Sections, alongside its existing ones.
6. **More host sensors.** The census measures disk, swap, GPU and VRAM on every host, and tailnet round-trip time between hosts.
7. **History.** Host samples and token samples are kept for 24 hours to 7 days, so the tiles and the 7 day view have data.
8. **One memory metric.** The census and the governor agree on what "memory" means on the Mac.

## Not in the file

- The headline accent on the recommended option (`park it` in tide) is one text run in Pen. The code renders it as an `em`.
- The tide field on the empty state is a static frame at t = 6. In the app it is the live `liquid-dawn` kit field.
- Ask content is real (the open asks on 2026-11-04 and 11-05). Times, commits and counts in the receipts are illustrative.
