import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@lutin/chat-widgets/theme.css";
import { App } from "./App";
import { getLutin } from "./lutin";
import "./reviewed-chat.css";

const root = createRoot(document.getElementById("root")!);

getLutin().then((lutin) => {
  root.render(
    <StrictMode>
      <App lutin={lutin} />
    </StrictMode>,
  );
});
