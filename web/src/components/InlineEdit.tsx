import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { cn } from "@/lib/cn";
import { pushToast } from "@/lib/toast";

interface InlineEditProps {
  /** Current saved value. */
  value: string;
  /** Called with the new string when the user commits. The component
   *  ignores the call if the new value equals `value`. */
  onCommit: (next: string) => void | Promise<void>;
  /** Read-mode placeholder, shown in italic muted style when `value`
   *  is empty. Acts as a call-to-action ("click to set IP"). */
  placeholder?: string;
  /** Input-mode placeholder, shown inside the `<input>` once the user
   *  starts editing. Defaults to `placeholder`. Use this to give the
   *  user a concrete example of the expected format
   *  ("e.g. 10.0.0.5/24") that's distinct from the call-to-action. */
  examplePlaceholder?: string;
  /** Render override for the read mode. By default the value is shown
   *  in muted italics if empty, or as-is. */
  display?: (v: string) => React.ReactNode;
  className?: string;
  inputClassName?: string;
  /** Optional input type, e.g. for narrower text fields. Default text. */
  type?: "text";
  /** Quick validator. Returning a string blocks commit and surfaces
   *  the message via inline red border + a toast. Returning null = OK. */
  validate?: (v: string) => string | null;
}

/**
 * Click to edit, Enter / blur to commit, Esc to cancel. Optimistic UI
 * is the caller's job — this primitive just owns the local edit
 * buffer.
 *
 * On a failed validate(), the input keeps focus, picks up a red
 * border + tooltip, AND a toast pops with the same message — so the
 * user sees the error even if their attention has wandered off the
 * field (e.g. they tabbed away and lost the inline indicator).
 */
export function InlineEdit({
  value,
  onCommit,
  placeholder,
  examplePlaceholder,
  display,
  className,
  inputClassName,
  type = "text",
  validate,
}: InlineEditProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (editing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [editing]);

  // External value changes while not editing.
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  const commit = () => {
    const trimmed = draft;
    if (trimmed === value) {
      setEditing(false);
      setError(null);
      return;
    }
    if (validate) {
      const e = validate(trimmed);
      if (e) {
        setError(e);
        pushToast("error", e);
        // Keep focus so the user can correct the value without
        // re-clicking. blur() would have already fired before this if
        // they tabbed away — that's fine, the toast catches them.
        if (inputRef.current) inputRef.current.focus();
        return;
      }
    }
    setError(null);
    setEditing(false);
    void onCommit(trimmed);
  };

  const cancel = () => {
    setDraft(value);
    setError(null);
    setEditing(false);
  };

  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    }
  };

  if (!editing) {
    return (
      <button
        type="button"
        onClick={() => setEditing(true)}
        className={cn(
          "rounded px-1 py-0.5 text-left hover:bg-accent",
          className,
        )}
      >
        {display
          ? display(value)
          : value || (
              <span className="text-muted-foreground italic">
                {placeholder ?? "click to set"}
              </span>
            )}
      </button>
    );
  }

  return (
    <input
      ref={inputRef}
      type={type}
      value={draft}
      onChange={(e) => {
        setDraft(e.target.value);
        // Clear stale error as the user types — the next commit will
        // re-validate from scratch.
        if (error) setError(null);
      }}
      onBlur={commit}
      onKeyDown={onKey}
      placeholder={examplePlaceholder ?? placeholder}
      title={error ?? undefined}
      className={cn(
        "rounded border border-border bg-background px-1.5 py-0.5 text-sm outline-none placeholder:text-muted-foreground/60 focus:ring-1 focus:ring-primary",
        error && "border-destructive focus:ring-destructive",
        inputClassName,
      )}
    />
  );
}
