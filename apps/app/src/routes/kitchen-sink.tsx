import { Avatar, Button, IconButton, Input } from "@maestro/ui";
import { Paperclip, Plus, Search, Settings } from "lucide-react";
import type { ReactNode } from "react";
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

// A tiny inline avatar image so the src variant renders with no network.
const SAMPLE_IMAGE =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="oklch(0.6 0.12 260)"/><circle cx="20" cy="16" r="7" fill="white"/><rect x="8" y="26" width="24" height="12" rx="6" fill="white"/></svg>',
  );

export function KitchenSink() {
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
        </div>
      </div>
    </main>
  );
}
