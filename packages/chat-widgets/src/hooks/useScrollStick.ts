import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

// Within this many px of the bottom counts as "at the bottom" for the
// purposes of re-sticking. Larger than 8px because Linux WebKit emits
// many small wheel deltas that previously couldn't escape the snap.
const BOTTOM_SLACK = 32;

interface UseChatScrollResult {
  /** Scroll to an explicit offset and unstick. ChatView uses this to
   *  anchor the just-sent user message at the top after submit. */
  anchorAt: (topPx: number) => void;
  /** Snap to content bottom (ignoring any trailing reserve) and
   *  re-stick. Backs the "Jump to latest" pill. */
  scrollToBottom: () => void;
  /** Reactive: true when the view auto-follows new content, false
   *  while the user has scrolled away. Consumers can render an
   *  affordance (e.g. a jump-pill) while `stuck` is false. */
  stuck: boolean;
}

/** Smart-stick scroller for chat transcripts.
 *
 *  Starts stuck to the bottom; unsticks when the user scrolls up;
 *  re-sticks when they return to within `BOTTOM_SLACK` of the real
 *  content bottom. The optional `bottomReserve` is trailing space the
 *  caller adds for layout (e.g. a spacer that lets the latest user
 *  message anchor at the top) that should be ignored by both the
 *  near-bottom check and the snap target.
 *
 *  Detection model: we record `scrollTop` after every layout pass.
 *  Any divergence next pass is user-driven (browsers don't move
 *  `scrollTop` on a pure content append), so we re-derive stuck-ness
 *  from the user's new position. Reading inside the layout effect
 *  avoids a passive-listener race during streaming where a tiny wheel
 *  could be obliterated by the next snap.
 */
export function useScrollStick(
  ref: RefObject<HTMLElement | null>,
  deps: ReadonlyArray<unknown>,
  bottomReserve?: () => number,
): UseChatScrollResult {
  const lastSeenScrollTop = useRef(-1);
  // Mirror of the React state for synchronous use inside the layout
  // effect (state setters are async; the effect needs the live value
  // to know whether to snap on this very pass).
  const stuckRef = useRef(true);
  const [stuck, setStuck] = useState(true);

  const flipStuck = useCallback((next: boolean) => {
    if (stuckRef.current === next) return;
    stuckRef.current = next;
    setStuck(next);
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const reserve = bottomReserve ? bottomReserve() : 0;
    const contentBottom = el.scrollHeight - reserve - el.clientHeight;

    if (lastSeenScrollTop.current === -1) {
      el.scrollTop = Math.max(0, contentBottom);
      lastSeenScrollTop.current = el.scrollTop;
      return;
    }

    // `contentBottom <= 0` means real content fits inside the viewport
    // (the trailing reserve makes the container scrollable, but there's
    // no row position to snap to). Skip the snap so a user dragging
    // into the spacer below isn't yanked back to the top on the next
    // streaming tick.
    if (stuckRef.current && contentBottom > 0) {
      el.scrollTop = contentBottom;
    }
    lastSeenScrollTop.current = el.scrollTop;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  // User-input detection. We can't rely on comparing scrollTop across
  // layout passes during streaming: rapid token-driven snaps run
  // between wheel pulses, so the snap overwrites the wheel's scrollTop
  // before any check sees it and the upward intent is lost. Listening
  // for the input events themselves (wheel/touch/key) is unambiguous —
  // these only fire from the user.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0) flipStuck(false);
    };
    let touchY: number | null = null;
    const onTouchStart = (e: TouchEvent) => {
      touchY = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e: TouchEvent) => {
      if (touchY === null) return;
      const y = e.touches[0]?.clientY;
      if (y !== undefined && y > touchY) flipStuck(false);
      touchY = y ?? touchY;
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "PageUp" || e.key === "ArrowUp" || e.key === "Home") {
        flipStuck(false);
      }
    };
    // Re-stick when the user (or the scrollbar drag) brings the view
    // back near the bottom. Also covers the idle case where no content
    // is streaming so the layout effect wouldn't re-run.
    const onScroll = () => {
      if (lastSeenScrollTop.current === -1) return;
      if (el.scrollTop === lastSeenScrollTop.current) return;
      lastSeenScrollTop.current = el.scrollTop;
      if (stuckRef.current) return;
      const reserve = bottomReserve ? bottomReserve() : 0;
      const contentBottom = el.scrollHeight - reserve - el.clientHeight;
      // Nothing to re-stick to while content fits inside the viewport —
      // `fromBottom` would be negative and trip the BOTTOM_SLACK check
      // even when the user is at the top of a short transcript.
      if (contentBottom <= 0) return;
      const fromBottom = contentBottom - el.scrollTop;
      if (fromBottom <= BOTTOM_SLACK) flipStuck(true);
    };

    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("keydown", onKeyDown);
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("keydown", onKeyDown);
      el.removeEventListener("scroll", onScroll);
    };
  }, [ref, bottomReserve, flipStuck]);

  const anchorAt = useCallback(
    (topPx: number) => {
      const el = ref.current;
      if (!el) return;
      el.scrollTop = Math.max(0, Math.min(topPx, el.scrollHeight - el.clientHeight));
      flipStuck(false);
      lastSeenScrollTop.current = el.scrollTop;
    },
    [ref, flipStuck],
  );

  const scrollToBottom = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const reserve = bottomReserve ? bottomReserve() : 0;
    el.scrollTop = Math.max(0, el.scrollHeight - reserve - el.clientHeight);
    flipStuck(true);
    lastSeenScrollTop.current = el.scrollTop;
  }, [ref, bottomReserve, flipStuck]);

  return { anchorAt, scrollToBottom, stuck };
}
