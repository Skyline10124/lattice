import { useState } from "react";

const palette = {
  "cream": [
    { step: 1, hex: "#fbfaf8" }, { step: 2, hex: "#f5f3ef" },
    { step: 3, hex: "#eae5dd" }, { step: 4, hex: "#dfd6c8" },
    { step: 5, hex: "#d1c6b4" }, { step: 6, hex: "#c0b29c" },
    { step: 7, hex: "#a9997f" }, { step: 8, hex: "#907d5f" },
    { step: 9, hex: "#836e4c" }, { step: 10, hex: "#705d3e" },
    { step: 11, hex: "#4e402a" }, { step: 12, hex: "#2f281b" },
  ],
  "amber": [
    { step: 1, hex: "#fff9f3" }, { step: 2, hex: "#fcf1e7" },
    { step: 3, hex: "#f9e2ca" }, { step: 4, hex: "#f7d0a6" },
    { step: 5, hex: "#f0bc85" }, { step: 6, hex: "#e4a65f" },
    { step: 7, hex: "#d38926" }, { step: 8, hex: "#b36e00" },
    { step: 9, hex: "#9f6200" }, { step: 10, hex: "#885300" },
    { step: 11, hex: "#5f3800" }, { step: 12, hex: "#3c2200" },
  ],
  "purple": [
    { step: 1, hex: "#fbf9ff" }, { step: 2, hex: "#f6f1fc" },
    { step: 3, hex: "#ebe1f8" }, { step: 4, hex: "#e0cff5" },
    { step: 5, hex: "#d2bcee" }, { step: 6, hex: "#c1a6e3" },
    { step: 7, hex: "#ab8ad1" }, { step: 8, hex: "#926bbd" },
    { step: 9, hex: "#8658b4" }, { step: 10, hex: "#73489e" },
    { step: 11, hex: "#50316e" }, { step: 12, hex: "#301f42" },
  ],
  "signal-red": [
    { step: 1, hex: "#fff8f8" }, { step: 2, hex: "#ffefee" },
    { step: 3, hex: "#ffddda" }, { step: 4, hex: "#ffc8c3" },
    { step: 5, hex: "#ffb0a8" }, { step: 6, hex: "#f8958d" },
    { step: 7, hex: "#e8736c" }, { step: 8, hex: "#d34c49" },
    { step: 9, hex: "#cb2b31" }, { step: 10, hex: "#b21823" },
    { step: 11, hex: "#7c1117" }, { step: 12, hex: "#4b1111" },
  ],
  "cyan": [
    { step: 1, hex: "#f4fcfc" }, { step: 2, hex: "#e9f7f6" },
    { step: 3, hex: "#cfeded" }, { step: 4, hex: "#aee3e3" },
    { step: 5, hex: "#8ed7d6" }, { step: 6, hex: "#68c6c6" },
    { step: 7, hex: "#2ab0b0" }, { step: 8, hex: "#009393" },
    { step: 9, hex: "#008282" }, { step: 10, hex: "#006f6f" },
    { step: 11, hex: "#004d4d" }, { step: 12, hex: "#003030" },
  ],
  "navy": [
    { step: 1, hex: "#f8fafe" }, { step: 2, hex: "#f0f4fa" },
    { step: 3, hex: "#dee7f5" }, { step: 4, hex: "#cad8f0" },
    { step: 5, hex: "#b6c8e7" }, { step: 6, hex: "#9fb5da" },
    { step: 7, hex: "#829cc8" }, { step: 8, hex: "#6381b2" },
    { step: 9, hex: "#5072a9" }, { step: 10, hex: "#416093" },
    { step: 11, hex: "#2c4267" }, { step: 12, hex: "#1c293e" },
  ],
  "iris": [
    { step: 1, hex: "#f9faff" }, { step: 2, hex: "#f0f3fe" },
    { step: 3, hex: "#dfe5fe" }, { step: 4, hex: "#ccd5ff" },
    { step: 5, hex: "#b8c4fa" }, { step: 6, hex: "#a2b0f2" },
    { step: 7, hex: "#8695e3" }, { step: 8, hex: "#6977d1" },
    { step: 9, hex: "#5965ca" }, { step: 10, hex: "#4a54b2" },
    { step: 11, hex: "#333a7c" }, { step: 12, hex: "#1f254b" },
  ],
  "warm-gray": [
    { step: 1, hex: "#fafaf9" }, { step: 2, hex: "#f4f3f2" },
    { step: 3, hex: "#e8e6e4" }, { step: 4, hex: "#dad7d3" },
    { step: 5, hex: "#cac6c2" }, { step: 6, hex: "#b8b3ae" },
    { step: 7, hex: "#a09a94" }, { step: 8, hex: "#857f78" },
    { step: 9, hex: "#777068" }, { step: 10, hex: "#665f58" },
    { step: 11, hex: "#46413c" }, { step: 12, hex: "#2b2825" },
  ],
};

