---
name: brief
description: Summarize a Maestro workspace, project, or initiative and show what needs the owner.
---

# Maestro brief

1. Call `maestro_context` with the requested project, initiative, or current working directory. Use the narrowest scope the user gave.
2. Lead with the current state and the first supported next action. List real owner holds before other blockers. Read `maestro_ask_get` only for a decision whose details matter to the reply.
3. Report unread sources as unread. Preserve the tool's scope and any source errors. Distinguish a queued request, a decision, a running session, and a verified receipt.
4. Give the user the relevant decision or work reference. Leave approvals and dispatch to the owner's Maestro app.
