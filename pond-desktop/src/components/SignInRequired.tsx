// Shown when a browser tab has no host credential: it can neither sign in nor pair phones
// until it is opened from the link `pond-server dashboard` prints on the Pond.

import { KeyRound } from "lucide-react";
import { useTranslation } from "react-i18next";

export function SignInRequired() {
  const { t } = useTranslation();
  return (
    <div style={styles.root} role="alert">
      <div style={styles.card}>
        <KeyRound size={40} aria-hidden />
        <h1 style={styles.title}>{t("signIn.title")}</h1>
        <p style={styles.text}>{t("signIn.instructions")}</p>
        <code style={styles.code}>pond-server dashboard</code>
        <p style={styles.text}>{t("signIn.restart")}</p>
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    width: "100%",
    height: "100%",
    background: "var(--color-bg)",
    fontFamily: "var(--font-body)",
  },
  card: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "12px",
    maxWidth: "420px",
    padding: "0 16px",
    textAlign: "center",
  },
  title: { margin: 0, fontSize: "20px" },
  text: { margin: 0, lineHeight: 1.5 },
  code: { fontFamily: "var(--font-mono, monospace)", fontSize: "14px" },
};
