import {
  type KeyboardEvent as ReactKeyboardEvent,
  type RefObject,
  useEffect,
  useRef,
} from "react";

const focusableSelector =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

// Open dialogs, innermost last. Only the innermost answers Escape, and the
// page stays scroll-locked until the last of them closes.
const openDialogs: symbol[] = [];

/**
 * Modal focus handling: focus moves into the dialog, Tab stays inside it,
 * Escape closes the innermost dialog unless something inside already handled
 * that key, and focus returns to `returnFocus` or to what opened it, or to
 * `fallbackFocus` when that is no longer shown.
 */
export function useDialogFocus(
  dialog: RefObject<HTMLElement | null>,
  initial: RefObject<HTMLElement | null>,
  onClose: () => void,
  returnFocus?: RefObject<HTMLElement | null>,
  fallbackFocus?: RefObject<HTMLElement | null>,
) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const token = Symbol("dialog");
    openDialogs.push(token);
    const opener = returnFocus?.current ??
      (document.activeElement instanceof HTMLElement
        ? document.activeElement
        : undefined);
    const target = initial.current ?? dialog.current;
    target?.focus({ preventScroll: true });
    if (document.activeElement !== target) {
      dialog.current?.focus({ preventScroll: true });
    }
    document.body.classList.add("dialog-open");
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        event.key !== "Escape" || event.defaultPrevented ||
        openDialogs.at(-1) !== token
      ) return;
      event.preventDefault();
      close.current();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      openDialogs.splice(openDialogs.indexOf(token), 1);
      if (!openDialogs.length) document.body.classList.remove("dialog-open");
      const restore = opener?.isConnected ? opener : fallbackFocus?.current;
      restore?.focus({ preventScroll: true });
    };
  }, [dialog, initial, returnFocus, fallbackFocus]);
  return (event: ReactKeyboardEvent<HTMLElement>) => {
    if (event.key !== "Tab" || !dialog.current) return;
    const focusable = [
      ...dialog.current.querySelectorAll<HTMLElement>(focusableSelector),
    ].filter((element) =>
      element.getClientRects().length > 0 || element === document.activeElement
    );
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };
}
