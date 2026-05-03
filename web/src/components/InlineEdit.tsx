import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { cn } from "@/lib/cn";

interface InlineEditProps {
  /** Current saved value. */
  value: string;
  /** Called with the new string when the user commits. The component
   *  ignores the call if the new value equals `value`. */
  onCommit: (next: string) => void | Promise<void>;
  placeholder?: string;
  /** Render override for the read mode. By default the value is shown
   *  in muted italics if empty, or as-is. */
  display?: (v: string) => React.ReactNode;
  className?: string;
  inputClassName?: string;
  /** Optional input type, e.g. for narrower text fields. Default text. */
  type?: "text";
  /** Quick validator. Returning a string blocks commit and shows the
   *  message via title=. Returning null = OK. */
  validate?: (v: string) => string | null;
}

/**
 * Click to edit, Enter / blur to commit, Esc to cancel. Optimistic UI
 * is the caller's job — this primitive just owns the local edit
 * buffer.
 */
export function InlineEdit({
  value,
  onCommit,
  placeholder,
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
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={onKey}
      placeholder={placeholder}
      title={error ?? undefined}
      className={cn(
        "rounded border border-border bg-background px-1.5 py-0.5 text-sm outline-none focus:ring-1 focus:ring-primary",
        error && "border-destructive focus:ring-destructive",
        inputClassName,
      )}
    />
  );
}
