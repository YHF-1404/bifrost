// Phase 3 — segment-locked IPv4 picker.
//
// The bridge IP fixes which octets of a client's TAP IP are editable
// vs. inherited from the bridge (B5). Examples:
//
//   bridge 10.0.0.1/24  → client picker shows  10.0.0.[__]/24   (octet 4)
//   bridge 10.0.0.1/16  → client picker shows  10.0.[__].[__]/16 (octets 3+4)
//   bridge 10.0.0.1     (no prefix) → all four octets editable
//
// Used both for the *bridge* IP itself (where octets 1–3 are editable
// and the prefix is a /16 /24 toggle) and for *client* TAP IPs (where
// the editable octets are constrained by the bridge's prefix).
//
// Server-side validation is the source of truth; this component just
// makes the legal shape obvious.

import { useEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/cn";

export type Prefix = 16 | 24;

interface IpSegmentInputProps {
  /** Current value as `"a.b.c.d/p"` or empty. */
  value: string;
  /** Called when the user commits (Enter / blur) AND the value parses. */
  onCommit: (next: string) => void;
  /** Bridge prefix that constrains the editable octets. Pass `null`
   *  to leave all four octets free (used for the bridge IP itself
   *  when there is no upstream constraint). */
  bridgePrefix: Prefix | null;
  /** When `bridgePrefix` is `null`, the user can choose between /16
   *  and /24 via a small selector. Defaults to `false` — clients
   *  inherit from the bridge and can't change prefix. */
  allowPrefixToggle?: boolean;
  /** Address bytes pinned by the bridge (used when bridgePrefix !== null
   *  to fill the locked octets). For `/24` only the first 3 are honored;
   *  for `/16` only the first 2. */
  pinFromBridge?: string | null;
  /** Display-mode placeholder when `value` is empty. */
  placeholder?: string;
  className?: string;
  /** If set, validate the user's input against this collision set
   *  before commit. Strings should be `"a.b.c.d/p"`; the comparison
   *  is on the address part only. */
  collisions?: string[];
}

function parseCidr(v: string): { octets: number[]; prefix: number } | null {
  if (!v) return null;
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\/(\d{1,2})$/.exec(v.trim());
  if (!m) return null;
  const o = [m[1], m[2], m[3], m[4]].map((s) => Number.parseInt(s, 10));
  const p = Number.parseInt(m[5]!, 10);
  if (o.some((n) => Number.isNaN(n) || n < 0 || n > 255)) return null;
  if (Number.isNaN(p) || p < 0 || p > 32) return null;
  return { octets: o, prefix: p };
}

function format(octets: number[], prefix: number): string {
  return `${octets.join(".")}/${prefix}`;
}

export function IpSegmentInput({
  value,
  onCommit,
  bridgePrefix,
  allowPrefixToggle = false,
  pinFromBridge,
  placeholder = "click to set",
  className,
  collisions,
}: IpSegmentInputProps) {
  // Editable / locked octets are derived once from bridgePrefix.
  const lockedCount = useMemo(() => {
    if (bridgePrefix === 24) return 3;
    if (bridgePrefix === 16) return 2;
    return 0; // no constraint — all 4 editable
  }, [bridgePrefix]);

  const initial = useMemo(() => {
    const parsed = parseCidr(value);
    if (parsed) return parsed;
    // Fresh row: pre-fill the locked octets from the bridge so the
    // user only needs to type the editable ones.
    const fromBridge = pinFromBridge ? parseCidr(pinFromBridge) : null;
    const seedOctets =
      fromBridge?.octets ?? [0, 0, 0, 0];
    return {
      octets: seedOctets,
      prefix: bridgePrefix ?? fromBridge?.prefix ?? 24,
    };
  }, [value, pinFromBridge, bridgePrefix]);

  const [octets, setOctets] = useState<string[]>(
    initial.octets.map((n) => String(n)),
  );
  const [prefix, setPrefix] = useState<number>(initial.prefix);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const inputRefs = useRef<Array<HTMLInputElement | null>>([null, null, null, null]);

  // Re-sync from props when the saved `value` changes externally (e.g.
  // after a server update from another tab).
  useEffect(() => {
    if (editing) return;
    const parsed = parseCidr(value);
    if (parsed) {
      setOctets(parsed.octets.map((n) => String(n)));
      setPrefix(parsed.prefix);
    } else if (!value) {
      // Empty value — reset to the seed so the user starts from the
      // bridge's pre-filled locked octets.
      const fromBridge = pinFromBridge ? parseCidr(pinFromBridge) : null;
      setOctets((fromBridge?.octets ?? [0, 0, 0, 0]).map((n) => String(n)));
      setPrefix(bridgePrefix ?? fromBridge?.prefix ?? 24);
    }
  }, [value, editing, pinFromBridge, bridgePrefix]);

  if (!editing) {
    const parsed = parseCidr(value);
    return (
      <button
        type="button"
        onClick={() => setEditing(true)}
        className={cn(
          "rounded px-1 py-0.5 text-left font-mono text-sm hover:bg-accent",
          className,
        )}
      >
        {parsed ? value : <span className="text-muted-foreground italic">{placeholder}</span>}
      </button>
    );
  }

  const commit = () => {
    const nums = octets.map((s) => Number.parseInt(s, 10));
    if (nums.some((n) => Number.isNaN(n) || n < 0 || n > 255)) {
      setError("each octet must be 0–255");
      return;
    }
    // Bridge IP collision (the gateway address is reserved).
    if (pinFromBridge) {
      const br = parseCidr(pinFromBridge);
      if (br && br.octets.join(".") === nums.join(".")) {
        setError("address conflicts with the bridge IP");
        return;
      }
    }
    if (collisions?.some((c) => parseCidr(c)?.octets.join(".") === nums.join("."))) {
      setError("address already used by another client");
      return;
    }
    const cidr = format(nums, prefix);
    setError(null);
    setEditing(false);
    onCommit(cidr);
  };

  const cancel = () => {
    setError(null);
    setEditing(false);
    const parsed = parseCidr(value);
    if (parsed) {
      setOctets(parsed.octets.map((n) => String(n)));
      setPrefix(parsed.prefix);
    }
  };

  return (
    <span
      className={cn(
        "inline-flex items-center gap-0.5 rounded border bg-background px-1 py-0.5 font-mono text-sm",
        error ? "border-destructive" : "border-border",
        className,
      )}
      title={error ?? undefined}
    >
      {octets.map((octet, i) => {
        const locked = i < lockedCount;
        return (
          <span key={i} className="inline-flex items-center">
            {i > 0 && <span className="px-0.5 text-muted-foreground">.</span>}
            {locked ? (
              <span className="px-0.5 text-muted-foreground">{octet}</span>
            ) : (
              <input
                ref={(el) => (inputRefs.current[i] = el)}
                value={octet}
                autoFocus={i === lockedCount}
                inputMode="numeric"
                onChange={(e) => {
                  const nx = [...octets];
                  // Strip non-digits.
                  nx[i] = e.target.value.replace(/[^\d]/g, "").slice(0, 3);
                  setOctets(nx);
                  if (error) setError(null);
                  // Auto-advance after 3 digits or '.'.
                  if (
                    nx[i]!.length === 3 &&
                    i < 3 &&
                    inputRefs.current[i + 1]
                  ) {
                    inputRefs.current[i + 1]!.focus();
                    inputRefs.current[i + 1]!.select();
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === ".") {
                    e.preventDefault();
                    if (i < 3 && inputRefs.current[i + 1]) {
                      inputRefs.current[i + 1]!.focus();
                      inputRefs.current[i + 1]!.select();
                    }
                  } else if (e.key === "Enter") {
                    e.preventDefault();
                    commit();
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    cancel();
                  } else if (
                    e.key === "Backspace" &&
                    octets[i] === "" &&
                    i > 0 &&
                    inputRefs.current[i - 1]
                  ) {
                    e.preventDefault();
                    inputRefs.current[i - 1]!.focus();
                  }
                }}
                onBlur={(e) => {
                  // Commit only when focus leaves the whole widget,
                  // not when it moves between octets.
                  const nextEl = e.relatedTarget as HTMLElement | null;
                  if (
                    nextEl &&
                    inputRefs.current.some((r) => r === nextEl)
                  ) {
                    return;
                  }
                  commit();
                }}
                // w-12 (48 px) easily fits a 3-digit octet ("255") in
                // font-mono text-sm with comfortable padding so the
                // cursor + caret never sit right against the digit.
                className="w-12 bg-transparent px-1 text-center outline-none"
              />
            )}
          </span>
        );
      })}
      <span className="px-0.5 text-muted-foreground">/</span>
      {allowPrefixToggle ? (
        // Toggle button instead of <select>: a native dropdown loses
        // focus the moment the menu opens, which would commit the
        // edit before the user could pick the new value. With a
        // button, a single click flips between /16 and /24.
        <button
          type="button"
          onMouseDown={(e) => e.preventDefault() /* keep edit focused */}
          onClick={() => setPrefix((p) => (p === 24 ? 16 : 24))}
          title={`prefix /${prefix} — click to switch (only /16 or /24)`}
          className="cursor-pointer bg-transparent px-0.5 hover:underline"
        >
          {prefix}
        </button>
      ) : (
        <span className="text-muted-foreground">{prefix}</span>
      )}
      {/* Confirm button so the user can commit on click without
          fighting the focus-leaves-the-row blur logic. */}
      {allowPrefixToggle && (
        <button
          type="button"
          onMouseDown={(e) => e.preventDefault()}
          onClick={commit}
          title="apply"
          className="ml-1 rounded bg-primary px-1.5 text-[10px] text-primary-foreground hover:bg-primary/90"
        >
          ok
        </button>
      )}
    </span>
  );
}
