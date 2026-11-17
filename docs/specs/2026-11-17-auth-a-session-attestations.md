# auth-A: session attestations from maestrod

Status: proposed, 2026-11-17. Tracking: BRO-3273 (slice A), under arc BRO-3841.
Owner decisions: 2026-11-17 AskUserQuestion. "Build BRO-3273 properly" was then narrowed to "Slice auth-A first".
Accepted inputs, not reopened here: workspace `docs/adrs/2026-10-25-maestro-signing-contract.md` (Ed25519, explicit algorithm, contract version and certified key id; the private key lives only under the Maestro OS user) and `docs/adrs/2026-10-25-maestro-owner-custody.md` (stage isolated users and a TEST key before production).
Probe receipts: [2026-11-17-auth-a-probes/probes.md](2026-11-17-auth-a-probes/probes.md), cited as P1 to P9.
Reversal cost: high for the attestation wire format and the socket protocol; medium for the thread-binding mechanism, which is versioned; low for the CLI and MCP surface.

## Objective

Let a live agent session write to Maestro's flow ledger, and any other agent-write surface, under an identity that an agent running as the owner's user cannot mint. Today no live session can write at all (P1). The Stop gate also cannot tell which session it is gating (P2).

## Context

- Since BRO-3268, `verified_caller` refuses T3 and Paseo claims without a signed bridge credential, and Direct claims without a launch token. Only tests mint either. The bridge credential is HMAC, so anything that can verify one can also forge one; the signing ADR rejected that for exactly this reason.
- Agents run as the owner's macOS user. Whatever that user can read, an agent can read, and whatever it can write, an agent can forge. That includes T3's own state (P7). So no key, and no evidence the binding rests on, may live only in storage the owner's user can write.
- The kernel's process facts can't be forged by a non-root process. A socket's peer pid and uid come from the kernel, and so do a process's parent and start time (P6). An agent can't make its MCP server the child of another thread's harness.
- T3 records every tool call in the calling thread's timeline. The record carries the provider and the call id, and it is persisted while the call is still running (P5). But T3 doesn't record which process belongs to which thread (P4), and its database is writable by the owner's user (P7).

## Goals

