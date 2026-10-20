import { useEffect, useId, useRef } from "react";

const FOCUSABLE = "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

export function Modal({ title, onClose, children, className, labelledBy, initialFocus }: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  className?: string;
  labelledBy?: string;
  initialFocus?: React.RefObject<HTMLElement | null> | undefined;
}) {
  const ref = useRef<HTMLElement>(null);
  const autoId = useId();
  const id = labelledBy ?? autoId;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const initialFocusRef = useRef(initialFocus);
  initialFocusRef.current = initialFocus;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    // Everything behind the modal goes inert, the modal itself stays interactive. The scrim is
    // rendered inside the app's own container (`.shell` under `#root`), so the elements to
    // disable are the scrim's parent's other children — inerting `#root`'s children would make
    // the whole shell, modal included, non-interactive.
    const scrim = el.parentElement;
    const container = scrim?.parentElement;
    const inerted: Element[] = [];
    if (scrim && container) {
      for (const sibling of Array.from(container.children)) {
        if (sibling !== scrim && !sibling.hasAttribute("inert")) {
          sibling.setAttribute("inert", "");
          inerted.push(sibling);
        }
      }
    }

    // Where focus was before the modal opened, so closing puts it back.
    const previouslyFocused = document.activeElement as HTMLElement | null;

    // Focus
    const focusTarget = initialFocusRef.current?.current ?? el.querySelector<HTMLElement>(FOCUSABLE);
    focusTarget?.focus();

    // Escape handler
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCloseRef.current();
      }
    };
    window.addEventListener("keydown", onKey, true);

    // Focus trap
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const focusable = Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((el) => !el.hasAttribute("disabled"));
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    el.addEventListener("keydown", onKeyDown);

    return () => {
      // Only undo what this modal set, so a modal opened over another one restores cleanly.
      for (const element of inerted) element.removeAttribute("inert");
      window.removeEventListener("keydown", onKey, true);
      el.removeEventListener("keydown", onKeyDown);

      // Restore focus
      previouslyFocused?.focus();
    };
  }, []);

  return (
    <div className="modal-scrim" onMouseDown={onClose}>
      <section
        ref={ref}
        className={`modal ${className ?? ""}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={id}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h3 id={id}>{title}</h3>
        {children}
      </section>
    </div>
  );
}
