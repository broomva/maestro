# The ledger fast lane: asks and signed answers count in seconds (BRO-4093)

Status: accepted. Owner decision, 2026-11-22 (AskUserQuestion): "Approve the fast lane" and
"Route agents to the tool". This replaces the flow kernel's 2026-11-09 rule "Landed main,
remote-checked" (`crates/flow/src/kernel/asks.rs`, item 4).

## Problem

An ask shows on Decisions only once its asks-only PR merges to main, and the owner's
passkey-signed answer reaches agents only once its landing PR merges. CI can take an hour.
Progress must not stall on it.

## Decision

1. **One branch, `maestro/ledger`, written only by Maestro.** It replaces `maestro/answers` and
   carries both kinds of ledger change: asks filed through Maestro and the owner's answers. Maestro
   writes it in its own checkout of the workspace (`MAESTRO_ANSWERS_CHECKOUT`), under its write
   lock, on a base of the branch with main merged in, and pushes it as a fast-forward. The owner's
   checkout and main are never written. While `maestro/answers` still exists on a remote, readers
   read it too, as a second source of the same kind.

2. **Asks.** `maestro_ask_file`, and `flow ask`, which files through the same path
   (`maestro_store::requests::file_ask`), files the ask with the writer of record
   (`ask_ledger.py file`) into the ledger in Maestro's checkout. It then commits that one file with
   a `Maestro-Ask: <ledger>#<id>` trailer and pushes. The kernel's confirm step reads the row back
   from the branch tip Maestro pushed, not from the owner's working tree. Agents file asks through
   the tool. An ask in a plain PR branch appears only once merged to main.

3. **Answers.** The owner's answer is written as in BRO-4042 (passkey, 8 second undo, a commit
   carrying `Maestro-Answer` and `Maestro-Owner-Proof` trailers), on `maestro/ledger`. So an ask
   that is on the branch but not yet on main can be answered.

4. **Readers count the branch at once.**
   - Maestro's asks read is main overlaid with `maestro/ledger`. A ledger file on the branch,
     merged with main, is the one read. An ask on the branch and not on main shows as open, marked
     filed and waiting to land.
   - An answer counts from the branch at once, for `flow ask` and the coordinator, only when:
     - it is read remotely and object-verified, as main is read today: `refs/heads/maestro/ledger`
       of the compiled answers remote (`ls-remote`), then that commit object by object
       (`tree::read_verified`). No local ref counts.
     - the commit that wrote it carries a `Maestro-Owner-Proof` that verifies: a WebAuthn ES256
       assertion over `SHA-256(intent || "\n" || nonce)`, from an enrolled passkey (the keyring
       Maestro holds) for the configured relying party, with user presence and verification.
     - the intent's ledger, handle, option and note are the ones the row now holds.

     An answer on the branch without a verifying proof is no answer, and readers say so.
   - Main stays a source. An answer that has landed on main counts as before.

5. **Landing is bookkeeping.** A landing PR from `maestro/ledger` into main is opened or kept
   open, and merged by its gates or the coordinator's rule. Nothing waits on it.

## Consequences

- An ask is on screen within a minute of filing, and a signed answer resumes its flow within a
  minute, with no PR merged.
- The evidence of the owner's answer is the passkey signature, verified, not a merge to main.
- **Same-user limits remain (BRO-4059).** The keyring and Maestro's checkout are writable by any
  process running as the owner's OS user. Running Maestro under its own OS user closes this.
- The coordinator's evidence rule (`.broomva/loops/prompts/coordinator-tick.md` in the workspace)
  changes from "on main or `maestro/answers`" to "on main, or on `maestro/ledger` with a verifying
  passkey proof".