const semanticMap = {
  "cream": "aged surface / paper / tape label",
  "amber": "warning / caution",
  "purple": "auxiliary / decorative",
  "signal-red": "VHS red stripe / error / CRT red",
  "cyan": "CRT phosphor / tech highlight / info",
  "navy": "deep space / terminal bg",
  "iris": "primary / link / interactive",
  "warm-gray": "panel / equipment / text / border",
};

const stepLabels = {
  1: "app-bg", 2: "subtle-bg", 3: "elem-bg", 4: "hover-bg",
  5: "active-bg", 6: "subtle-border", 7: "border", 8: "strong-border",
  9: "solid", 10: "solid-hover", 11: "lo-text", 12: "hi-text",
};

function lum(hex) {
  const r = parseInt(hex.slice(1, 3), 16) / 255;
  const g = parseInt(hex.slice(3, 5), 16) / 255;
  const b = parseInt(hex.slice(5, 7), 16) / 255;
  return 0.299 * r + 0.587 * g + 0.114 * b;
}

// CRT scanline stripe for decoration
function VHSStripe() {
  return (
    <div style={{ display: "flex", height: 3, gap: 0, marginBottom: 16, opacity: 0.7 }}>
      <div style={{ flex: 1, background: "#cb2b31" }} />
      <div style={{ flex: 1, background: "#d38926" }} />
      <div style={{ flex: 1, background: "#008282" }} />
    </div>
  );
}

