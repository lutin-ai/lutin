// Out-of-band channel for pushing text into the composer (e.g. PTT
// transcription) without lifting `draft` to the workflow's root. Keeps
// per-keystroke renders contained inside the Composer subtree — lifting
// would force the whole transcript to re-render on every character.
//
// The Composer subscribes via `addEventListener` on mount and unsubs on
// unmount; the workflow dispatches via `appendComposerText`.

export const COMPOSER_APPEND_EVENT = "lutin:composer-append";

export interface ComposerAppendDetail {
  text: string;
}

export function appendComposerText(text: string): void {
  if (typeof window === "undefined" || !text) return;
  window.dispatchEvent(
    new CustomEvent<ComposerAppendDetail>(COMPOSER_APPEND_EVENT, {
      detail: { text },
    }),
  );
}
