# Web dogfood and source contracts

The owner surface answers three questions: what needs a decision, what changed today, and whether the fleet can take more work. Read-only navigation preserves one document; owner answer/enrollment forms retain native navigation and the passkey boundary.

Health reads machine-local telemetry from `MAESTRO_FLEET_META` (default `~/.config/broomva/fleet/meta`). Census `observed_at`, each host's `host_last_observed.at`, capacity `generated_at`/`ttl_seconds`, and loop `evaluated_at` establish freshness. Copying an old file does not refresh it. Unknown, missing, stale, invalid or partial data never certifies a healthy fleet. Registered cloud hosts are included. The capacity reader uses the fleet quota producer, so the web Health page does not require the separate legacy `MAESTRO_USAGE` economy input.

History readers take at most 1 MiB per log and disclose omitted earlier records. Quota history uses `capacity-history.jsonl`; capacity transitions use `capacity.log`; provider transitions use `signals.jsonl`; host samples use `usage-watch.jsonl`. Missing or malformed inputs remain explicit. Chart lines break at unread samples, counter resets and long sample gaps. Projection requires current contiguous samples and a known reset. Incident charts coalesce adjacent identical intervals, retain gaps, and bound their preview. Blank intervals are not healthy intervals. Raw evidence and source errors have bounded previews.

Today reads delivery activity from the configured owner directory's `receipts.jsonl`, at most a 256 KiB tail. The latest recorded state for each receipt wins; an identifiable damaged latest record invalidates the older state. A writer commit proves a recorded ledger write, not main landing. A recorded T3 delivery proves notification, not execution. Maestro does not infer an agent outcome, display receipt notes, or add a second work-ledger writer. Work holds have read-only disclosures; work lifecycle actions remain with their existing controller.

Each render pins overlay refs in a fresh read scope and reuses immutable git ask reads. The next request sees new answers; there is no cross-request answer TTL. A shared disclosure separates request read time, producer time and the workspace commit date.

## Rerunnable interaction checks

- Serve a build with an explicit test owner directory and open it in the shared browser. Evaluate `scripts/acceptance/web-navigation.js` for tab selection, focus, Back, rapid-click ordering and overflow. Evaluate `stalled-navigation.js` twice to verify timeout/native fallback recovery.
- Create an isolated fixture with `python3 -B scripts/acceptance/owner-fixture.py /tmp/maestro-owner-dogfood-UNIQUE --validator /absolute/workspace/scripts/ask_ledger.py --binary /absolute/maestro/target/debug/maestro`. Run its printed server command.
- Open `http://localhost:7190/owner`, fill the fixture's setup code, and evaluate `scripts/acceptance/signed-owner.js`. Navigate to its returned decision URL and evaluate it again. The test authenticator uses WebCrypto ES256; the real server verifies registration/assertions and writes the isolated answer after Undo and a second signature. Local mode prevents remote landing; the fixture names no agent session.
- Inspect Decisions, Today, Health and owner status at desktop, 390 px and 320 px in both themes. Operate disclosures and inspect freshness/error states, keyboard focus, note restoration and touch targets.

The signing fixture is not proof of physical biometrics, iCloud passkey synchronization or Home Screen behavior. Do not run test answers against the real owner's ledgers. Private live screenshots belong in the owner's workspace, not this repository.
