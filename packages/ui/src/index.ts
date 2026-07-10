// @maestro/ui — the component library (shadcn primitives restyled to Broomva, CVA variants).
// M1 primitives (BRO-1752): Button · IconButton · Input · Avatar.
// M1 primitives (BRO-1757): StatusBadge · DotComet + the plain-voice → chrome bridge.
// Prop names track the design-system contracts in
// handoff/design_handoff_maestro/build-docs/design-system/components/core/*.d.ts.
export { Avatar, type AvatarProps } from "./avatar";
export { Button, type ButtonProps, buttonVariants } from "./button";
export { DotComet, type DotCometProps } from "./dot-comet";
export { IconButton, type IconButtonProps } from "./icon-button";
export { Input, type InputProps } from "./input";
export { cn } from "./lib/cn";
export {
  STATUS_DOT_VAR,
  StatusBadge,
  type StatusBadgeProps,
  type StatusTone,
} from "./status-badge";
export { dotToTone, PLAIN_VOICE, type WorkStatusView, workStatusView } from "./work-status";
