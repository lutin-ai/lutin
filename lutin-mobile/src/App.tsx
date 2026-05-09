export default function App() {
  return (
    <div
      style={{
        height: "100%",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: "var(--s-3)",
      }}
    >
      <div
        style={{
          fontSize: 18,
          fontWeight: 600,
          color: "var(--text-1-strong)",
        }}
      >
        Lutin
      </div>
      <div style={{ fontSize: 13, color: "var(--text-3)" }}>
        mobile shell · scaffold
      </div>
    </div>
  );
}
