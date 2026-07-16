import type { Intent, Kind, OrchState, WorkItem } from "@maestro/protocol";
import {
  Avatar,
  Button,
  Card,
  Composer,
  DotComet,
  IconButton,
  Input,
  StatusBadge,
  workStatusView,
} from "@maestro/ui";
import { Paperclip, Plus, Search, Settings } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Inspector } from "../components/board/inspector";
import { GateQueue } from "../components/gate/gate-queue";
import { ThemeToggle } from "../components/theme-toggle";

/**
 * /kitchen-sink — every M1 primitive in every variant and state (BUILD-PLAN §M1).
 * A developer surface: it renders the components from @maestro/ui so the build proves
 * the token utilities resolve, and the visual gate (hover frosts, never scales; light
 * and dark both correct) has one place to check. Not linked from the product chrome.
 */
function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-4 rounded-card border border-border bg-card p-5">
      <span className="bv-section-header">{title}</span>
      {children}
    </section>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <span className="text-muted-foreground text-xs">{label}</span>
      <div className="flex flex-wrap items-center gap-3">{children}</div>
    </div>
  );
}

const BUTTON_VARIANTS = ["primary", "secondary", "soft", "ghost"] as const;

// A work node's plain-voice badge, mapped from its OrchState (BRO-1757). Running work wears
// the tidepool DotComet via the StatusBadge `dot` slot; standing routines pulse; everything
// else is a static dot. One capsule, no hand-rolled duplicate (BRO-1762 folds the P20 nit).
function StateBadge({ state, kind }: { state: OrchState; kind?: Kind }) {
  const v = workStatusView(state, kind);
  return (
    <StatusBadge
      status={v.tone}
      pulse={v.pulse}
      dot={v.running ? <DotComet size={8} /> : undefined}
    >
      {v.label}
    </StatusBadge>
  );
}

// A tiny inline avatar image so the src variant renders with no network.
const SAMPLE_IMAGE =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="oklch(0.6 0.12 260)"/><circle cx="20" cy="16" r="7" fill="white"/><rect x="8" y="26" width="24" height="12" rx="6" fill="white"/></svg>',
  );

// Seed gate-queue items for the gallery (the interactive grace window is proven here — the live
// projection needs a store + SSE-emitted gate rows, so the component-level proof is decoupled).
const GATE_ITEMS: WorkItem[] = [
  {
    id: "g-deploy",
    state: "review",
    kind: "task",
    title: "Approve the deploy to production",
    gate: "human",
    path: "hawthorne/gate",
    updatedAt: "2026-07-14T00:00:00.000Z",
    gateId: "gate-1",
    run: "run/7c2f1a",
    look: {
      ran: "2h 14m unsupervised · judge passed · 14 tests",
      decided: ["transcripts persist on the Run record", "replay covered by 14 tests"],
      ask: "merge the branch; tonight's phase 2 builds on it",
    },
  },
  {
    id: "g-token",
    state: "blocked",
    kind: "task",
    title: "Waiting on an API token",
    gate: "human",
    path: "hawthorne/stuck",
    updatedAt: "2026-07-14T00:00:00.000Z",
    reason: "the deploy key expired",
  },
];

