import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { getLutin } from "./lutin";

const root = createRoot(document.getElementById("root")!);

// Wait for chrome's handshake before mounting so the App can render
// against a guaranteed-present `Lutin` object instead of a Suspense
// boundary or sentinel value. The handshake is single-message and
// fires immediately after `iframe.onload`, so the wait is invisible
// in practice.
getLutin().then((lutin) => {
  root.render(
    <StrictMode>
      <App lutin={lutin} />
    </StrictMode>,
  );
});