export default function Palette() {
  const [copied, setCopied] = useState(null);
  const [mode, setMode] = useState("light"); // light or dark preview

  const copy = (hex, id) => {
    navigator.clipboard.writeText(hex);
    setCopied(id);
    setTimeout(() => setCopied(null), 1200);
  };

  const dark = mode === "dark";
  const bg = dark ? "#1c293e" : "#f5f3ef";
  const fg = dark ? "#f5f3ef" : "#2b2825";
  const muted = dark ? "#a09a94" : "#777068";
  const border = dark ? "#2c4267" : "#e8e6e4";

  return (
    <div style={{
      fontFamily: "'SF Mono', 'Cascadia Code', 'JetBrains Mono', 'Courier New', monospace",
      fontSize: 11, padding: "24px 16px", maxWidth: 960, margin: "0 auto",
      background: bg, color: fg, borderRadius: 8, transition: "all 0.3s",
      minHeight: "100vh",
    }}>
      <div style={{ marginBottom: 8, display: "flex", justifyContent: "space-between", alignItems: "baseline", flexWrap: "wrap", gap: 8 }}>
        <div>
          <div style={{
            fontSize: 13, fontWeight: 700, letterSpacing: "0.12em",
            textTransform: "uppercase", color: fg,
          }}>
            ◆ CASSETTE FUTURISM
          </div>
          <div style={{ fontSize: 10, color: muted, marginTop: 2, letterSpacing: "0.05em" }}>
            OKLCH · 8 hues × 12 steps · Lone Trail reference
          </div>
        </div>
        <div style={{ display: "flex", gap: 4 }}>
          {["light", "dark"].map(m => (
            <button key={m} onClick={() => setMode(m)} style={{
              fontSize: 10, padding: "3px 10px", border: `1px solid ${border}`,
              borderRadius: 3, cursor: "pointer", fontFamily: "inherit",
              letterSpacing: "0.08em", textTransform: "uppercase",
              background: mode === m ? fg : "transparent",
              color: mode === m ? bg : muted,
            }}>
              {m}
            </button>
          ))}
        </div>
      </div>

      <VHSStripe />

      {Object.entries(palette).map(([hue, scale]) => (
        <div key={hue} style={{ marginBottom: 14 }}>
          <div style={{ display: "flex", alignItems: "baseline", gap: 8, marginBottom: 5 }}>
            <span style={{ fontSize: 11, fontWeight: 700, color: fg, letterSpacing: "0.06em", textTransform: "uppercase" }}>
              {hue}
            </span>
            <span style={{ fontSize: 9, color: muted, letterSpacing: "0.03em" }}>
              {semanticMap[hue]}
            </span>
          </div>
          <div style={{ display: "flex", gap: 2 }}>
            {scale.map((swatch) => {
              const id = `${hue}-${swatch.step}`;
              const isDark = lum(swatch.hex) < 0.45;
              const textColor = isDark ? "rgba(255,255,255,0.8)" : "rgba(0,0,0,0.55)";
              return (
                <div
                  key={id}
                  onClick={() => copy(swatch.hex, id)}
                  title={`${stepLabels[swatch.step]}\n${swatch.hex}`}
                  style={{
                    flex: 1, aspectRatio: "1/1.25", minWidth: 0,
                    backgroundColor: swatch.hex, borderRadius: 2,
                    cursor: "pointer", display: "flex", flexDirection: "column",
                    alignItems: "center", justifyContent: "center",
                    transition: "transform 0.1s",
                    transform: copied === id ? "scale(0.9)" : "scale(1)",
                    position: "relative",
                    border: `1px solid ${isDark ? "rgba(255,255,255,0.06)" : "rgba(0,0,0,0.04)"}`,
                  }}
                >
                  <span style={{ fontSize: 8, fontWeight: 700, color: textColor }}>
                    {swatch.step}
                  </span>
                  {copied === id && (
                    <span style={{
                      position: "absolute", fontSize: 8, fontWeight: 700,
                      color: textColor, background: swatch.hex,
                      padding: "1px 4px", borderRadius: 2,
                    }}>✓</span>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      ))}

      <VHSStripe />

      {/* Combo preview */}
      <div style={{ marginTop: 8, marginBottom: 16 }}>
        <div style={{ fontSize: 10, fontWeight: 700, color: muted, letterSpacing: "0.1em", textTransform: "uppercase", marginBottom: 8 }}>
          COMBO PREVIEW
        </div>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          {/* Terminal card */}
          <div style={{
            flex: "1 1 200px", background: "#1c293e", borderRadius: 4,
            padding: 12, border: "1px solid #2c4267",
          }}>
            <div style={{ fontSize: 9, color: "#008282", letterSpacing: "0.1em", marginBottom: 4 }}>
              RHINE.TERM://SYS
            </div>
            <div style={{ fontSize: 11, color: "#f5f3ef", marginBottom: 6 }}>
              Project Horizon — Status <span style={{ color: "#d38926" }}>NOMINAL</span>
            </div>
            <div style={{ fontSize: 9, color: "#829cc8" }}>
              Payload telemetry active · T-00:42:17
            </div>
            <div style={{ display: "flex", gap: 4, marginTop: 8 }}>
              <span style={{
                fontSize: 9, padding: "2px 8px", background: "#cb2b31",
                color: "#fff", borderRadius: 2,
              }}>ABORT</span>
              <span style={{
                fontSize: 9, padding: "2px 8px", background: "#008282",
                color: "#fff", borderRadius: 2,
              }}>CONFIRM</span>
            </div>
          </div>
          {/* Light panel card */}
          <div style={{
            flex: "1 1 200px", background: "#f5f3ef", borderRadius: 4,
            padding: 12, border: "1px solid #e8e6e4",
          }}>
            <div style={{ fontSize: 9, color: "#9f6200", letterSpacing: "0.1em", marginBottom: 4 }}>
              RHINE LIFE · MEMO
            </div>
            <div style={{ fontSize: 11, color: "#2b2825", marginBottom: 6 }}>
              Kristen Wright — <span style={{ color: "#b36e00" }}>Clearance Revoked</span>
            </div>
            <div style={{ fontSize: 9, color: "#777068" }}>
              Site 359 operations suspended pending review
            </div>
            <div style={{ display: "flex", gap: 4, marginTop: 8 }}>
              <span style={{
                fontSize: 9, padding: "2px 8px", background: "#9f6200",
                color: "#fff", borderRadius: 2,
              }}>ARCHIVE</span>
              <span style={{
                fontSize: 9, padding: "2px 8px", background: "transparent",
                color: "#9f6200", borderRadius: 2, border: "1px solid #9f6200",
              }}>DISMISS</span>
            </div>
          </div>
        </div>
      </div>

      <div style={{ fontSize: 9, color: muted, lineHeight: 1.6 }}>
        <div style={{ fontWeight: 700, color: fg, marginBottom: 4, letterSpacing: "0.06em", textTransform: "uppercase" }}>
          Step semantics
        </div>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(130px, 1fr))", gap: "1px 14px" }}>
          {Object.entries(stepLabels).map(([s, label]) => (
            <div key={s}>
              <span style={{ color: fg, fontWeight: 600 }}>{s}</span>{" "}{label}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
