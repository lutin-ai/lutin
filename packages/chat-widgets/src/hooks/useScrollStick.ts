import { useLayoutEffect, useRef, type RefObject } from "react";

// Treat anything within this many pixels of the bottom as "still at
// the bottom" so a wheel that lands a few pixels off doesn't break
// auto-stick. Larger than the prior 8px because Linux WebKit emits a
// lot of small wheel deltas that previously couldn't escape the snap.
const BOTTOM_SLACK = 32;

// Sticks a scroll container to the bottom whenever its content grows,
// but only if the user was already at (or very near) the bottom — so
// scrolling up to read history doesn't get yanked back down by new
// streamed tokens.
//
// Detection model: we remember the `scrollTop` we last left the
// container at (after our own snap, or after a no-op render). On the
// next render's layout effect, if `scrollTop` differs from that
// remembered value, the *user* moved it (browsers don't reposition
// scrollTop on a pure content append). We re-derive stuck-ness from
// the user's new position; subsequent renders honor that decision and
// stop snapping until the user scrolls back near the bottom.
//
// This replaces a passive-scroll-listener model that had a fatal race
// during streaming: the listener fired *after* the next render's
// snap, so a tiny wheel got obliterated before it could un-stick.
// Reading scrollTop directly inside the layout effect closes that gap.
export function useScrollStick(
  ref: RefObject<HTMLElement | null>,
  deps: ReadonlyArray<unknown>,
) {
  // -1 sentinel = "haven't run yet" (avoids treating the first observed
  // scrollTop as a user-driven divergence).
  const lastSeenScrollTop = useRef(-1);
  const stuck = useRef(true);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;

    if (lastSeenScrollTop.current === -1) {
      // First-paint: snap to bottom, regardless of where the browser
      // initialized the scroll position.
      el.scrollTop = el.scrollHeight;
      lastSeenScrollTop.current = el.scrollTop;
      return;
    }

    // If scrollTop drifted from what we left it at, the user scrolled
    // (mouse wheel, scrollbar drag, keyboard). Update stuck-ness from
    // the new position. Browsers don't move scrollTop on a pure
    // content append, so any divergence here is user-driven.
    if (el.scrollTop !== lastSeenScrollTop.current) {
      const fromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      stuck.current = fromBottom <= BOTTOM_SLACK;
    }

    if (stuck.current) {
      el.scrollTop = el.scrollHeight;
    }
    lastSeenScrollTop.current = el.scrollTop;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
