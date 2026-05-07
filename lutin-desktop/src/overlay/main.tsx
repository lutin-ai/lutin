import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { OverlayApp } from "./OverlayApp";
import "./overlay.css";

createRoot(document.getElementById("overlay-root")!).render(
  <StrictMode>
    <OverlayApp />
  </StrictMode>,
);