export function KitchenSink() {
  const [sent, setSent] = useState<{ id: number; text: string }[]>([]);
  const onSend = (text: string) => setSent((s) => [...s, { id: s.length, text }]);
  // The gate queue echoes each dispatched verb so the pw harness can assert the intent + grace timing.
  const [dispatched, setDispatched] = useState<string[]>([]);
  const onIntent = async (intent: Intent) => {
    const key = "gateId" in intent ? intent.gateId : "nodeId" in intent ? intent.nodeId : "";
    setDispatched((d) => [...d, `${intent.type}:${key}`]);
  };
  // A mount toggle so the harness can prove the unmount-FLUSH: unmounting mid-grace must still commit the
  // chosen verdict (gate.ts §PendingVerdict), not drop it — the live analogue is a session switch.
  const [gateMounted, setGateMounted] = useState(true);
  return (
    <main className="flex min-h-dvh flex-col bg-background text-foreground">
      <header className="flex h-[52px] shrink-0 items-center justify-between border-border border-b px-5">
        <span className="font-medium text-sm">kitchen sink</span>
        <ThemeToggle />
      </header>

      <div className="flex-1 overflow-y-auto px-6 py-10">
        <div className="mx-auto flex w-full max-w-[720px] flex-col gap-8">
          <div className="flex flex-col gap-2">
            <h1 className="text-h1">Primitives</h1>
            <p className="text-muted-foreground text-sm">
              Button, icon button, input, and avatar in every variant and state. Hover frosts blue
              or lightens one step; nothing scales. Toggle the theme to check light and dark.
            </p>
          </div>

          <Section title="Button">
            {BUTTON_VARIANTS.map((variant) => (
              <Row key={variant} label={variant}>
                <Button variant={variant} size="sm">
                  New mission
                </Button>
                <Button variant={variant} size="md">
                  New mission
                </Button>
                <Button variant={variant} size="lg">
                  New mission
                </Button>
                <Button variant={variant} disabled>
                  Disabled
                </Button>
              </Row>
            ))}
          </Section>

          <Section title="Icon button">
            <Row label="Ghost squares, 20px Lucide icon, hover frosts">
              <IconButton label="New mission">
                <Plus size={20} strokeWidth={2} />
              </IconButton>
              <IconButton label="Search">
                <Search size={20} strokeWidth={2} />
              </IconButton>
              <IconButton label="Attach">
                <Paperclip size={20} strokeWidth={2} />
              </IconButton>
              <IconButton label="Settings" disabled>
                <Settings size={20} strokeWidth={2} />
              </IconButton>
            </Row>
          </Section>

          <Section title="Input">
            <Row label="Default, filled, disabled: ai-blue ring on focus">
              <Input placeholder="Prompt" />
              <Input defaultValue="A running mission" />
              <Input placeholder="Disabled" disabled />
            </Row>
          </Section>

          <Section title="Avatar">
            <Row label="Initials, sizes, an accent, and an image">
              <Avatar name="Ana Diaz" />
              <Avatar name="Broomva" size={28} />
              <Avatar name="Needs You" size={40} color="var(--bv-blue-accent)" />
              <Avatar name="Ana Diaz" size={40} src={SAMPLE_IMAGE} />
            </Row>
          </Section>

          <Section title="Work states">
            <Row label="Plain voice, mapped from OrchState. The dot carries the color; Needs you is accent-blue, never red.">
              <StateBadge state="proposed" />
              <StateBadge state="running" />
              <StateBadge state="blocked" />
              <StateBadge state="review" />
              <StateBadge state="done" />
              <StateBadge state="triggered" kind="routine" />
            </Row>
            <Row label="The tidepool dot on its own, at the running size (15px)">
              <DotComet />
            </Row>
          </Section>

          <Section title="Card">
            <Row label="Matte content card. Never glass, never pill-radius; radius 0.75rem, whisper border.">
              <Card className="w-[240px]">
                <span className="font-medium text-sm">Draft the launch note</span>
                <StateBadge state="proposed" />
              </Card>
            </Row>
            <Row label="Interactive: hover lifts to a diffuse blue-tinted shadow. It never scales.">
              <Card interactive className="w-[240px]">
                <span className="font-medium text-sm">Review the migration plan</span>
                <StateBadge state="review" />
              </Card>
            </Row>
            <Row label="Running: the card stays matte and wears the Undertow; pair with a DotComet on the status row.">
              <Card running className="w-[240px]">
                <span className="font-medium text-sm">Porting the parser</span>
                <StateBadge state="running" />
              </Card>
            </Row>
          </Section>

          <Section title="Composer">
            <Row label="The one glass surface: rounded-28 capsule, frosted-blue halo, inner light line. Enter or send fires onSend (trimmed); empty never sends.">
              <div className="w-full max-w-[520px]">
                <Composer onSend={onSend} />
              </div>
            </Row>
            <Row label="With a leading attach button">
              <div className="w-full max-w-[520px]">
                <Composer
                  leading={
                    <IconButton label="Attach">
                      <Paperclip size={20} strokeWidth={2} />
                    </IconButton>
                  }
                  onSend={onSend}
                />
              </div>
            </Row>
            {sent.length > 0 && (
              <Row label="Sent (dogfood echo)">
                <ul className="flex flex-col gap-1" data-testid="composer-sent">
                  {sent.map((item) => (
                    <li key={item.id} className="text-muted-foreground text-sm">
                      {item.text}
                    </li>
                  ))}
                </ul>
              </Row>
            )}
          </Section>

          <Section title="Live signals">
            <Row label="Running: the tidepool dot (blue to ice weather, 3.2s)">
              <span className="bv-dot-live" data-testid="dot-live" />
              <span className="text-muted-foreground text-sm">Running</span>
            </Row>
            <Row label="Standing: the pulse dot (1s opacity breath)">
              <span
                data-testid="dot-pulse"
                className="bv-dot--pulse inline-block size-[15px] rounded-full bg-[var(--bv-info)]"
              />
              <span className="text-muted-foreground text-sm">Standing</span>
            </Row>
            <Row label="Running card: the Undertow halo (the card stays matte)">
              <div className="bv-undertow" data-testid="undertow">
                <span className="bv-undertow-orbit" data-testid="undertow-orbit" />
                <div className="w-[220px] rounded-card border border-border bg-card p-4">
                  <span className="text-sm">A running mission</span>
                </div>
              </div>
            </Row>
          </Section>

          <Section title="Gate queue">
            <Row label="Rung 2: the human looks, then acts with verbs. Click a card to see the look + verbs; Approve is reversible for a beat (grace), Needs you is accent-blue, never red.">
              <div className="flex w-full max-w-[520px] flex-col gap-2">
                <button
                  type="button"
                  data-testid="gate-mount-toggle"
                  className="self-start text-muted-foreground text-xs underline"
                  onClick={() => setGateMounted((m) => !m)}
                >
                  {gateMounted ? "unmount queue" : "remount queue"}
                </button>
                {gateMounted ? <GateQueue items={GATE_ITEMS} onIntent={onIntent} /> : null}
              </div>
            </Row>
            <Row label="Empty: nothing at your gate">
              <div className="w-full max-w-[520px]">
                <GateQueue items={[]} onIntent={onIntent} />
              </div>
            </Row>
            {dispatched.length > 0 && (
              <Row label="Dispatched (harness echo)">
                <ul className="flex flex-col gap-1" data-testid="gate-dispatched">
                  {dispatched.map((d) => (
                    <li key={d} className="text-muted-foreground text-sm">
                      {d}
                    </li>
                  ))}
                </ul>
              </Row>
            )}
          </Section>

          <Section title="Inspector (M5 verbs)">
            <Row label="Rung 3: verify, then decide. The gate verbs (approve / send back / block / escalate) dispatch real intents; approve is reversible for a beat. The same harness echo captures the intent below.">
              <div
                data-testid="inspector-harness"
                className="w-full max-w-[420px] rounded-card border border-border bg-card p-4"
              >
                <Inspector item={GATE_ITEMS[0]} onIntent={onIntent} />
              </div>
            </Row>
            <Row label="Blocked (Stuck): the only verb is Redispatch">
              <div
                data-testid="inspector-harness-blocked"
                className="w-full max-w-[420px] rounded-card border border-border bg-card p-4"
              >
                <Inspector item={GATE_ITEMS[1]} onIntent={onIntent} />
              </div>
            </Row>
          </Section>
        </div>
      </div>
    </main>
  );
}
