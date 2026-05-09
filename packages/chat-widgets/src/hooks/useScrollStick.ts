import { useLayoutEffect, useRef, type RefObject } from "react";

const BOTTOM_SLACK = 8;

// Sticks a scroll container to the bottom whenever its content grows,
// but only if the user was already at (or very near) the bottom — so
// scrolling up to read history doesn't get yanked back down by new
// streamed tokens.
//
// Caller passes the scroll element's ref, lets the hook subscribe to
// scroll events on it, and re-sticks on any of `deps` changing.
// Virtualized lists should include the virtualizer's reported total
// size in `deps` so newly-measured rows still pin the viewport.
export function useScrollStick(
  ref: RefObject<HTMLElement | null>,
  deps: ReadonlyArray<unknown>,
) {
  const stuck = useRef(true);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = () => {
      const fromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      stuck.current = fromBottom <= BOTTOM_SLACK;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [ref]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (stuck.current) el.scrollTop = el.scrollHeight;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
