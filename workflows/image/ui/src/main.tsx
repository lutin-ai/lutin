import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { getLutin } from "./lutin";

const root = createRoot(document.getElementById("root")!);

// Wait for chrome's handshake before mounting so the App can render
// against a guaranteed-present `Lutin` object. Same pattern as
// workflows/chat/ui/src/main.tsx.
getLutin().then((lutin) => {
  root.render(
    <StrictMode>
      <App lutin={lutin} />
    </StrictMode>,
  );
});