1. An agent running as the owner's user cannot produce a valid attestation. That means 0 of the forgery cases in the acceptance below, and the key file can't be read as that user.
2. A session in thread A cannot obtain an attestation for thread B by mix-up, by stealing a bearer token from a transcript, or as a confused deputy (persuading B's agent through a message).
3. The attestation names the provider and harness, so BRO-3828's different-provider routing can be enforced from it.
4. The Stop gate learns the session from the attestation, without `MAESTRO_SESSION`.
5. Nothing in phase 1 changes meaning: every existing flow and authority test keeps its behaviour.

## Non-goals

- **Passkey owner tokens, the HTTPS owner page, and session-end records on the event stream.** Those are auth-B, after BRO-3272.
- **Defending against a deliberately malicious agent that forges T3's database during first binding.** That needs the T3 server to register its harness pids with maestrod (option 2 below). It is named as the follow-up, not solved here. See Drawbacks.
- **Defending against root, or against the owner's admin rights.** The custody ADR already scopes the OS user as a boundary only against a non-privileged agent.
- **Linux parity in this slice.** The design names the Linux equivalents (`SO_PEERCRED`, `/proc/<pid>/stat`). Live acceptance is on the Mac first; the VPS follows the custody ADR's staging.

## Design

### Parts

| Part | Runs as | Holds |
|---|---|---|
| `maestrod` (new `crates/auth`, binary from `crates/cli`) | `_maestro` (LaunchDaemon) | The Ed25519 private key (`/var/db/maestro/key/ed25519.sk`, mode 0400, owner `_maestro`), the revocation list and the challenge table |
| Socket | — | `/var/run/maestro/auth.sock`, directory `_maestro:staff 0750`, socket 0660, so only `staff` can connect |
| `maestro mcp` / `maestro` CLI | the owner (as today) | The certified public key (`/etc/maestro/ed25519.pub` plus its owner certification), and, in memory only, the attestation for its own session |

### The thread binding: peer process plus T3-witnessed challenge

When `maestro mcp` first serves a write verb, it binds its session in four steps:

1. **Connect and register.** It connects to the socket and sends `hello {claimed_session, cwd}`. maestrod reads the peer's pid, uid and start time from the kernel (`LOCAL_PEERPID`, `LOCAL_PEERCRED`) and walks the parent chain with `sysctl KERN_PROC_PID`. It requires `peer → harness → T3 server`. The T3 server pid must be the one that launchd started from `~/.t3/runtime/versions/*/t3`. At install, the owner pins its code-signing identity or path. maestrod records a challenge: a 128-bit nonce bound to `(peer pid, peer start time, harness pid, harness start time, claimed_session)`, expiring after 60 s. It returns the nonce to the peer, never to the agent.
2. **Surface the nonce through a tool call.** `maestro mcp` answers the agent's write call with `attest_required {nonce}`. The agent must then call the tool `maestro_session_attest {nonce}`. The driver skills and the MCP tool description say so, and the call costs one extra tool call per session per hour.
3. **Confirm through the same process.** `maestro_session_attest` arrives on the same `maestro mcp` process, and it forwards `confirm {nonce}` on its existing socket. maestrod confirms three things:
   - the confirm comes from the same peer pid and start time that registered the challenge, so a nonce relayed to another thread's process is refused;
   - T3's timeline holds exactly one provider-sourced item (id prefix `turn-item:provider:`) for the tool `maestro_session_attest` whose input contains the nonce, created after the challenge was issued. maestrod reads this from T3's state through a read-only ACL the owner grants `_maestro` on `~/.t3/userdata` (P8). An agent's message to another thread is a user-message item, not a provider item, so it cannot satisfy this;
   - that item belongs to `claimed_session`'s thread.
   The provider comes from the item id (`provider:<instance>`), not from the agent.
4. **Issue the attestation.** maestrod signs `{v: 1, alg: "ed25519", kid, session, node, provider, harness: {pid, start}, peer: {pid, start}, iat, exp ≤ iat + 3600}` over identity canonical bytes. It returns the attestation to the peer only.
   - `maestro mcp` keeps the attestation in memory, never in a tool result. A transcript therefore never carries a bearer token.
   - Every verification checks the signature, the expiry and the revocation list, and that the verifier's own pid and start time equal `peer`. A copied attestation is therefore useless in another process.
   - The harness binding survives a renewal. After one hour, the same harness re-attests with a fresh challenge, with no T3 check if `(harness pid, start)` is unchanged and unrevoked.

The CLI follows the same path through `maestro attest`. A Direct session, started by a launcher, keeps launch tokens. maestrod becomes their registry, which closes the conformance runner's throwaway registry.

### Stop gate

`maestro flow stop-gate` runs as a hook child of the harness. It connects to the socket. maestrod derives the harness from the peer's parent chain and returns that harness's attested session, if there is one. With no attestation, the session never wrote a flow, so there is nothing to gate. This replaces `MAESTRO_SESSION`, which stays as an override for Direct sessions only.

### Revocation and renewal

- maestrod keeps a revocation list keyed by `(harness pid, start)` and by session.
- `maestro session end` (the agent's own close) and `maestrod revoke` (the owner, through the CLI as the owner's uid over the socket) add to it.
- A dead harness pid revokes itself.
- Attestations last at most one hour. Exactly 3600 s is accepted; 3601 s, zero and negative lifetimes are refused. This follows the identity crate's contract.

### Replacing the HMAC bridge credential

- `BridgeCredential` (HMAC) stays verifiable only under `#[cfg(test)]` and the test-support feature, so existing identity tests keep their meaning.
- Production verification accepts only Ed25519 attestations whose key id is certified.
- `issue_bridge_credential` leaves the production API.
- `MAESTRO_BRIDGE_SECRET` is refused at startup outside tests, with a message naming this spec.

### Owner steps (staged, per the custody ADR)

1. **TEST stage.** Create a `_maestrotest` user, then run `sudo maestro auth install --stage test`. That writes the LaunchDaemon plist, the socket directory, the ACL on `~/.t3/userdata` (read only) and a TEST key.
   - Certify the public key: the owner signs its fingerprint in the asks ledger answer.
   - Run the acceptance suite against the TEST daemon.
2. **Production.** Run the same command with `--stage prod` and a fresh key, after the TEST receipts pass. Quarterly rotation keeps the previous key verifiable for outstanding attestation lifetimes (at most one hour).

## Alternatives considered

| Alternative | Why not | What we keep |
|---|---|---|
| **Same-user issuer**: an HMAC key in the owner's user, checked against T3 state | Any agent can read the key and mint for any thread. The owner rejected this on 2026-11-17 | The T3 timeline as evidence |
| **T3 timeline nonce alone**, with no peer process binding | T3's database is owner-writable (P7), and the attestation would be a bearer token in the transcript, readable from T3 state by any agent | The provider-sourced item rule |
| **Peer process ancestry plus a T3-recorded harness pid** (T3 tells maestrod "pid H is thread X" at spawn) | The strongest binding, with no owner-writable evidence anywhere. But it needs a T3 server change, and T3 records no pids today (P4) | Named as the follow-up that closes the residual risk. The wire format already carries `harness {pid, start}`, so adding it is a binding-version change, not a format change |
| **Launch token minted at spawn into the harness environment** | Any process of the owner's uid can read the environment of that uid's processes through `ps eww`, so the token leaks to every agent | Kept for Direct sessions only, where Maestro's launcher is the spawner |
| **Harness call id (`tool_use_id`) matched against T3's running item** | Neither harness provably passes the call id to MCP servers (P9) | Revisit if a harness documents it |

## Acceptance

Commands that must pass, with each guard proved by a mutation that fails its test:

```bash
bash scripts/ci.sh && cargo test -p maestro-auth --test forgery
bash scripts/mutation-check.sh   # anchors for every guard below
```

`crates/auth/tests/forgery.rs`, with **0 of 10 succeeding**:

1. A forged signature.
2. A wrong key id.
3. An expired attestation, including exactly-1 h accepted, 1 h + 1 s refused, and zero or negative refused.
4. A replayed nonce.
5. A confirm from a different peer pid.
6. A confirm whose T3 item is a user message, not a provider item.
7. A T3 item in another thread.
8. A wrong node.
9. A revoked harness.
10. An attestation copied into another process.

Mutants that remove each of these guards must be killed: the signature check, nonce consumption, peer equality, provider-item filter, thread equality, node binding, apply-time expiry and the revocation check.

Live on the Mac (TEST stage), each kept as a receipt under `docs/conformance/`:

- `cat /var/db/maestro/key/ed25519.sk` run as the agent's uid fails with `Permission denied`, and the agent has no sudo (`sudo -n true` fails).
- A live T3 thread calls `maestro_flow_start` and completes a node under an attestation. The flow log's `Claimed` event carries the attested provider.
- A second thread's attempt to confirm the first thread's nonce is refused.
- With an unfinished instance open, the Stop gate refuses the thread's final message without `MAESTRO_SESSION` set.

## Drawbacks and costs

- **Residual risk, stated plainly.** A deliberately malicious agent running as the owner's uid could write a forged provider item into T3's database during the 60 s challenge, in its own process's name, for another thread. This needs intent and knowledge of the schema; the peer binding stops mix-ups and relays, but not this. The fix is the T3-recorded harness pid (alternative 3). Until then, flows that gate money or publication should keep a human node.
- **One extra tool call per session per hour**, plus a daemon to install, monitor and back up.
- **A dependency on T3's schema** (`orchestration_v2_projection_turn_items`). That schema has no stability promise, so the reader is versioned and fails closed: no attestation, never a wrong one.
- **Owner privileged steps** are needed before anything is live. Without them the kernel stays unusable from T3 threads.

## Open questions

1. Does the owner prefer the T3 change (alternative 3) instead of the read ACL on `~/.t3/userdata`, if T3 can take it?
2. Should Codex sessions launched outside T3 (`codex exec`) use Direct tokens from a Maestro launcher? The recommended default is yes.
