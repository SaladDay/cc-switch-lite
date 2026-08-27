import { useEffect, useRef } from "react";

const FOCUSABLE = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "a[href]",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

interface ModalDialogOptions {
  busy: boolean;
  onCancel: () => void;
}

interface SiblingState {
  element: Element;
  ariaHidden: string | null;
  inert: string | null;
  hadFallbackClass: boolean;
}

function focusableElements(dialog: HTMLDialogElement): HTMLElement[] {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (element) =>
      !element.hidden && element.getAttribute("aria-hidden") !== "true",
  );
}

export function useModalDialog({ busy, onCancel }: ModalDialogOptions) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(
    typeof document !== "undefined" &&
      document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null,
  );
  const busyRef = useRef(busy);
  const cancelRef = useRef(onCancel);
  busyRef.current = busy;
  cancelRef.current = onCancel;

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (typeof dialog.showModal === "function") {
      dialog.showModal();
      return () => {
        if (dialog.open) dialog.close();
      };
    }

    const previousFocus = previousFocusRef.current;
    const parent = dialog.parentElement;
    const siblings: SiblingState[] = parent
      ? Array.from(parent.children)
          .filter((element) => element !== dialog)
          .map((element) => ({
            element,
            ariaHidden: element.getAttribute("aria-hidden"),
            inert: element.getAttribute("inert"),
            hadFallbackClass: element.classList.contains("lite-modal-inert"),
          }))
      : [];
    for (const { element } of siblings) {
      element.setAttribute("aria-hidden", "true");
      element.setAttribute("inert", "");
      element.classList.add("lite-modal-inert");
    }

    const backdrop = document.createElement("div");
    backdrop.className = "lite-dialog-backdrop";
    backdrop.setAttribute("aria-hidden", "true");
    parent?.insertBefore(backdrop, dialog);
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    dialog.setAttribute("open", "");

    const focusFirst = () => {
      const elements = focusableElements(dialog);
      (elements[0] ?? dialog).focus();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        if (!busyRef.current) cancelRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const elements = focusableElements(dialog);
      if (elements.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = elements[0];
      const last = elements[elements.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      } else if (!dialog.contains(document.activeElement)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      }
    };
    const onFocusIn = (event: FocusEvent) => {
      if (event.target instanceof Node && !dialog.contains(event.target)) {
        focusFirst();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn, true);
    const focusTimer = window.setTimeout(focusFirst, 0);

    return () => {
      window.clearTimeout(focusTimer);
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn, true);
      dialog.removeAttribute("open");
      backdrop.remove();
      document.body.style.overflow = previousOverflow;
      for (const state of siblings) {
        if (state.ariaHidden === null)
          state.element.removeAttribute("aria-hidden");
        else state.element.setAttribute("aria-hidden", state.ariaHidden);
        if (state.inert === null) state.element.removeAttribute("inert");
        else state.element.setAttribute("inert", state.inert);
        if (!state.hadFallbackClass)
          state.element.classList.remove("lite-modal-inert");
      }
      previousFocus?.focus();
    };
  }, []);

  return dialogRef;
}
