// Settings controls (BRO-1893 FID-6 slice 3) — Switch / Stepper / Segmented / Slider, ported from
// ConceptSettings.jsx's Ds* projections but built accessible from scratch (no shadcn dep): the switch is
// role="switch", the segmented is a WAI-ARIA radiogroup (roving tabindex; arrows move selection AND
// focus), the slider is a native range input (already accessible). All matte, cool-axis, canon motion.

import { type KeyboardEvent, useRef } from "react";

/** A binary toggle — role="switch". */
export function SetSwitch({
  on,
  onToggle,
  label,
}: {
  on: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      className={`set-switch${on ? " is-on" : ""}`}
      onClick={onToggle}
    />
  );
}

/** A bounded integer stepper. */
export function SetStepper({
  value,
  set,
  label,
  min = 0,
  max = 99,
  suffix,
}: {
  value: number;
  set: (n: number) => void;
  label: string;
  min?: number;
  max?: number;
  suffix?: string;
}) {
  return (
    // Not role="group": each button carries the full context in its own aria-label, so a group wrapper
    // adds nothing (and a semantic <fieldset> would drag in form/legend styling this inline control does
    // not want).
    <div className="set-stepper">
      <button
        type="button"
        aria-label={`Decrease ${label}`}
        disabled={value <= min}
        onClick={() => set(Math.max(min, value - 1))}
      >
        −
      </button>
      <span className="set-stepper-val">
        {value}
        {suffix ? (
          <span style={{ fontWeight: 400, color: "var(--muted-foreground)" }}> {suffix}</span>
        ) : null}
      </span>
      <button
        type="button"
        aria-label={`Increase ${label}`}
        disabled={value >= max}
        onClick={() => set(Math.min(max, value + 1))}
      >
        +
      </button>
    </div>
  );
}

export type SegOption = readonly [value: string, label: string];

/** A segmented value picker — a WAI-ARIA radiogroup (arrows move selection AND focus, Home/End jump). */
export function SetSegmented({
  value,
  set,
  options,
  label,
}: {
  value: string;
  set: (v: string) => void;
  options: readonly SegOption[];
  label: string;
}) {
  const radios = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    const i = options.findIndex(([v]) => v === value);
    const last = options.length - 1;
    let n = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") n = i >= last ? 0 : i + 1;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") n = i <= 0 ? last : i - 1;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = last;
    const next = n < 0 ? undefined : options[n];
    if (!next) return;
    e.preventDefault();
    set(next[0]);
    radios.current[n]?.focus();
  };
  return (
    <div className="set-seg" role="radiogroup" aria-label={label}>
      {options.map(([v, lab], i) => (
        // biome-ignore lint/a11y/useSemanticElements: a native <input type="radio"> can't be styled as a segmented pill; role="radio" on a button + roving tabindex + arrow keys is the accessible radiogroup pattern (the Knowledge ViewToggle role="tab" precedent).
        <button
          key={v}
          ref={(el) => {
            radios.current[i] = el;
          }}
          type="button"
          role="radio"
          aria-checked={value === v}
          tabIndex={value === v ? 0 : -1}
          className={`set-seg-btn${value === v ? " is-active" : ""}`}
          onClick={() => set(v)}
          onKeyDown={onKey}
        >
          {lab}
        </button>
      ))}
    </div>
  );
}

/** A continuous slider — a native range input (accessible) + a formatted read-out. */
export function SetSlider({
  value,
  set,
  label,
  min,
  max,
  step = 1,
  fmt,
}: {
  value: number;
  set: (n: number) => void;
  label: string;
  min: number;
  max: number;
  step?: number;
  fmt: (v: number) => string;
}) {
  return (
    <div className="set-slider">
      <input
        className="set-range"
        type="range"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => set(Number(e.target.value))}
      />
      <span className="set-slider-val">{fmt(value)}</span>
    </div>
  );
}
